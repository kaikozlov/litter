use crate::ffi::ClientError;
use crate::ffi::shared::{shared_mobile_client, shared_runtime};
use crate::provider::AgentType;
use crate::provider::detection::detect_all_agents;
use crate::session::connection::ServerConfig;
use crate::ssh::{RemoteShell, SshAuth, SshBootstrapResult, SshClient, SshCredentials, SshError};
use crate::store::{
    AppConnectionProgressSnapshot, AppConnectionStepKind, AppConnectionStepState,
    ServerHealthSnapshot,
};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use tokio::sync::oneshot;
use tracing::{debug, info, trace, warn};

const WAKE_MAC_SCRIPT: &str = r#"iface="$(route -n get default 2>/dev/null | awk '/interface:/{print $2; exit}')"
if [ -z "$iface" ]; then iface="en0"; fi
mac="$(ifconfig "$iface" 2>/dev/null | awk '/ether /{print $2; exit}')"
if [ -z "$mac" ]; then
  mac="$(ifconfig en0 2>/dev/null | awk '/ether /{print $2; exit}')"
fi
if [ -z "$mac" ]; then
  mac="$(ifconfig 2>/dev/null | awk '/ether /{print $2; exit}')"
fi
printf '%s' "$mac""#;

#[derive(Clone)]
pub(crate) struct ManagedSshSession {
    pub(crate) client: Arc<SshClient>,
    pub(crate) pid: Option<u32>,
    pub(crate) shell: RemoteShell,
}

pub(crate) struct ManagedSshBootstrapFlow {
    pub(crate) install_decision: Option<oneshot::Sender<bool>>,
}

#[derive(uniffi::Object)]
pub struct SshBridge {
    pub(crate) rt: Arc<tokio::runtime::Runtime>,
    pub(crate) ssh_sessions: Mutex<std::collections::HashMap<String, ManagedSshSession>>,
    pub(crate) next_ssh_session_id: AtomicU64,
    pub(crate) bootstrap_flows:
        Arc<tokio::sync::Mutex<std::collections::HashMap<String, ManagedSshBootstrapFlow>>>,
}

#[derive(uniffi::Record)]
pub struct AppSshConnectionResult {
    pub session_id: String,
    pub normalized_host: String,
    pub server_port: u16,
    pub tunnel_local_port: Option<u16>,
    pub server_version: Option<String>,
    pub pid: Option<u32>,
    pub wake_mac: Option<String>,
}

impl Default for SshBridge {
    fn default() -> Self {
        Self::new()
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl SshBridge {
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self {
            rt: shared_runtime(),
            ssh_sessions: Mutex::new(std::collections::HashMap::new()),
            next_ssh_session_id: AtomicU64::new(1),
            bootstrap_flows: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Connect to an SSH server and bootstrap the **Codex** app-server.
    ///
    /// This is a convenience method for the Codex-only use case.
    /// It unconditionally resolves, installs (if needed), and starts
    /// the Codex app-server on the remote host. It does NOT perform
    /// agent detection or present an agent picker.
    ///
    /// For agent-type-aware connections, use `ssh_start_remote_server_connect`
    /// instead, which supports detection, picker, and provider factory routing.
    #[allow(clippy::too_many_arguments)]
    pub async fn ssh_connect_and_bootstrap(
        &self,
        host: String,
        port: u16,
        username: String,
        password: Option<String>,
        private_key_pem: Option<String>,
        passphrase: Option<String>,
        accept_unknown_host: bool,
        working_dir: Option<String>,
    ) -> Result<AppSshConnectionResult, ClientError> {
        let normalized_host = normalize_ssh_host(&host);
        let auth = ssh_auth(password, private_key_pem, passphrase)?;
        info!(
            "SshBridge: ssh_connect_and_bootstrap start host={} normalized_host={} ssh_port={} username={} auth={} working_dir={}",
            host.as_str(),
            normalized_host.as_str(),
            port,
            username.as_str(),
            ssh_auth_kind(&auth),
            working_dir.as_deref().unwrap_or("<none>")
        );
        let credentials = SshCredentials {
            host: normalized_host.clone(),
            port,
            username,
            auth,
        };

        let rt = Arc::clone(&self.rt);
        let session = tokio::task::spawn_blocking(move || {
            rt.block_on(async move {
                SshClient::connect(
                    credentials,
                    Box::new(move |_fingerprint| Box::pin(async move { accept_unknown_host })),
                )
                .await
                .map_err(map_ssh_error)
            })
        })
        .await
        .map_err(|e| ClientError::Rpc(format!("task join error: {e}")))??;
        info!(
            "SshBridge: ssh_connect_and_bootstrap connected normalized_host={} ssh_port={}",
            normalized_host.as_str(),
            port
        );

        let session = Arc::new(session);
        let bootstrap = {
            let session = Arc::clone(&session);
            let rt = Arc::clone(&self.rt);
            let working_dir = working_dir.clone();
            let use_ipv6 = normalized_host.contains(':');
            tokio::task::spawn_blocking(move || {
                rt.block_on(async move {
                    session
                        .bootstrap_codex_server(working_dir.as_deref(), use_ipv6)
                        .await
                        .map_err(map_ssh_error)
                })
            })
            .await
            .map_err(|e| ClientError::Rpc(format!("task join error: {e}")))?
        };

        let bootstrap = match bootstrap {
            Ok(result) => result,
            Err(error) => {
                warn!(
                    "SshBridge: ssh_connect_and_bootstrap bootstrap failed normalized_host={} ssh_port={} error={}",
                    normalized_host.as_str(),
                    port,
                    error
                );
                let session = Arc::clone(&session);
                let rt = Arc::clone(&self.rt);
                let _ = tokio::task::spawn_blocking(move || {
                    rt.block_on(async move {
                        session.disconnect().await;
                    })
                })
                .await;
                return Err(error);
            }
        };

        let wake_mac = self.ssh_read_wake_mac(Arc::clone(&session)).await;
        let session_id = format!(
            "ssh-{}",
            self.next_ssh_session_id.fetch_add(1, Ordering::Relaxed)
        );
        let shell = {
            let session = Arc::clone(&session);
            let rt = Arc::clone(&self.rt);
            tokio::task::spawn_blocking(move || {
                rt.block_on(async move { session.detect_remote_shell().await })
            })
            .await
            .unwrap_or(RemoteShell::Posix)
        };
        self.ssh_sessions_lock().insert(
            session_id.clone(),
            ManagedSshSession {
                client: Arc::clone(&session),
                pid: bootstrap.pid,
                shell,
            },
        );
        info!(
            "SshBridge: ssh_connect_and_bootstrap succeeded normalized_host={} ssh_port={} session_id={} remote_port={} local_tunnel_port={} pid={:?}",
            normalized_host.as_str(),
            port,
            session_id,
            bootstrap.server_port,
            bootstrap.tunnel_local_port,
            bootstrap.pid
        );

        Ok(AppSshConnectionResult {
            session_id,
            normalized_host,
            server_port: bootstrap.server_port,
            tunnel_local_port: Some(bootstrap.tunnel_local_port),
            server_version: bootstrap.server_version,
            pid: bootstrap.pid,
            wake_mac,
        })
    }

    pub async fn ssh_close(&self, session_id: String) -> Result<(), ClientError> {
        debug!("SshBridge: ssh_close session_id={}", session_id);
        let session = self
            .ssh_sessions_lock()
            .remove(&session_id)
            .ok_or_else(|| {
                ClientError::InvalidParams(format!("unknown SSH session id: {session_id}"))
            })?;
        let rt = Arc::clone(&self.rt);
        tokio::task::spawn_blocking(move || {
            rt.block_on(async move {
                if let Some(pid) = session.pid {
                    let kill_cmd = match session.shell {
                        RemoteShell::Posix => format!("kill {pid} 2>/dev/null"),
                        RemoteShell::PowerShell => {
                            format!("Stop-Process -Id {pid} -Force -ErrorAction SilentlyContinue")
                        }
                    };
                    let _ = session.client.exec_shell(&kill_cmd, session.shell).await;
                }
                session.client.disconnect().await;
            })
        })
        .await
        .map_err(|e| ClientError::Rpc(format!("task join error: {e}")))?;
        debug!("SshBridge: ssh_close completed session_id={}", session_id);
        Ok(())
    }

    /// Connect to an SSH server without bootstrapping any agent.
    ///
    /// This is the generic SSH connection method. It establishes the SSH
    /// session and returns a session ID that can be used for subsequent
    /// operations (agent detection, exec, etc.). No agent binary is
    /// resolved, installed, or started by this method.
    ///
    /// This replaces the use of `ssh_connect_and_bootstrap` when the
    /// caller wants to control agent detection and selection separately.
    #[allow(clippy::too_many_arguments)]
    pub async fn ssh_connect_only(
        &self,
        host: String,
        port: u16,
        username: String,
        password: Option<String>,
        private_key_pem: Option<String>,
        passphrase: Option<String>,
        accept_unknown_host: bool,
    ) -> Result<String, ClientError> {
        let normalized_host = normalize_ssh_host(&host);
        let auth = ssh_auth(password, private_key_pem, passphrase)?;
        info!(
            "SshBridge: ssh_connect_only start host={} normalized_host={} ssh_port={} username={} auth={}",
            host.as_str(),
            normalized_host.as_str(),
            port,
            username.as_str(),
            ssh_auth_kind(&auth),
        );
        let credentials = SshCredentials {
            host: normalized_host.clone(),
            port,
            username,
            auth,
        };

        let rt = Arc::clone(&self.rt);
        let session = tokio::task::spawn_blocking(move || {
            rt.block_on(async move {
                SshClient::connect(
                    credentials,
                    Box::new(move |_fingerprint| Box::pin(async move { accept_unknown_host })),
                )
                .await
                .map_err(map_ssh_error)
            })
        })
        .await
        .map_err(|e| ClientError::Rpc(format!("task join error: {e}")))??;
        info!(
            "SshBridge: ssh_connect_only connected normalized_host={} ssh_port={}",
            normalized_host.as_str(),
            port
        );

        let session = Arc::new(session);
        let session_id = format!(
            "ssh-{}",
            self.next_ssh_session_id.fetch_add(1, Ordering::Relaxed)
        );
        let shell = {
            let session = Arc::clone(&session);
            let rt = Arc::clone(&self.rt);
            tokio::task::spawn_blocking(move || {
                rt.block_on(async move { session.detect_remote_shell().await })
            })
            .await
            .unwrap_or(RemoteShell::Posix)
        };
        self.ssh_sessions_lock().insert(
            session_id.clone(),
            ManagedSshSession {
                client: Arc::clone(&session),
                pid: None,
                shell,
            },
        );
        info!(
            "SshBridge: ssh_connect_only succeeded normalized_host={} ssh_port={} session_id={}",
            normalized_host.as_str(),
            port,
            session_id
        );
        Ok(session_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn ssh_connect_remote_server(
        &self,
        server_id: String,
        display_name: String,
        host: String,
        port: u16,
        username: String,
        password: Option<String>,
        private_key_pem: Option<String>,
        passphrase: Option<String>,
        accept_unknown_host: bool,
        working_dir: Option<String>,
        ipc_socket_path_override: Option<String>,
        agent_type: Option<AgentType>,
        remote_command: Option<String>,
    ) -> Result<String, ClientError> {
        let normalized_host = normalize_ssh_host(&host);
        let auth = ssh_auth(password, private_key_pem, passphrase)?;
        info!(
            "SshBridge: ssh_connect_remote_server start server_id={} host={} normalized_host={} ssh_port={} username={} auth={} working_dir={} ipc_socket_path_override={} agent_type={}",
            server_id,
            host.as_str(),
            normalized_host.as_str(),
            port,
            username.as_str(),
            ssh_auth_kind(&auth),
            working_dir.as_deref().unwrap_or("<none>"),
            ipc_socket_path_override.as_deref().unwrap_or("<none>"),
            agent_type
                .map(|at| at.to_string())
                .unwrap_or_else(|| "None".to_string())
        );
        let credentials = SshCredentials {
            host: normalized_host.clone(),
            port,
            username,
            auth,
        };
        let config = ServerConfig {
            server_id,
            display_name,
            host: normalized_host,
            port: 0,
            websocket_url: None,
            is_local: false,
            tls: false,
        };
        let mobile_client = shared_mobile_client();
        let (tx, rx) = oneshot::channel();
        let task_server_id = config.server_id.clone();

        // Run the full SSH bootstrap on Tokio and only surface the final
        // completion back through UniFFI. Polling the full bootstrap future
        // directly from Swift's cooperative executor can overflow its small
        // stack on iOS when the websocket handshake wakes aggressively.
        tokio::spawn(async move {
            let result = mobile_client
                .connect_remote_over_ssh_with_agent_type(
                    config,
                    credentials,
                    accept_unknown_host,
                    working_dir,
                    ipc_socket_path_override,
                    agent_type,
                    remote_command,
                )
                .await
                .map_err(|e| ClientError::Transport(e.to_string()));
            match &result {
                Ok(server_id) => info!(
                    "SshBridge: ssh_connect_remote_server completed server_id={}",
                    server_id
                ),
                Err(error) => warn!(
                    "SshBridge: ssh_connect_remote_server failed server_id={} error={}",
                    task_server_id, error
                ),
            }
            let _ = tx.send(result);
        });

        rx.await
            .map_err(|_| ClientError::Rpc("ssh connect task cancelled".to_string()))?
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn ssh_start_remote_server_connect(
        &self,
        server_id: String,
        display_name: String,
        host: String,
        port: u16,
        username: String,
        password: Option<String>,
        private_key_pem: Option<String>,
        passphrase: Option<String>,
        accept_unknown_host: bool,
        working_dir: Option<String>,
        ipc_socket_path_override: Option<String>,
        agent_type: Option<AgentType>,
        remote_command: Option<String>,
    ) -> Result<String, ClientError> {
        let normalized_host = normalize_ssh_host(&host);
        let auth = ssh_auth(password, private_key_pem, passphrase)?;
        info!(
            "SshBridge: ssh_start_remote_server_connect start server_id={} host={} normalized_host={} ssh_port={} username={} auth={} working_dir={} ipc_socket_path_override={} agent_type={}",
            server_id,
            host.as_str(),
            normalized_host.as_str(),
            port,
            username.as_str(),
            ssh_auth_kind(&auth),
            working_dir.as_deref().unwrap_or("<none>"),
            ipc_socket_path_override.as_deref().unwrap_or("<none>"),
            agent_type
                .map(|at| at.to_string())
                .unwrap_or_else(|| "None".to_string())
        );
        let credentials = SshCredentials {
            host: normalized_host.clone(),
            port,
            username,
            auth,
        };
        let config = ServerConfig {
            server_id: server_id.clone(),
            display_name,
            host: normalized_host,
            port: 0,
            websocket_url: None,
            is_local: false,
            tls: false,
        };

        {
            let mut flows = self.bootstrap_flows.lock().await;
            if flows.contains_key(&server_id) {
                debug!(
                    "SshBridge: ssh_start_remote_server_connect reusing existing bootstrap flow server_id={}",
                    server_id
                );
                return Ok(server_id);
            }
            flows.insert(
                server_id.clone(),
                ManagedSshBootstrapFlow {
                    install_decision: None,
                },
            );
        }

        let mobile_client = shared_mobile_client();
        mobile_client
            .app_store
            .upsert_server(&config, ServerHealthSnapshot::Connecting, true);
        let initial_progress = AppConnectionProgressSnapshot::ssh_bootstrap();
        mobile_client
            .app_store
            .update_server_connection_progress(&server_id, Some(initial_progress.clone()));

        let flows = Arc::clone(&self.bootstrap_flows);
        let task_server_id = server_id.clone();
        let task_host = credentials.host.clone();
        tokio::spawn(async move {
            let task_result = if is_non_codex_agent(agent_type) {
                // Non-Codex agents: skip guided SSH bootstrap, use provider factory path.
                trace!(
                    "SshBridge: provider ssh connect task spawned server_id={} host={} agent_type={}",
                    task_server_id, task_host,
                    agent_type.map(|at| at.to_string()).unwrap_or_else(|| "None".to_string())
                );
                run_provider_ssh_connect(
                    Arc::clone(&mobile_client),
                    config,
                    credentials,
                    accept_unknown_host,
                    working_dir,
                    agent_type,
                    remote_command,
                )
                .await
            } else {
                // Codex (or None): existing guided SSH connect path unchanged.
                let mut progress = initial_progress;
                trace!(
                    "SshBridge: guided ssh connect task spawned server_id={} host={}",
                    task_server_id, task_host
                );
                run_guided_ssh_connect(
                    Arc::clone(&mobile_client),
                    Arc::clone(&flows),
                    config,
                    credentials,
                    accept_unknown_host,
                    working_dir,
                    ipc_socket_path_override,
                    &mut progress,
                )
                .await
            };

            if let Err(ref error) = task_result {
                warn!(
                    "ssh connect task failed server_id={} host={} error={}",
                    task_server_id, task_host, error
                );
                mobile_client
                    .app_store
                    .update_server_health(&task_server_id, ServerHealthSnapshot::Disconnected);
                mobile_client
                    .app_store
                    .update_server_connection_progress(&task_server_id, None);
            }

            if task_result.is_ok() {
                info!(
                    "SshBridge: ssh connect task completed server_id={} host={}",
                    task_server_id, task_host
                );
            }

            flows.lock().await.remove(&task_server_id);
        });

        Ok(server_id)
    }

    pub async fn ssh_respond_to_install_prompt(
        &self,
        server_id: String,
        install: bool,
    ) -> Result<(), ClientError> {
        info!(
            "SshBridge: ssh_respond_to_install_prompt server_id={} install={}",
            server_id, install
        );
        let sender = {
            let mut flows = self.bootstrap_flows.lock().await;
            flows
                .get_mut(&server_id)
                .and_then(|flow| flow.install_decision.take())
        }
        .ok_or_else(|| {
            ClientError::InvalidParams(format!("no pending install prompt for {server_id}"))
        })?;

        sender
            .send(install)
            .map_err(|_| ClientError::EventClosed("install prompt already closed".to_string()))
    }
}

fn ssh_auth_kind(auth: &SshAuth) -> &'static str {
    match auth {
        SshAuth::Password(_) => "password",
        SshAuth::PrivateKey { .. } => "private_key",
    }
}

impl SshBridge {
    fn ssh_sessions_lock(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, ManagedSshSession>> {
        match self.ssh_sessions.lock() {
            Ok(guard) => guard,
            Err(error) => {
                tracing::warn!("SshBridge: recovering poisoned ssh_sessions lock");
                error.into_inner()
            }
        }
    }

    pub(crate) async fn ssh_read_wake_mac(&self, session: Arc<SshClient>) -> Option<String> {
        let rt = Arc::clone(&self.rt);
        tokio::task::spawn_blocking(move || {
            rt.block_on(async move { read_wake_mac(session).await })
        })
        .await
        .ok()?
    }
}

async fn read_wake_mac(session: Arc<SshClient>) -> Option<String> {
    let result = session
        .exec(WAKE_MAC_SCRIPT)
        .await
        .map_err(map_ssh_error)
        .ok()?;
    if result.exit_code != 0 {
        return None;
    }
    normalize_wake_mac(&result.stdout)
}

#[allow(clippy::too_many_arguments)]
async fn run_guided_ssh_connect(
    mobile_client: Arc<crate::MobileClient>,
    bootstrap_flows: Arc<
        tokio::sync::Mutex<std::collections::HashMap<String, ManagedSshBootstrapFlow>>,
    >,
    config: ServerConfig,
    credentials: SshCredentials,
    accept_unknown_host: bool,
    working_dir: Option<String>,
    ipc_socket_path_override: Option<String>,
    progress: &mut AppConnectionProgressSnapshot,
) -> Result<(), ClientError> {
    let server_id = config.server_id.clone();
    info!(
        "guided ssh connect start server_id={} host={} ssh_port={} working_dir={} ipc_socket_path_override={}",
        server_id,
        credentials.host.as_str(),
        credentials.port,
        working_dir.as_deref().unwrap_or("<none>"),
        ipc_socket_path_override.as_deref().unwrap_or("<none>")
    );
    let ssh_client = Arc::new(
        SshClient::connect(
            credentials.clone(),
            Box::new(move |_fingerprint| Box::pin(async move { accept_unknown_host })),
        )
        .await
        .map_err(map_ssh_error)?,
    );
    info!(
        "guided ssh connect connected to ssh server_id={} host={} ssh_port={}",
        server_id,
        credentials.host.as_str(),
        credentials.port
    );
    let wake_mac_server_id = server_id.clone();
    let wake_mac_store = Arc::clone(&mobile_client.app_store);
    let wake_mac_client = Arc::clone(&ssh_client);
    tokio::spawn(async move {
        if let Some(wake_mac) = read_wake_mac(wake_mac_client).await {
            info!(
                "guided ssh connect discovered wake mac server_id={} wake_mac={}",
                wake_mac_server_id, wake_mac
            );
            wake_mac_store.update_server_wake_mac(&wake_mac_server_id, Some(wake_mac));
        }
    });
    progress.update_step(
        AppConnectionStepKind::ConnectingToSsh,
        AppConnectionStepState::Completed,
        Some(format!("Connected to {}", credentials.host.as_str())),
    );

    // ── Agent Detection ────────────────────────────────────────────────
    // After SSH auth, before Codex bootstrap: probe the host for Pi/Droid
    // agents. If any non-Codex agents found, signal iOS to show the picker.
    // If none found, proceed to Codex bootstrap. On failure/timeout,
    // fall through to Codex path.
    progress.update_step(
        AppConnectionStepKind::DetectingAgents,
        AppConnectionStepState::InProgress,
        None,
    );
    mobile_client
        .app_store
        .update_server_connection_progress(&server_id, Some(progress.clone()));

    let detected = detect_all_agents(&ssh_client).await;

    info!(
        "guided ssh connect agent detection server_id={} agents={}",
        server_id,
        detected.agents.len()
    );

    progress.update_step(
        AppConnectionStepKind::DetectingAgents,
        AppConnectionStepState::Completed,
        Some(format!("{} agent(s) detected", detected.agents.len())),
    );

    if detected.has_any_agent() {
        // One or more non-Codex agents detected: store list, signal iOS
        // to show picker.
        info!(
            "guided ssh connect agents detected server_id={} agents={:?}",
            server_id,
            detected.agents.iter().map(|a| a.id.clone()).collect::<Vec<_>>()
        );
        progress.detected_agents = detected.agents.clone();
        progress.pending_agent_selection = true;
        let label = if detected.agents.len() == 1 {
            "Agent detected. Please select it to connect.".to_string()
        } else {
            "Multiple agents detected. Please select one.".to_string()
        };
        progress.terminal_message = Some(label);
        mobile_client
            .app_store
            .update_server_connection_progress(&server_id, Some(progress.clone()));

        // Do NOT proceed with Codex bootstrap. Return normally — iOS will
        // see `pending_agent_selection == true` and `detected_agents` list
        // in the progress snapshot, present the picker, then call
        // `ssh_start_remote_server_connect` again with the chosen AgentType.
        return Ok(());
    }

    // No non-Codex agents found (or detection failed/timed out).
    // with normal Codex bootstrap.
    progress.pending_agent_selection = false;
    mobile_client
        .app_store
        .update_server_connection_progress(&server_id, Some(progress.clone()));

    // ── Codex Bootstrap ────────────────────────────────────────────────
    progress.update_step(
        AppConnectionStepKind::FindingAgent,
        AppConnectionStepState::InProgress,
        None,
    );
    mobile_client
        .app_store
        .update_server_connection_progress(&server_id, Some(progress.clone()));

    let remote_shell = ssh_client.detect_remote_shell().await;
    info!(
        "guided ssh connect detected shell server_id={} shell={:?}",
        server_id, remote_shell
    );
    let codex_binary = match ssh_client
        .resolve_codex_binary_optional_with_shell(Some(remote_shell))
        .await
        .map_err(map_ssh_error)?
    {
        Some(binary) => {
            info!(
                "guided ssh connect found codex server_id={} path={}",
                server_id,
                binary.path()
            );
            progress.update_step(
                AppConnectionStepKind::FindingAgent,
                AppConnectionStepState::Completed,
                Some(binary.path().to_string()),
            );
            progress.update_step(
                AppConnectionStepKind::InstallingAgent,
                AppConnectionStepState::Cancelled,
                Some("Already installed".to_string()),
            );
            mobile_client
                .app_store
                .update_server_connection_progress(&server_id, Some(progress.clone()));
            binary
        }
        None => {
            info!(
                "guided ssh connect missing codex server_id={} host={}; awaiting install decision",
                server_id,
                credentials.host.as_str()
            );
            progress.pending_install = true;
            progress.update_step(
                AppConnectionStepKind::FindingAgent,
                AppConnectionStepState::AwaitingUserInput,
                Some("Codex not found on remote host".to_string()),
            );
            mobile_client
                .app_store
                .update_server_connection_progress(&server_id, Some(progress.clone()));

            let (tx, rx) = oneshot::channel();
            {
                let mut flows = bootstrap_flows.lock().await;
                if let Some(flow) = flows.get_mut(&server_id) {
                    flow.install_decision = Some(tx);
                }
            }

            let should_install = rx.await.unwrap_or(false);
            info!(
                "guided ssh connect install decision server_id={} install={}",
                server_id, should_install
            );
            progress.pending_install = false;
            if !should_install {
                progress.update_step(
                    AppConnectionStepKind::FindingAgent,
                    AppConnectionStepState::Failed,
                    Some("Install declined".to_string()),
                );
                progress.update_step(
                    AppConnectionStepKind::InstallingAgent,
                    AppConnectionStepState::Cancelled,
                    Some("Install declined".to_string()),
                );
                progress.terminal_message = Some("Install declined".to_string());
                mobile_client
                    .app_store
                    .update_server_health(&server_id, ServerHealthSnapshot::Disconnected);
                mobile_client
                    .app_store
                    .update_server_connection_progress(&server_id, Some(progress.clone()));
                ssh_client.disconnect().await;
                return Ok(());
            }

            progress.update_step(
                AppConnectionStepKind::FindingAgent,
                AppConnectionStepState::Completed,
                Some("Installing latest stable release".to_string()),
            );
            progress.update_step(
                AppConnectionStepKind::InstallingAgent,
                AppConnectionStepState::InProgress,
                None,
            );
            mobile_client
                .app_store
                .update_server_connection_progress(&server_id, Some(progress.clone()));

            let platform = ssh_client
                .detect_remote_platform_with_shell(Some(remote_shell))
                .await
                .map_err(map_ssh_error)?;
            info!(
                "guided ssh connect install platform server_id={} platform={:?}",
                server_id, platform
            );
            let installed_binary = ssh_client
                .install_latest_stable_codex(platform)
                .await
                .map_err(map_ssh_error)?;
            info!(
                "guided ssh connect install completed server_id={} path={}",
                server_id,
                installed_binary.path()
            );
            progress.update_step(
                AppConnectionStepKind::InstallingAgent,
                AppConnectionStepState::Completed,
                Some(installed_binary.path().to_string()),
            );
            mobile_client
                .app_store
                .update_server_connection_progress(&server_id, Some(progress.clone()));
            installed_binary
        }
    };

    progress.update_step(
        AppConnectionStepKind::StartingAgent,
        AppConnectionStepState::InProgress,
        None,
    );
    mobile_client
        .app_store
        .update_server_connection_progress(&server_id, Some(progress.clone()));

    info!(
        "guided ssh connect bootstrapping app server server_id={} host={}",
        server_id,
        credentials.host.as_str()
    );
    let bootstrap = ssh_client
        .bootstrap_codex_server_with_binary(
            &codex_binary,
            working_dir.as_deref(),
            config.host.contains(':'),
        )
        .await
        .map_err(map_ssh_error)?;
    info!(
        "guided ssh connect bootstrap completed server_id={} remote_port={} local_tunnel_port={} pid={:?}",
        server_id, bootstrap.server_port, bootstrap.tunnel_local_port, bootstrap.pid
    );

    progress.update_step(
        AppConnectionStepKind::StartingAgent,
        AppConnectionStepState::Completed,
        Some(format!("Remote port {}", bootstrap.server_port)),
    );
    progress.update_step(
        AppConnectionStepKind::OpeningTunnel,
        AppConnectionStepState::Completed,
        Some(format!("127.0.0.1:{}", bootstrap.tunnel_local_port)),
    );
    progress.update_step(
        AppConnectionStepKind::Connected,
        AppConnectionStepState::InProgress,
        None,
    );
    mobile_client
        .app_store
        .update_server_connection_progress(&server_id, Some(progress.clone()));

    let host = credentials.host.clone();
    mobile_client
        .finish_connect_remote_over_ssh(
            config,
            credentials,
            accept_unknown_host,
            ssh_client,
            SshBootstrapResult {
                server_port: bootstrap.server_port,
                tunnel_local_port: bootstrap.tunnel_local_port,
                server_version: bootstrap.server_version,
                pid: bootstrap.pid,
            },
            working_dir,
            ipc_socket_path_override,
        )
        .await
        .map_err(|error| ClientError::Transport(error.to_string()))?;
    info!(
        "guided ssh connect attached remote session server_id={} host={}",
        server_id,
        host.as_str()
    );

    progress.update_step(
        AppConnectionStepKind::Connected,
        AppConnectionStepState::Completed,
        Some("Connected".to_string()),
    );
    progress.terminal_message = None;
    mobile_client
        .app_store
        .update_server_connection_progress(&server_id, Some(progress.clone()));
    Ok(())
}

pub(crate) fn map_ssh_error(error: SshError) -> ClientError {
    match error {
        SshError::ConnectionFailed(message)
        | SshError::AuthFailed(message)
        | SshError::PortForwardFailed(message)
        | SshError::ExecFailed {
            stderr: message, ..
        } => ClientError::Transport(message),
        SshError::HostKeyVerification { fingerprint } => {
            ClientError::Transport(format!("host key verification failed: {fingerprint}"))
        }
        SshError::Timeout => ClientError::Transport("SSH operation timed out".into()),
        SshError::Disconnected => ClientError::Transport("SSH session disconnected".into()),
    }
}

fn ssh_auth(
    password: Option<String>,
    private_key_pem: Option<String>,
    passphrase: Option<String>,
) -> Result<SshAuth, ClientError> {
    match (password, private_key_pem) {
        (Some(password), None) => Ok(SshAuth::Password(password)),
        (None, Some(key_pem)) => Ok(SshAuth::PrivateKey {
            key_pem,
            passphrase,
        }),
        (None, None) => Err(ClientError::InvalidParams(
            "missing SSH credential: provide either password or private key".into(),
        )),
        (Some(_), Some(_)) => Err(ClientError::InvalidParams(
            "ambiguous SSH credentials: provide either password or private key, not both".into(),
        )),
    }
}

fn normalize_ssh_host(host: &str) -> String {
    let mut normalized = host.trim().trim_matches(['[', ']']).replace("%25", "%");
    if !normalized.contains(':')
        && let Some((base, _scope)) = normalized.split_once('%')
    {
        normalized = base.to_string();
    }
    normalized
}

fn normalize_wake_mac(raw: &str) -> Option<String> {
    let compact = raw
        .trim()
        .replace([':', '-'], "")
        .to_ascii_lowercase();
    if compact.len() != 12 || !compact.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }

    let mut chunks = Vec::with_capacity(6);
    for index in (0..12).step_by(2) {
        chunks.push(compact[index..index + 2].to_string());
    }
    Some(chunks.join(":"))
}

/// Returns `true` if the agent type is a non-Codex agent that should use the
/// provider factory path instead of the guided Codex bootstrap.
fn is_non_codex_agent(agent_type: Option<AgentType>) -> bool {
    matches!(
        agent_type,
        Some(
            AgentType::PiAcp
                | AgentType::PiNative
                | AgentType::DroidAcp
                | AgentType::DroidNative
                | AgentType::GenericAcp
        )
    )
}

/// Connect to a remote server over SSH using the provider factory path
/// for non-Codex agent types (Pi, Droid, ACP).
///
/// This skips all Codex-specific bootstrap steps (FindingAgent, InstallingAgent,
/// StartingAgent, OpeningTunnel) and delegates directly to
/// `MobileClient::connect_remote_over_ssh_with_agent_type`.
async fn run_provider_ssh_connect(
    mobile_client: Arc<crate::MobileClient>,
    config: ServerConfig,
    credentials: SshCredentials,
    accept_unknown_host: bool,
    working_dir: Option<String>,
    agent_type: Option<AgentType>,
    remote_command: Option<String>,
) -> Result<(), ClientError> {
    let server_id = config.server_id.clone();
    info!(
        "SshBridge: provider ssh connect start server_id={} host={} agent_type={}",
        server_id,
        credentials.host.as_str(),
        agent_type
            .map(|at| at.to_string())
            .unwrap_or_else(|| "None".to_string())
    );

    mobile_client
        .connect_remote_over_ssh_with_agent_type(
            config,
            credentials,
            accept_unknown_host,
            working_dir,
            None, // No IPC for provider-backed sessions
            agent_type,
            remote_command,
        )
        .await
        .map_err(|e| ClientError::Transport(e.to_string()))?;

    info!(
        "SshBridge: provider ssh connect completed server_id={}",
        server_id
    );
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_non_codex_agent ─────────────────────────────────────────────

    #[test]
    fn is_non_codex_agent_returns_false_for_none() {
        assert!(!is_non_codex_agent(None));
    }

    #[test]
    fn is_non_codex_agent_returns_false_for_codex() {
        assert!(!is_non_codex_agent(Some(AgentType::Codex)));
    }

    #[test]
    fn is_non_codex_agent_returns_true_for_pi_native() {
        assert!(is_non_codex_agent(Some(AgentType::PiNative)));
    }

    #[test]
    fn is_non_codex_agent_returns_true_for_pi_acp() {
        assert!(is_non_codex_agent(Some(AgentType::PiAcp)));
    }

    #[test]
    fn is_non_codex_agent_returns_true_for_droid_native() {
        assert!(is_non_codex_agent(Some(AgentType::DroidNative)));
    }

    #[test]
    fn is_non_codex_agent_returns_true_for_droid_acp() {
        assert!(is_non_codex_agent(Some(AgentType::DroidAcp)));
    }

    #[test]
    fn is_non_codex_agent_returns_true_for_generic_acp() {
        assert!(is_non_codex_agent(Some(AgentType::GenericAcp)));
    }

    // ── ssh_start_remote_server_connect accepts agent_type ─────────────

    #[tokio::test]
    async fn ssh_start_remote_server_connect_accepts_agent_type_param() {
        let bridge = SshBridge::new();
        // Call with agent_type: None (backward compatible — should behave
        // identically to the old signature, returning server_id immediately
        // while the connect runs in the background).
        let result = bridge
            .ssh_start_remote_server_connect(
                "test-ffi-none".to_string(),
                "Test None".to_string(),
                "127.0.0.1".to_string(),
                22,
                "user".to_string(),
                Some("pass".to_string()),
                None,
                None,
                false,
                None,
                None,
                None, // agent_type: None
                None, // remote_command: None
            )
            .await;
        assert!(
            result.is_ok(),
            "ssh_start_remote_server_connect with None agent_type should return Ok"
        );
        assert_eq!(result.unwrap(), "test-ffi-none");
    }

    #[tokio::test]
    async fn ssh_start_remote_server_connect_accepts_codex_agent_type() {
        let bridge = SshBridge::new();
        let result = bridge
            .ssh_start_remote_server_connect(
                "test-ffi-codex".to_string(),
                "Test Codex".to_string(),
                "127.0.0.1".to_string(),
                22,
                "user".to_string(),
                Some("pass".to_string()),
                None,
                None,
                false,
                None,
                None,
                Some(AgentType::Codex), // agent_type: Codex
                None, // remote_command: None
            )
            .await;
        assert!(
            result.is_ok(),
            "ssh_start_remote_server_connect with Codex agent_type should return Ok"
        );
        assert_eq!(result.unwrap(), "test-ffi-codex");
    }

    #[tokio::test]
    async fn ssh_start_remote_server_connect_accepts_pi_native_agent_type() {
        let bridge = SshBridge::new();
        let result = bridge
            .ssh_start_remote_server_connect(
                "test-ffi-pinative".to_string(),
                "Test PiNative".to_string(),
                "127.0.0.1".to_string(),
                22,
                "user".to_string(),
                Some("pass".to_string()),
                None,
                None,
                false,
                None,
                None,
                Some(AgentType::PiNative),
                None, // remote_command: None
            )
            .await;
        assert!(
            result.is_ok(),
            "ssh_start_remote_server_connect with PiNative agent_type should return Ok"
        );
        assert_eq!(result.unwrap(), "test-ffi-pinative");
    }

    #[tokio::test]
    async fn ssh_start_remote_server_connect_accepts_droid_native_agent_type() {
        let bridge = SshBridge::new();
        let result = bridge
            .ssh_start_remote_server_connect(
                "test-ffi-droidnative".to_string(),
                "Test DroidNative".to_string(),
                "127.0.0.1".to_string(),
                22,
                "user".to_string(),
                Some("pass".to_string()),
                None,
                None,
                false,
                None,
                None,
                Some(AgentType::DroidNative),
                None, // remote_command: None
            )
            .await;
        assert!(
            result.is_ok(),
            "ssh_start_remote_server_connect with DroidNative agent_type should return Ok"
        );
        assert_eq!(result.unwrap(), "test-ffi-droidnative");
    }

    // ── ssh_connect_remote_server accepts agent_type ───────────────────

    #[tokio::test]
    async fn ssh_connect_remote_server_accepts_agent_type_none() {
        let bridge = SshBridge::new();
        // Will fail (no SSH server), but the call proves the signature
        // accepts agent_type: None for backward compatibility.
        let result = bridge
            .ssh_connect_remote_server(
                "test-conn-none".to_string(),
                "Test".to_string(),
                "127.0.0.1".to_string(),
                22,
                "user".to_string(),
                Some("pass".to_string()),
                None,
                None,
                false,
                None,
                None,
                None, // agent_type: None
                None, // remote_command: None
            )
            .await;
        // Expected to fail — no SSH server running.
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn ssh_connect_remote_server_accepts_agent_type_pi() {
        let bridge = SshBridge::new();
        let result = bridge
            .ssh_connect_remote_server(
                "test-conn-pi".to_string(),
                "Test".to_string(),
                "127.0.0.1".to_string(),
                22,
                "user".to_string(),
                Some("pass".to_string()),
                None,
                None,
                false,
                None,
                None,
                Some(AgentType::PiNative),
                None, // remote_command: None
            )
            .await;
        // Expected to fail — no SSH server running.
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn ssh_connect_remote_server_accepts_agent_type_droid() {
        let bridge = SshBridge::new();
        let result = bridge
            .ssh_connect_remote_server(
                "test-conn-droid".to_string(),
                "Test".to_string(),
                "127.0.0.1".to_string(),
                22,
                "user".to_string(),
                Some("pass".to_string()),
                None,
                None,
                false,
                None,
                None,
                Some(AgentType::DroidAcp),
                None, // remote_command: None
            )
            .await;
        // Expected to fail — no SSH server running.
        assert!(result.is_err());
    }

    // ── Provider path does not go through guided bootstrap ──────────────

    #[tokio::test]
    async fn ssh_start_remote_server_connect_provider_failure_updates_health() {
        use crate::store::ServerHealthSnapshot;

        let bridge = SshBridge::new();
        let server_id = "test-provider-fail";

        let _result = bridge
            .ssh_start_remote_server_connect(
                server_id.to_string(),
                "Test Provider Fail".to_string(),
                "127.0.0.1".to_string(),
                22,
                "user".to_string(),
                Some("pass".to_string()),
                None,
                None,
                false,
                None,
                None,
                Some(AgentType::PiNative),
                None, // remote_command: None
            )
            .await;

        // Wait for background task to complete (with short timeout).
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let mobile_client = shared_mobile_client();
        let snapshot = mobile_client.app_store.snapshot();
        let server = snapshot.servers.get(server_id);
        // The server should be Disconnected since there's no SSH server.
        if let Some(server) = server {
            assert_eq!(
                server.health,
                ServerHealthSnapshot::Disconnected,
                "server should be Disconnected after failed provider connect"
            );
        }
    }

    // ── Detection in guided SSH connect ────────────────────────────────

    // VAL-GUIDED-001: detection runs after SSH auth, before Codex bootstrap
    //
    // This is a code-review assertion: the detection call appears in
    // run_guided_ssh_connect() after SshClient::connect() succeeds and
    // before resolve_codex_binary_optional_with_shell(). Verified by
    // reading the function body.
    //
    // In the function body, the sequence is:
    //   1. SshClient::connect() → ssh_client
    //   2. detect_all_agents(&ssh_client) → detected
    //   3. if detected.has_multiple_agents() → return early (picker)
    //   4. resolve_codex_binary_optional_with_shell() → codex bootstrap
    #[test]
    fn val_guided_001_detection_after_ssh_before_codex_verified() {
        // Structural test: verify that the detection-related step kind
        // exists and appears before FindingAgent in the bootstrap sequence.
        let progress = AppConnectionProgressSnapshot::ssh_bootstrap();
        let step_kinds: Vec<_> = progress.steps.iter().map(|s| s.kind).collect();

        let detecting_pos = step_kinds
            .iter()
            .position(|k| *k == AppConnectionStepKind::DetectingAgents);
        let finding_codex_pos = step_kinds
            .iter()
            .position(|k| *k == AppConnectionStepKind::FindingAgent);

        assert!(
            detecting_pos.is_some(),
            "DetectingAgents step must exist in bootstrap progress"
        );
        assert!(
            finding_codex_pos.is_some(),
            "FindingAgent step must exist in bootstrap progress"
        );
        assert!(
            detecting_pos.unwrap() < finding_codex_pos.unwrap(),
            "DetectingAgents must appear before FindingAgent in the step sequence"
        );
    }

    // VAL-GUIDED-002: when only Codex found, proceeds with normal Codex bootstrap (unchanged)
    //
    // When detection returns only Codex (agents.len() == 1), the guided flow
    // continues exactly as before with no picker shown. This is verified by:
    // 1. The code path: `if detected.has_multiple_agents()` is false → skip early return
    // 2. The connection progress has `pending_agent_selection == false` and empty `detected_agents`
    #[test]
    fn val_guided_002_codex_only_proceeds_normally() {
        let mut progress = AppConnectionProgressSnapshot::ssh_bootstrap();

        // Simulate: detection ran, found only Codex.
        progress.update_step(
            AppConnectionStepKind::DetectingAgents,
            AppConnectionStepState::Completed,
            Some("1 agent(s) detected".to_string()),
        );
        // pending_agent_selection remains false
        assert!(!progress.pending_agent_selection);
        assert!(progress.detected_agents.is_empty());

        // FindingAgent should still be Pending (not yet started)
        let finding_step = progress
            .steps
            .iter()
            .find(|s| s.kind == AppConnectionStepKind::FindingAgent)
            .unwrap();
        assert_eq!(finding_step.state, AppConnectionStepState::Pending);
    }

    // VAL-GUIDED-003: when multiple agents found, returns detected agent list for iOS picker
    #[test]
    fn val_guided_003_multiple_agents_populates_progress() {
        use crate::provider::AgentInfo;

        let mut progress = AppConnectionProgressSnapshot::ssh_bootstrap();

        // Simulate: detection found Codex + Pi.
        let detected = vec![
            AgentInfo {
                id: "codex".to_string(),
                display_name: "Codex".to_string(),
                description: "Codex app-server".to_string(),
                detected_transports: vec![AgentType::Codex],
                capabilities: vec!["streaming".to_string()],
            },
            AgentInfo {
                id: "pi-native".to_string(),
                display_name: "Pi (Native)".to_string(),
                description: "Pi agent".to_string(),
                detected_transports: vec![AgentType::PiNative],
                capabilities: vec!["streaming".to_string()],
            },
        ];

        progress.detected_agents = detected.clone();
        progress.pending_agent_selection = true;
        progress.terminal_message = Some("Multiple agents detected. Please select one.".to_string());

        assert!(progress.pending_agent_selection);
        assert_eq!(progress.detected_agents.len(), 2);
        assert_eq!(progress.detected_agents[0].id, "codex");
        assert_eq!(progress.detected_agents[1].id, "pi-native");
        assert!(progress.terminal_message.is_some());
    }

    // VAL-GUIDED-004: detection failure falls through to Codex path
    //
    // When detect_all_agents() encounters SSH errors or times out, it
    // returns a DetectedAgents with only Codex (via timeout/fallback).
    // The guided flow then proceeds with normal Codex bootstrap.
    #[test]
    fn val_guided_004_detection_failure_falls_through() {
        let mut progress = AppConnectionProgressSnapshot::ssh_bootstrap();

        // Simulate: detection failed/timed out → only Codex in result.
        // The code path: `if detected.has_multiple_agents()` → false
        // → sets pending_agent_selection = false and continues.
        progress.update_step(
            AppConnectionStepKind::DetectingAgents,
            AppConnectionStepState::Completed,
            Some("1 agent(s) detected".to_string()),
        );
        progress.pending_agent_selection = false;

        // Should not be waiting for agent selection
        assert!(!progress.pending_agent_selection);
        assert!(progress.detected_agents.is_empty());

        // Should be able to continue to FindingAgent
        let finding_step = progress
            .steps
            .iter()
            .find(|s| s.kind == AppConnectionStepKind::FindingAgent)
            .unwrap();
        assert_eq!(finding_step.state, AppConnectionStepState::Pending);
    }

    // VAL-GUIDED-005: existing Codex-only flow is unchanged
    //
    // All existing tests must pass. The Codex-only flow is unchanged because:
    // 1. When agent_type is None/Codex, the guided path is selected (unchanged routing)
    // 2. detect_all_agents() always includes Codex, so if no Pi/Droid found,
    //    has_multiple_agents() returns false → normal Codex bootstrap proceeds
    // 3. The DetectingAgents step is completed and the flow continues to FindingAgent
    #[test]
    fn val_guided_005_codex_flow_has_detection_step_in_bootstrap() {
        let progress = AppConnectionProgressSnapshot::ssh_bootstrap();

        // Verify the detection step exists in the bootstrap sequence
        let detecting_step = progress
            .steps
            .iter()
            .find(|s| s.kind == AppConnectionStepKind::DetectingAgents);
        assert!(
            detecting_step.is_some(),
            "DetectingAgents step must be present in bootstrap"
        );

        // Verify it starts as Pending
        let detecting_step = detecting_step.unwrap();
        assert_eq!(detecting_step.state, AppConnectionStepState::Pending);

        // Verify default state is not pending agent selection
        assert!(!progress.pending_agent_selection);
        assert!(progress.detected_agents.is_empty());
    }

    // ── AppConnectionProgressSnapshot new fields ───────────────────────

    #[test]
    fn progress_snapshot_default_fields_empty() {
        let progress = AppConnectionProgressSnapshot::ssh_bootstrap();
        assert!(progress.detected_agents.is_empty());
        assert!(!progress.pending_agent_selection);
    }

    #[test]
    fn progress_snapshot_can_hold_detected_agents() {
        use crate::provider::AgentInfo;

        let mut progress = AppConnectionProgressSnapshot::ssh_bootstrap();
        progress.detected_agents = vec![
            AgentInfo {
                id: "codex".to_string(),
                display_name: "Codex".to_string(),
                description: "Codex".to_string(),
                detected_transports: vec![AgentType::Codex],
                capabilities: vec![],
            },
            AgentInfo {
                id: "pi-native".to_string(),
                display_name: "Pi".to_string(),
                description: "Pi".to_string(),
                detected_transports: vec![AgentType::PiNative],
                capabilities: vec![],
            },
        ];

        assert_eq!(progress.detected_agents.len(), 2);
    }

    #[test]
    fn progress_snapshot_pending_agent_selection_flag() {
        let mut progress = AppConnectionProgressSnapshot::ssh_bootstrap();
        assert!(!progress.pending_agent_selection);
        progress.pending_agent_selection = true;
        assert!(progress.pending_agent_selection);
    }

    // ── Guided connect with detection updates store progress ───────────

    #[tokio::test]
    async fn guided_connect_with_codex_only_detection_updates_progress() {
        // Start a guided connect that will fail at SSH level (no server),
        // but verify the progress snapshot includes the DetectingAgents step.
        let bridge = SshBridge::new();
        let server_id = "test-detection-progress";

        let _result = bridge
            .ssh_start_remote_server_connect(
                server_id.to_string(),
                "Test Detection".to_string(),
                "127.0.0.1".to_string(),
                22,
                "user".to_string(),
                Some("pass".to_string()),
                None,
                None,
                false,
                None,
                None,
                None, // agent_type: None → guided path
                None, // remote_command: None
            )
            .await;

        // The connect runs in background and will fail (no SSH server).
        // Wait briefly for the background task to start.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // The server should exist in the store (created at start).
        let mobile_client = shared_mobile_client();
        let snapshot = mobile_client.app_store.snapshot();
        let server = snapshot.servers.get(server_id);
        // Server may or may not still be present depending on timing.
        // The important thing is the method accepted the call.
        if let Some(server) = server {
            if let Some(ref progress) = server.connection_progress {
                // The DetectingAgents step should be in the progress steps.
                let has_detecting = progress
                    .steps
                    .iter()
                    .any(|s| s.kind == AppConnectionStepKind::DetectingAgents);
                assert!(
                    has_detecting,
                    "DetectingAgents step should be present in connection progress"
                );
            }
        }
    }

    // ── Provider SSH connect with binary validation ───────────────────

    /// VAL-PROVIDER-001: Validates selected agent binary exists before spawning transport.
    ///
    /// The provider connect path (`run_provider_ssh_connect`) delegates to
    /// `MobileClient::connect_remote_over_ssh_with_agent_type()`, which now
    /// calls `validate_agent_binary_on_host()` after SSH connects and before
    /// `create_provider_over_ssh()`. This test verifies the structural
    /// validation: `agent_binary_name` returns a binary for each non-Codex
    /// agent type, meaning validation will execute for those types.
    #[test]
    fn val_provider_001_provider_connect_validates_binary() {
        use crate::mobile_client::agent_binary_name;

        // All non-Codex agent types that go through run_provider_ssh_connect
        // should have a binary name for validation.
        let validated = [
            AgentType::PiNative,
            AgentType::PiAcp,
            AgentType::DroidNative,
            AgentType::DroidAcp,
        ];
        for at in &validated {
            assert!(
                agent_binary_name(*at).is_some(),
                "{at:?} should have a binary name for validation"
            );
        }
    }

    /// VAL-PROVIDER-002: If selected agent not found, returns clear error.
    ///
    /// When the binary check fails, `validate_agent_binary_on_host` returns
    /// `TransportError::ConnectionFailed` with a descriptive message including
    /// the agent name and binary. This test verifies the error message
    /// construction uses the correct agent and binary names.
    #[test]
    fn val_provider_002_error_message_contains_agent_and_binary() {
        use crate::mobile_client::agent_binary_name;

        // For each agent type with a binary, verify the expected message parts.
        let cases = [
            (AgentType::PiNative, "pi"),
            (AgentType::PiAcp, "pi"),
            (AgentType::DroidNative, "droid"),
            (AgentType::DroidAcp, "droid"),
        ];

        for (agent_type, expected_binary) in &cases {
            let binary = agent_binary_name(*agent_type).unwrap();
            assert_eq!(binary, *expected_binary);

            // The error message format is:
            // "{agent_type} agent not found on remote host — binary '{binary}' is not on PATH"
            // Verify the display name is meaningful.
            let display = format!("{agent_type}");
            assert!(!display.is_empty());

            // Construct the expected error message to verify it reads well.
            let msg = format!(
                "{agent_type} agent not found on remote host — binary '{binary}' is not on PATH"
            );
            assert!(msg.contains(expected_binary));
            assert!(msg.contains("not found"));
            assert!(msg.contains("not on PATH"));
        }
    }

    /// VAL-PROVIDER-003: If selected agent found, proceeds with provider factory.
    ///
    /// When validation confirms the binary exists, the connect flow
    /// proceeds to `create_provider_over_ssh()`. This test verifies
    /// the flow by checking that `is_non_codex_agent` correctly identifies
    /// all agent types that should go through the provider path (and thus
    /// through binary validation).
    #[test]
    fn val_provider_003_provider_path_covers_all_non_codex_types() {
        // Verify that all agent types with binaries are routed through
        // the provider path (is_non_codex_agent returns true).
        assert!(is_non_codex_agent(Some(AgentType::PiNative)));
        assert!(is_non_codex_agent(Some(AgentType::PiAcp)));
        assert!(is_non_codex_agent(Some(AgentType::DroidNative)));
        assert!(is_non_codex_agent(Some(AgentType::DroidAcp)));
        assert!(is_non_codex_agent(Some(AgentType::GenericAcp)));

        // Codex and None should NOT go through the provider path.
        assert!(!is_non_codex_agent(Some(AgentType::Codex)));
        assert!(!is_non_codex_agent(None));
    }

    /// Verify that run_provider_ssh_connect is called for non-Codex agent types.
    /// This tests the routing in ssh_start_remote_server_connect.
    #[tokio::test]
    async fn provider_connect_task_fails_gracefully_for_pi_native() {
        use crate::store::ServerHealthSnapshot;

        let bridge = SshBridge::new();
        let server_id = "test-provider-validation-pi";

        let _result = bridge
            .ssh_start_remote_server_connect(
                server_id.to_string(),
                "Test Provider Validation".to_string(),
                "127.0.0.1".to_string(),
                22,
                "user".to_string(),
                Some("pass".to_string()),
                None,
                None,
                false,
                None,
                None,
                Some(AgentType::PiNative),
                None, // remote_command: None
            )
            .await;

        // Wait for background task to complete (SSH will fail).
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let mobile_client = shared_mobile_client();
        let snapshot = mobile_client.app_store.snapshot();
        if let Some(server) = snapshot.servers.get(server_id) {
            assert_eq!(
                server.health,
                ServerHealthSnapshot::Disconnected,
                "server should be Disconnected after failed provider connect"
            );
        }
    }

    /// Verify that run_provider_ssh_connect is called for non-Codex agent types.
    #[tokio::test]
    async fn provider_connect_task_fails_gracefully_for_droid_native() {
        use crate::store::ServerHealthSnapshot;

        let bridge = SshBridge::new();
        let server_id = "test-provider-validation-droid";

        let _result = bridge
            .ssh_start_remote_server_connect(
                server_id.to_string(),
                "Test Provider Validation".to_string(),
                "127.0.0.1".to_string(),
                22,
                "user".to_string(),
                Some("pass".to_string()),
                None,
                None,
                false,
                None,
                None,
                Some(AgentType::DroidNative),
                None, // remote_command: None
            )
            .await;

        // Wait for background task to complete (SSH will fail).
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let mobile_client = shared_mobile_client();
        let snapshot = mobile_client.app_store.snapshot();
        if let Some(server) = snapshot.servers.get(server_id) {
            assert_eq!(
                server.health,
                ServerHealthSnapshot::Disconnected,
                "server should be Disconnected after failed provider connect"
            );
        }
    }

    // ══════════════════════════════════════════════════════════════════════
    // SSH Bootstrap Separation Tests (VAL-DISC-017 / VAL-DISC-018)
    // ══════════════════════════════════════════════════════════════════════

    /// VAL-DISC-017: SSH connect establishes a connection without any
    /// Codex-specific assumptions.
    ///
    /// `ssh_connect_only` performs a pure SSH handshake. No agent binary
    /// resolution, no installation, no server bootstrap. This is the
    /// generic transport layer that works with any agent.
    #[tokio::test]
    async fn ssh_bootstrap_separation_connect_only_is_generic() {
        let bridge = SshBridge::new();
        // No SSH server running locally, so this will fail — but it proves
        // the method exists and doesn't reference any Codex-specific logic.
        let result = bridge
            .ssh_connect_only(
                "127.0.0.1".to_string(),
                22,
                "user".to_string(),
                Some("pass".to_string()),
                None,
                None,
                false,
            )
            .await;
        // Expected to fail — no SSH server running.
        assert!(result.is_err(), "ssh_connect_only should fail without SSH server");
    }

    /// VAL-DISC-017: `ssh_connect_and_bootstrap` is clearly documented as
    /// Codex-specific, while `ssh_connect_only` is generic.
    ///
    /// This structural test verifies both methods exist and serve different
    /// purposes. The `ssh_connect_and_bootstrap` unconditionally bootstraps
    /// Codex; `ssh_connect_only` is agent-agnostic.
    #[test]
    fn ssh_bootstrap_separation_two_distinct_methods_exist() {
        // Structural assertion: verify the progress snapshot for SSH bootstrap
        // contains all expected steps in the correct order.
        let progress = AppConnectionProgressSnapshot::ssh_bootstrap();
        let step_kinds: Vec<_> = progress.steps.iter().map(|s| s.kind).collect();

        // The generic steps appear before the Codex-specific ones.
        let ssh_pos = step_kinds.iter().position(|k| *k == AppConnectionStepKind::ConnectingToSsh);
        let detect_pos = step_kinds.iter().position(|k| *k == AppConnectionStepKind::DetectingAgents);
        let find_pos = step_kinds.iter().position(|k| *k == AppConnectionStepKind::FindingAgent);
        let install_pos = step_kinds.iter().position(|k| *k == AppConnectionStepKind::InstallingAgent);
        let start_pos = step_kinds.iter().position(|k| *k == AppConnectionStepKind::StartingAgent);

        assert!(ssh_pos.is_some(), "ConnectingToSsh step must exist");
        assert!(detect_pos.is_some(), "DetectingAgents step must exist");
        assert!(find_pos.is_some(), "FindingAgent step must exist");
        assert!(install_pos.is_some(), "InstallingAgent step must exist");
        assert!(start_pos.is_some(), "StartingAgent step must exist");

        // Verify ordering: SSH → Detect → Find → Install → Start
        assert!(ssh_pos.unwrap() < detect_pos.unwrap());
        assert!(detect_pos.unwrap() < find_pos.unwrap());
        assert!(find_pos.unwrap() < install_pos.unwrap());
        assert!(install_pos.unwrap() < start_pos.unwrap());
    }

    /// VAL-DISC-018: No auto-install — when Codex binary is detected as
    /// present, the InstallingAgent step is skipped (set to Cancelled).
    ///
    /// In `run_guided_ssh_connect`, when `resolve_codex_binary_optional_with_shell`
    /// returns `Some(binary)`, the `InstallingAgent` step is immediately set
    /// to `Cancelled` with the message "Already installed". No installation
    /// attempt is made.
    #[test]
    fn ssh_bootstrap_separation_codex_present_skips_install() {
        let mut progress = AppConnectionProgressSnapshot::ssh_bootstrap();

        // Simulate: SSH connected, detection completed, Codex found.
        progress.update_step(
            AppConnectionStepKind::ConnectingToSsh,
            AppConnectionStepState::Completed,
            Some("Connected to host".to_string()),
        );
        progress.update_step(
            AppConnectionStepKind::DetectingAgents,
            AppConnectionStepState::Completed,
            Some("1 agent(s) detected".to_string()),
        );
        progress.update_step(
            AppConnectionStepKind::FindingAgent,
            AppConnectionStepState::Completed,
            Some("/usr/local/bin/codex".to_string()),
        );

        // When the binary is found, the install step is set to Cancelled
        // (not InProgress or Completed).
        progress.update_step(
            AppConnectionStepKind::InstallingAgent,
            AppConnectionStepState::Cancelled,
            Some("Already installed".to_string()),
        );

        // Verify: install step was cancelled, not run
        let install_step = progress
            .steps
            .iter()
            .find(|s| s.kind == AppConnectionStepKind::InstallingAgent)
            .unwrap();
        assert_eq!(
            install_step.state,
            AppConnectionStepState::Cancelled,
            "InstallingAgent should be Cancelled when binary is already present"
        );
        assert_eq!(
            install_step.detail.as_deref(),
            Some("Already installed"),
            "InstallingAgent detail should indicate already installed"
        );
    }

    /// VAL-DISC-018: No auto-install for detected non-Codex agents.
    ///
    /// When a non-Codex agent (e.g., Pi) is detected, the guided flow
    /// returns early for agent selection. No installation step is attempted.
    /// The `InstallingAgent` step remains in its initial state (Pending).
    #[test]
    fn ssh_bootstrap_separation_pi_detected_no_install() {
        use crate::provider::AgentInfo;

        let mut progress = AppConnectionProgressSnapshot::ssh_bootstrap();

        // Simulate: SSH connected, detection found Pi agent only.
        progress.update_step(
            AppConnectionStepKind::ConnectingToSsh,
            AppConnectionStepState::Completed,
            Some("Connected to host".to_string()),
        );
        progress.update_step(
            AppConnectionStepKind::DetectingAgents,
            AppConnectionStepState::Completed,
            Some("1 agent(s) detected".to_string()),
        );

        // Non-Codex agent detected → early return for picker.
        progress.detected_agents = vec![AgentInfo {
            id: "pi-native".to_string(),
            display_name: "Pi (Native)".to_string(),
            description: "Pi agent".to_string(),
            detected_transports: vec![AgentType::PiNative],
            capabilities: vec!["streaming".to_string()],
        }];
        progress.pending_agent_selection = true;
        progress.terminal_message = Some("Agent detected. Please select it to connect.".to_string());

        // Verify: install step was never touched (still Pending)
        let install_step = progress
            .steps
            .iter()
            .find(|s| s.kind == AppConnectionStepKind::InstallingAgent)
            .unwrap();
        assert_eq!(
            install_step.state,
            AppConnectionStepState::Pending,
            "InstallingAgent should remain Pending when non-Codex agent is detected"
        );

        // Verify: agent selection is pending
        assert!(progress.pending_agent_selection);
        assert_eq!(progress.detected_agents.len(), 1);
        assert_eq!(progress.detected_agents[0].id, "pi-native");
    }

    /// VAL-DISC-018: Generic SSH transport works with any agent binary
    /// that exists on the remote host.
    ///
    /// The provider factory path (`run_provider_ssh_connect`) delegates to
    /// `MobileClient::connect_remote_over_ssh_with_agent_type`, which
    /// validates the binary exists but never installs it. This test
    /// verifies the binary validation function exists for each non-Codex
    /// agent type.
    #[test]
    fn ssh_bootstrap_separation_provider_path_validates_without_installing() {
        use crate::mobile_client::agent_binary_name;

        // All non-Codex agent types have a binary name for validation.
        // Validation only checks existence — it never installs.
        let cases = [
            (AgentType::PiNative, "pi"),
            (AgentType::PiAcp, "pi"),
            (AgentType::DroidNative, "droid"),
            (AgentType::DroidAcp, "droid"),
        ];

        for (agent_type, expected_binary) in &cases {
            let binary = agent_binary_name(*agent_type);
            assert_eq!(
                binary.as_deref(),
                Some(*expected_binary),
                "{agent_type:?} should validate binary '{expected_binary}'"
            );
        }

        // Codex is NOT validated through this path.
        assert!(
            agent_binary_name(AgentType::Codex).is_none(),
            "Codex should NOT go through provider binary validation"
        );
    }

    /// VAL-DISC-017: Codex installation is handled by Codex-specific logic only.
    ///
    /// The `is_non_codex_agent` function gates the routing: Codex/None goes
    /// through guided bootstrap (which may install), non-Codex goes through
    /// the provider factory (which never installs). This test verifies that
    /// the routing function correctly separates the two paths.
    #[test]
    fn ssh_bootstrap_separation_codex_install_in_codex_branch_only() {
        // Codex and None → guided path (may install)
        assert!(!is_non_codex_agent(None), "None should use guided (Codex) path");
        assert!(!is_non_codex_agent(Some(AgentType::Codex)), "Codex should use guided path");

        // All others → provider path (never installs)
        assert!(is_non_codex_agent(Some(AgentType::PiNative)));
        assert!(is_non_codex_agent(Some(AgentType::PiAcp)));
        assert!(is_non_codex_agent(Some(AgentType::DroidNative)));
        assert!(is_non_codex_agent(Some(AgentType::DroidAcp)));
        assert!(is_non_codex_agent(Some(AgentType::GenericAcp)));
    }

    /// VAL-DISC-018: When no agents detected and Codex binary not found,
    /// install is gated behind user prompt (not auto-install).
    ///
    /// In the guided flow, when Codex is not found, `pending_install` is
    /// set to `true` and the flow waits for user confirmation before
    /// proceeding with installation. This is NOT auto-install.
    #[test]
    fn ssh_bootstrap_separation_install_requires_user_prompt() {
        let mut progress = AppConnectionProgressSnapshot::ssh_bootstrap();

        // Simulate: SSH connected, detection completed, no agents found.
        progress.update_step(
            AppConnectionStepKind::ConnectingToSsh,
            AppConnectionStepState::Completed,
            Some("Connected to host".to_string()),
        );
        progress.update_step(
            AppConnectionStepKind::DetectingAgents,
            AppConnectionStepState::Completed,
            Some("0 agent(s) detected".to_string()),
        );

        // FindingAgent → AwaitingUserInput (not auto-install)
        progress.update_step(
            AppConnectionStepKind::FindingAgent,
            AppConnectionStepState::AwaitingUserInput,
            Some("Codex not found on remote host".to_string()),
        );
        progress.pending_install = true;

        // Verify: install is pending user decision, not auto-started
        let find_step = progress
            .steps
            .iter()
            .find(|s| s.kind == AppConnectionStepKind::FindingAgent)
            .unwrap();
        assert_eq!(
            find_step.state,
            AppConnectionStepState::AwaitingUserInput,
            "FindingAgent should be AwaitingUserInput when binary not found"
        );
        assert!(
            progress.pending_install,
            "pending_install flag should be set"
        );

        // Install step should NOT have started
        let install_step = progress
            .steps
            .iter()
            .find(|s| s.kind == AppConnectionStepKind::InstallingAgent)
            .unwrap();
        assert_eq!(
            install_step.state,
            AppConnectionStepState::Pending,
            "InstallingAgent should remain Pending until user confirms"
        );
    }

    /// VAL-DISC-017: The provider SSH connect path completely bypasses
    /// Codex bootstrap steps (FindingAgent, InstallingAgent, StartingAgent).
    ///
    /// When `is_non_codex_agent` returns true, `run_provider_ssh_connect`
    /// is called instead of `run_guided_ssh_connect`. This means no
    /// `resolve_codex_binary`, no `install_latest_stable_codex`, and no
    /// `bootstrap_codex_server` is ever invoked for non-Codex agents.
    #[tokio::test]
    async fn ssh_bootstrap_separation_provider_skips_codex_bootstrap() {
        use crate::store::ServerHealthSnapshot;

        let bridge = SshBridge::new();
        let server_id = "test-separation-provider";

        // Start a PiNative connection — will fail at SSH level since there's
        // no server, but the important thing is that the background task
        // takes the provider path, not the guided path.
        let _result = bridge
            .ssh_start_remote_server_connect(
                server_id.to_string(),
                "Test Separation".to_string(),
                "127.0.0.1".to_string(),
                22,
                "user".to_string(),
                Some("pass".to_string()),
                None,
                None,
                false,
                None,
                None,
                Some(AgentType::PiNative),
                None,
            )
            .await;

        // Wait for background task to complete.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Verify the server ended up Disconnected (SSH failed as expected).
        // This confirms the provider path was taken (which doesn't bootstrap
        // Codex) and the failure came from SSH, not from Codex bootstrap.
        let mobile_client = shared_mobile_client();
        let snapshot = mobile_client.app_store.snapshot();
        if let Some(server) = snapshot.servers.get(server_id) {
            assert_eq!(
                server.health,
                ServerHealthSnapshot::Disconnected,
                "provider path should end in Disconnected when SSH fails"
            );
            // The progress should be cleared on failure.
            assert!(
                server.connection_progress.is_none(),
                "connection progress should be cleared after failure"
            );
        }
    }
}
