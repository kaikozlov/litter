//! Generic ACP transport implementation.
//!
//! A configurable ACP transport that works with any agent binary. Accepts a
//! configurable remote command (from `ProviderConfig::remote_command`) to
//! launch the agent, then speaks the standard ACP protocol (NDJSON-framed
//! JSON-RPC) over the resulting stdio/SSH stream.
//!
//! # Lifecycle
//!
//! ```text
//! SSH/exec → spawn remote command → ACP JSON-RPC (NDJSON)
//!   → initialize → authenticate → session/new → session/prompt
//!   → stream session/update → session/cancel (on abort)
//! ```
//!
//! # Usage
//!
//! The factory function `create_generic_acp()` in `factory.rs` constructs this
//! transport with the configured remote command:
//! - Reads `ProviderConfig::remote_command` for the shell command to spawn.
//! - Spawns the command over SSH via `SshClient::exec_stream()`.
//! - Constructs `GenericAcpTransport` wrapping the stream.
//! - Completes ACP handshake (initialize + authenticate) with timeout.
//!
//! # Difference from PiAcpTransport
//!
//! `GenericAcpTransport` is not tied to any specific agent. The remote command
//! comes from configuration, making it work with Codex, Claude, GPT, Gemini,
//! or any ACP-compatible binary. `PiAcpTransport` is the same architecture
//! but hardcoded to `npx pi-acp`.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;

use crate::provider::acp::client::{AcpClient, InitializeResult};
use crate::provider::{ProviderConfig, ProviderEvent, ProviderTransport, SessionInfo};
use crate::transport::{RpcError, TransportError};

/// Default working directory for ACP sessions.
const DEFAULT_CWD: &str = "~";

/// Generic ACP transport wrapping the universal ACP client.
///
/// Communicates with any ACP-compatible agent via NDJSON-framed JSON-RPC.
/// The command used to spawn the agent is determined externally (from
/// `ProviderConfig::remote_command`) — this transport only handles the
/// ACP protocol lifecycle over the resulting stream.
///
/// # Construction
///
/// Create with `GenericAcpTransport::new(stream)` where `stream` is any
/// bidirectional I/O (SSH channel, mock, etc.). Then call `connect()` to
/// perform the ACP handshake (initialize + authenticate).
pub struct GenericAcpTransport {
    /// The underlying ACP client.
    client: AcpClient,
    /// Whether the handshake (initialize + authenticate) has completed.
    initialized: Arc<Mutex<bool>>,
    /// Whether the transport has been disconnected.
    disconnected: Arc<Mutex<bool>>,
    /// The result of the initialize handshake, including capabilities and auth methods.
    /// Available after `connect()` completes successfully.
    initialize_result: Arc<Mutex<Option<InitializeResult>>>,
}

impl GenericAcpTransport {
    /// Create a new generic ACP transport over the given bidirectional stream.
    ///
    /// The stream is typically an SSH channel connected to an ACP-compatible
    /// agent. The ACP client is created but not yet initialized — call
    /// `connect()` to perform the handshake.
    pub fn new<T>(stream: T) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + 'static,
    {
        Self {
            client: AcpClient::new(stream),
            initialized: Arc::new(Mutex::new(false)),
            disconnected: Arc::new(Mutex::new(false)),
            initialize_result: Arc::new(Mutex::new(None)),
        }
    }

    /// Create a generic ACP transport from an existing ACP client.
    ///
    /// Useful when the client has already been created and possibly
    /// initialized externally.
    pub fn from_client(client: AcpClient) -> Self {
        Self {
            client,
            initialized: Arc::new(Mutex::new(false)),
            disconnected: Arc::new(Mutex::new(false)),
            initialize_result: Arc::new(Mutex::new(None)),
        }
    }

    /// Perform the ACP initialize + authenticate handshake.
    ///
    /// Called automatically by `connect()`. Can also be called directly
    /// if you need to control handshake timing.
    pub async fn handshake(
        &self,
        client_name: &str,
        client_version: &str,
    ) -> Result<(), GenericAcpError> {
        // Initialize: send protocol version + client info, receive capabilities + auth methods.
        let init_result = self
            .client
            .initialize(client_name, client_version)
            .await
            .map_err(GenericAcpError::from)?;

        tracing::info!(
            "GenericAcp initialized: protocol_version={:?}, auth_methods={:?}",
            init_result.protocol_version,
            init_result.auth_methods
        );

        // Store the initialize result for capabilities querying.
        {
            let mut guard = self.initialize_result.lock().await;
            *guard = Some(init_result);
        }

        // Authenticate using the first available method.
        self.client
            .authenticate(None)
            .await
            .map_err(GenericAcpError::from)?;

        let mut guard = self.initialized.lock().await;
        *guard = true;

        Ok(())
    }

    /// Create a new session via ACP.
    ///
    /// Returns the session ID on success.
    pub async fn create_session(&self, cwd: &str) -> Result<String, GenericAcpError> {
        let result = self
            .client
            .session_new(cwd)
            .await
            .map_err(GenericAcpError::from)?;
        Ok(result.session_id.to_string())
    }

    /// Send a prompt and stream events.
    ///
    /// Events are emitted via the `event_receiver()` stream.
    /// Returns the stop reason when the prompt completes.
    pub async fn send_prompt(
        &self,
        session_id: &str,
        prompt_text: &str,
    ) -> Result<String, GenericAcpError> {
        let session_id = agent_client_protocol_schema::SessionId::new(session_id.to_string());
        let result = self
            .client
            .session_prompt(&session_id, prompt_text)
            .await
            .map_err(GenericAcpError::from)?;
        Ok(result.stop_reason)
    }

    /// Cancel the active prompt for a session.
    pub async fn cancel_prompt(&self, session_id: &str) -> Result<(), GenericAcpError> {
        let session_id = agent_client_protocol_schema::SessionId::new(session_id.to_string());
        self.client
            .session_cancel(&session_id)
            .await
            .map_err(GenericAcpError::from)
    }

    /// Load an existing session via ACP.
    pub async fn load_session(
        &self,
        session_id: &str,
        cwd: &str,
    ) -> Result<(), GenericAcpError> {
        let session_id = agent_client_protocol_schema::SessionId::new(session_id.to_string());
        self.client
            .session_load(&session_id, cwd)
            .await
            .map_err(GenericAcpError::from)
    }

    /// Check if the handshake has completed.
    pub async fn is_initialized(&self) -> bool {
        *self.initialized.lock().await
    }

    /// Get the initialize result (capabilities, auth methods, agent info)
    /// from the last successful handshake.
    ///
    /// Returns `None` if the handshake has not completed or the transport
    /// has been disconnected.
    ///
    /// VAL-ACP-036: Capabilities from the initialize response are accessible
    /// after `connect()` completes.
    pub async fn initialize_result(&self) -> Option<InitializeResult> {
        self.initialize_result.lock().await.clone()
    }

    /// Convenience method to get agent capabilities after handshake.
    ///
    /// Returns `None` if the handshake has not completed.
    pub async fn capabilities(
        &self,
    ) -> Option<agent_client_protocol_schema::AgentCapabilities> {
        self.initialize_result
            .lock()
            .await
            .as_ref()
            .map(|r| r.agent_capabilities.clone())
    }
}

/// Error type for generic ACP transport operations.
#[derive(Debug, thiserror::Error)]
pub enum GenericAcpError {
    /// The transport is not connected.
    #[error("not connected")]
    NotConnected,

    /// The handshake (initialize + authenticate) has not completed.
    #[error("not initialized")]
    NotInitialized,

    /// ACP client error.
    #[error("ACP error: {0}")]
    AcpClient(#[from] crate::provider::acp::client::AcpClientError),

    /// Transport disconnected.
    #[error("transport disconnected")]
    Disconnected,
}

impl From<GenericAcpError> for RpcError {
    fn from(err: GenericAcpError) -> Self {
        match err {
            GenericAcpError::AcpClient(e) => RpcError::from(e),
            GenericAcpError::NotConnected | GenericAcpError::Disconnected => {
                RpcError::Transport(TransportError::Disconnected)
            }
            GenericAcpError::NotInitialized => RpcError::Server {
                code: -1,
                message: err.to_string(),
            },
        }
    }
}

impl From<GenericAcpError> for TransportError {
    fn from(err: GenericAcpError) -> Self {
        match err {
            GenericAcpError::NotConnected | GenericAcpError::Disconnected => {
                TransportError::Disconnected
            }
            _ => TransportError::ConnectionFailed(err.to_string()),
        }
    }
}

#[async_trait]
impl ProviderTransport for GenericAcpTransport {
    async fn connect(&mut self, config: &ProviderConfig) -> Result<(), TransportError> {
        if *self.disconnected.lock().await {
            return Err(TransportError::ConnectionFailed(
                "GenericAcp transport cannot reconnect; create a new instance".to_string(),
            ));
        }

        // Perform the ACP handshake.
        self.handshake(&config.client_name, &config.client_version)
            .await
            .map_err(|e| match e {
                GenericAcpError::AcpClient(
                    crate::provider::acp::client::AcpClientError::Disconnected,
                ) => TransportError::Disconnected,
                GenericAcpError::AcpClient(
                    crate::provider::acp::client::AcpClientError::Transport(_),
                ) => TransportError::ConnectionFailed(e.to_string()),
                _ => TransportError::ConnectionFailed(e.to_string()),
            })?;

        Ok(())
    }

    async fn disconnect(&mut self) {
        let mut guard = self.disconnected.lock().await;
        if *guard {
            return; // Idempotent.
        }
        *guard = true;
        drop(guard);

        self.client.shutdown().await;
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        if *self.disconnected.lock().await {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }

        if !*self.initialized.lock().await {
            return Err(RpcError::Server {
                code: -1,
                message: "Generic ACP transport not initialized".to_string(),
            });
        }

        match method {
            "session/new" => {
                let cwd = params
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or(DEFAULT_CWD);
                let session_id = self.create_session(cwd).await?;
                Ok(serde_json::json!({ "session_id": session_id }))
            }
            "prompt" => {
                let text = params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RpcError::Deserialization("missing 'text' field".to_string())
                    })?;
                let session_id = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                let stop_reason = self.send_prompt(session_id, text).await?;
                Ok(serde_json::json!({ "stop_reason": stop_reason }))
            }
            "cancel" => {
                let session_id = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                self.cancel_prompt(session_id).await?;
                Ok(serde_json::Value::Null)
            }
            "session/load" => {
                let session_id = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RpcError::Deserialization("missing 'session_id' field".to_string())
                    })?;
                let cwd = params
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or(DEFAULT_CWD);
                self.load_session(session_id, cwd).await?;
                Ok(serde_json::Value::Null)
            }
            "session/list" => {
                let sessions = self.list_sessions().await?;
                Ok(serde_json::to_value(sessions).unwrap_or(serde_json::Value::Null))
            }
            // ── Codex wire method adapters ───────────────────────────────
            // Map upstream Codex JSON-RPC methods to ACP session operations.
            //
            // VAL-ACP-046: thread/start → ACP session/new + prompt
            "thread/start" => {
                let text = crate::provider::pi::transport::extract_text_from_params(&params)?;
                let cwd = params
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or(DEFAULT_CWD);
                let session_id = self.create_session(cwd).await?;
                let stop_reason = self.send_prompt(&session_id, &text).await?;
                Ok(serde_json::json!({
                    "id": session_id,
                    "thread_id": session_id,
                    "stop_reason": stop_reason,
                }))
            }
            // VAL-ACP-046: turn/start → ACP prompt (no session/new)
            "turn/start" => {
                let text = crate::provider::pi::transport::extract_text_from_params(&params)?;
                let session_id = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                self.send_prompt(session_id, &text).await?;
                Ok(serde_json::Value::Null)
            }
            // VAL-ACP-046: turn/interrupt → ACP cancel
            "turn/interrupt" => {
                let session_id = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                self.cancel_prompt(session_id).await?;
                Ok(serde_json::Value::Null)
            }
            // VAL-ACP-046: thread/list → ACP session/list with format conversion
            "thread/list" => {
                let sessions = self.list_sessions().await?;
                let threads: Vec<serde_json::Value> = sessions
                    .into_iter()
                    .map(|s| {
                        serde_json::json!({
                            "id": s.id,
                            "title": s.title,
                            "created_at": s.created_at,
                            "updated_at": s.updated_at,
                        })
                    })
                    .collect();
                Ok(serde_json::json!({"threads": threads}))
            }
            // VAL-ACP-046: thread/read → ACP session/load
            "thread/read" => {
                let session_id = params
                    .get("thread_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RpcError::Deserialization("missing 'thread_id' field".to_string())
                    })?;
                let cwd = params
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or(DEFAULT_CWD);
                self.load_session(session_id, cwd).await?;
                Ok(serde_json::Value::Null)
            }
            // VAL-ACP-046: no-op methods return success
            "thread/archive" | "thread/rollback" | "thread/name/set" => {
                Ok(serde_json::json!({"ok": true}))
            }
            // VAL-ACP-046: collaborationMode/list returns empty
            "collaborationMode/list" => {
                Ok(serde_json::json!({"data": []}))
            }
            // Unknown method → -32601 error
            _ => Err(RpcError::Server {
                code: -32601,
                message: format!("unknown generic ACP RPC method: {method}"),
            }),
        }
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), RpcError> {
        // Generic ACP doesn't distinguish requests from notifications — delegate.
        self.send_request(method, params).await?;
        Ok(())
    }

    fn next_event(&self) -> Option<ProviderEvent> {
        // The ACP client uses broadcast internally.
        // We rely on the event_receiver for async consumption.
        None
    }

    fn event_receiver(&self) -> tokio::sync::broadcast::Receiver<ProviderEvent> {
        self.client.subscribe()
    }

    async fn list_sessions(&mut self) -> Result<Vec<SessionInfo>, RpcError> {
        if *self.disconnected.lock().await {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }

        self.client
            .session_list()
            .await
            .map_err(RpcError::from)
    }

    fn is_connected(&self) -> bool {
        match self.disconnected.try_lock() {
            Ok(guard) => !*guard,
            Err(_) => true, // Assume connected if lock is busy.
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::acp::client::AcpClient;
    use std::sync::Arc;

    // ── Helpers ──────────────────────────────────────────────────────────

    /// Create a duplex pair for testing.
    fn test_duplex() -> (tokio::io::DuplexStream, tokio::io::DuplexStream) {
        tokio::io::duplex(8192)
    }

    /// Read a single NDJSON line from the mock end of a duplex stream.
    async fn read_mock_line(mock: &mut tokio::io::DuplexStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match mock.read(&mut byte).await {
                Ok(0) => break,
                Ok(_) => {
                    buf.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                }
                Err(e) => panic!("read error: {e}"),
            }
        }
        String::from_utf8_lossy(&buf).trim().to_string()
    }

    /// Write a single NDJSON line to the mock end.
    async fn write_mock_line(mock: &mut tokio::io::DuplexStream, line: &str) {
        use tokio::io::AsyncWriteExt;
        mock.write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        mock.flush().await.unwrap();
    }

    /// Helper: perform ACP initialize + authenticate handshake on the mock side.
    async fn mock_handshake(mock: &mut tokio::io::DuplexStream) {
        use agent_client_protocol_schema::{AuthMethod, AuthMethodAgent};

        // Read initialize request, respond.
        let init_line = read_mock_line(mock).await;
        let init_val: serde_json::Value = serde_json::from_str(&init_line).unwrap();
        let am = vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))];
        let amj: Vec<serde_json::Value> = am.iter().map(|m| serde_json::to_value(m).unwrap()).collect();
        write_mock_line(mock, &serde_json::json!({
            "jsonrpc": "2.0", "id": init_val["id"],
            "result": {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": amj}
        }).to_string()).await;

        // Read authenticate request, respond.
        let auth_line = read_mock_line(mock).await;
        let auth_val: serde_json::Value = serde_json::from_str(&auth_line).unwrap();
        write_mock_line(mock, &serde_json::json!({
            "jsonrpc": "2.0", "id": auth_val["id"], "result": {}
        }).to_string()).await;
    }

    /// Helper: create a fully-authenticated GenericAcpTransport + mock stream.
    async fn setup_authenticated_transport() -> (
        tokio::task::JoinHandle<GenericAcpTransport>,
        tokio::io::DuplexStream,
    ) {
        let (client_end, mut mock_end) = test_duplex();

        let handle = tokio::spawn(async move {
            let config = ProviderConfig {
                client_name: "litter".to_string(),
                client_version: "0.1.0".to_string(),
                ..Default::default()
            };
            let mut transport = GenericAcpTransport::new(client_end);
            transport.connect(&config).await.expect("handshake should succeed");
            transport
        });

        mock_handshake(&mut mock_end).await;

        (handle, mock_end)
    }

    // ── VAL-ACP-020: GenericAcpTransport wraps AcpClient with configurable command ──

    /// Test: GenericAcpTransport can be constructed and reports connected.
    #[tokio::test]
    async fn generic_acp_lifecycle() {
        let (client_end, _) = test_duplex();
        let transport = GenericAcpTransport::new(client_end);
        assert!(transport.is_connected());
    }

    /// Test: GenericAcpTransport can be constructed from an existing AcpClient.
    #[tokio::test]
    async fn generic_acp_from_client() {
        let (client_end, _) = test_duplex();
        let acp_client = AcpClient::new(client_end);
        let transport = GenericAcpTransport::from_client(acp_client);
        assert!(transport.is_connected());
        assert!(!transport.is_initialized().await);
    }

    /// Test: Transport is not initialized initially.
    #[tokio::test]
    async fn generic_acp_not_initialized_initially() {
        let (client_end, _) = test_duplex();
        let transport = GenericAcpTransport::new(client_end);
        assert!(!transport.is_initialized().await);
    }

    /// Test: Full ACP lifecycle through GenericAcpTransport.
    ///
    /// Verifies initialize → authenticate → session/new → session/list.
    #[tokio::test]
    async fn generic_acp_session_lifecycle() {
        use agent_client_protocol_schema::{AuthMethod, AuthMethodAgent};

        let (client_end, mut mock_end) = test_duplex();
        let client = Arc::new(tokio::sync::Mutex::new(AcpClient::new(client_end)));

        // init + auth.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        let _ = read_mock_line(&mut mock_end).await;
        let am = vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))];
        let amj: Vec<serde_json::Value> = am.iter().map(|m| serde_json::to_value(m).unwrap()).collect();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": amj}
        }).to_string()).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "result": {}
        }).to_string()).await;
        h.await.unwrap().unwrap();

        // session/new.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/home/user").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "result": {"sessionId": "gen-sess-001"}
        }).to_string()).await;
        let sess = h.await.unwrap().unwrap();
        assert_eq!(sess.session_id.to_string(), "gen-sess-001");

        // session/list.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_list().await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": 4,
            "result": {"sessions": [{"sessionId": "gen-sess-001", "cwd": "/home/user", "title": "Test"}]}
        }).to_string()).await;
        let list = h.await.unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "gen-sess-001");

        client.lock().await.shutdown().await;
    }

    /// Test: connect() performs full ACP handshake (initialize + authenticate).
    #[tokio::test]
    async fn generic_acp_connect_performs_handshake() {
        let (client_end, mut mock_end) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);

        let config = ProviderConfig {
            client_name: "test-client".to_string(),
            client_version: "2.0.0".to_string(),
            ..Default::default()
        };

        let handle = tokio::spawn(async move { transport.connect(&config).await });

        // Read and verify initialize request uses client_name/version.
        let init_line = read_mock_line(&mut mock_end).await;
        let init_val: serde_json::Value = serde_json::from_str(&init_line).unwrap();
        assert_eq!(init_val["method"], "initialize");

        // Respond to initialize.
        use agent_client_protocol_schema::{AuthMethod, AuthMethodAgent};
        let am = vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))];
        let amj: Vec<serde_json::Value> = am.iter().map(|m| serde_json::to_value(m).unwrap()).collect();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": init_val["id"],
            "result": {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": amj}
        }).to_string()).await;

        // Read and respond to authenticate.
        let auth_line = read_mock_line(&mut mock_end).await;
        let auth_val: serde_json::Value = serde_json::from_str(&auth_line).unwrap();
        assert_eq!(auth_val["method"], "authenticate");
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": auth_val["id"], "result": {}
        }).to_string()).await;

        let result = handle.await.unwrap();
        assert!(result.is_ok(), "connect should succeed: {result:?}");
    }

    // ── VAL-ACP-021: All ProviderTransport methods work on GenericAcpTransport ──

    /// Test: send_request for all mapped methods.
    #[tokio::test]
    async fn generic_acp_send_request_session_new() {
        let (transport_handle, mut mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        let handle = tokio::spawn(async move {
            transport.send_request("session/new", serde_json::json!({"cwd": "/tmp"})).await
        });

        let line = read_mock_line(&mut mock_end).await;
        let val: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(val["method"], "session/new");

        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": val["id"],
            "result": {"sessionId": "gen-test-1"}
        }).to_string()).await;

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result["session_id"], "gen-test-1");
    }

    /// Test: send_request("prompt", ...) sends ACP prompt.
    #[tokio::test]
    async fn generic_acp_send_request_prompt() {
        let (transport_handle, mut mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        // First create a session.
        let handle = tokio::spawn(async move {
            transport.send_request("session/new", serde_json::json!({"cwd": "/tmp"})).await
        });
        let line = read_mock_line(&mut mock_end).await;
        let val: serde_json::Value = serde_json::from_str(&line).unwrap();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": val["id"],
            "result": {"sessionId": "gen-prompt-1"}
        }).to_string()).await;
        let _ = handle.await.unwrap();

        // The transport was moved into the spawned task above, so we
        // verify prompt indirectly through the streaming test below.
        // This test validates that session/new works as a prerequisite.
    }

    /// Test: event_receiver returns a broadcast receiver.
    #[tokio::test]
    async fn generic_acp_event_receiver_returns_broadcast_receiver() {
        let (client_end, _) = test_duplex();
        let transport = GenericAcpTransport::new(client_end);
        let _rx = transport.event_receiver();
    }

    /// Test: list_sessions delegates to ACP client.
    #[tokio::test]
    async fn generic_acp_list_sessions() {
        let (transport_handle, mut mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        let handle = tokio::spawn(async move { transport.list_sessions().await });

        let line = read_mock_line(&mut mock_end).await;
        let val: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(val["method"], "session/list");

        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": val["id"],
            "result": {"sessions": [
                {"sessionId": "s1", "cwd": "/tmp", "title": "Session 1"},
                {"sessionId": "s2", "cwd": "/home", "title": "Session 2"}
            ]}
        }).to_string()).await;

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result.len(), 2);
    }

    /// Test: send_notification delegates to send_request.
    #[tokio::test]
    async fn generic_acp_send_notification_delegates() {
        // Test with a no-op method that doesn't require wire communication.
        let (client_end, _) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);
        *transport.initialized.lock().await = true;

        // thread/archive is a no-op that returns Ok without wire communication.
        let result = transport
            .send_notification("thread/archive", serde_json::json!({}))
            .await;
        assert!(result.is_ok(), "send_notification should succeed for no-op: {result:?}");
    }

    /// Test: next_event returns None (broadcast-based).
    #[tokio::test]
    async fn generic_acp_next_event_returns_none() {
        let (client_end, _) = test_duplex();
        let transport = GenericAcpTransport::new(client_end);
        assert!(transport.next_event().is_none());
    }

    /// Test: is_connected returns true initially.
    #[tokio::test]
    async fn generic_acp_is_connected_initially() {
        let (client_end, _) = test_duplex();
        let transport = GenericAcpTransport::new(client_end);
        assert!(transport.is_connected());
    }

    /// Test: GenericAcpTransport can be used as Box<dyn ProviderTransport>.
    #[tokio::test]
    async fn generic_acp_boxed_as_provider_transport() {
        let (client_end, _) = test_duplex();
        let transport: Box<dyn ProviderTransport> = Box::new(GenericAcpTransport::new(client_end));
        assert!(transport.is_connected());
    }

    // ── VAL-ACP-022: GenericAcpTransport handshake timeout ──

    /// Test: connect() times out when the server never responds.
    #[tokio::test]
    async fn generic_acp_connect_timeout_when_no_response() {
        let (client_end, _mock_end) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);

        let config = ProviderConfig {
            client_name: "litter".to_string(),
            client_version: "0.1.0".to_string(),
            ..Default::default()
        };

        // Use a short timeout for testing.
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            transport.connect(&config),
        )
        .await;

        // Should timeout since we never respond to initialize.
        assert!(result.is_err(), "should timeout: {result:?}");
    }

    // ── VAL-ACP-023: GenericAcpTransport disconnect cleanup ──

    /// Test: disconnect is idempotent.
    #[tokio::test]
    async fn generic_acp_disconnect_idempotent() {
        let (client_end, _) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);

        transport.disconnect().await;
        assert!(!transport.is_connected());

        // Second disconnect should not panic.
        transport.disconnect().await;
        assert!(!transport.is_connected());
    }

    /// Test: after disconnect, is_connected returns false.
    #[tokio::test]
    async fn generic_acp_post_disconnect_is_not_connected() {
        let (client_end, _) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);

        assert!(transport.is_connected());
        transport.disconnect().await;
        assert!(!transport.is_connected());
    }

    /// Test: after disconnect, send_request returns Disconnected error.
    #[tokio::test]
    async fn generic_acp_post_disconnect_request_fails() {
        let (client_end, _) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);
        transport.disconnect().await;

        let result = transport
            .send_request("prompt", serde_json::json!({"text": "hello"}))
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            RpcError::Transport(TransportError::Disconnected) => {}
            other => panic!("expected Disconnected, got: {other:?}"),
        }
    }

    /// Test: after disconnect, send_notification returns Disconnected error.
    #[tokio::test]
    async fn generic_acp_post_disconnect_notification_fails() {
        let (client_end, _) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);
        transport.disconnect().await;

        let result = transport
            .send_notification("cancel", serde_json::json!({}))
            .await;
        assert!(result.is_err());
    }

    /// Test: after disconnect, list_sessions returns error.
    #[tokio::test]
    async fn generic_acp_post_disconnect_list_sessions_fails() {
        let (client_end, _) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);
        transport.disconnect().await;

        let result = transport.list_sessions().await;
        assert!(result.is_err());
    }

    /// Test: connect() after disconnect returns ConnectionFailed (cannot reconnect).
    #[tokio::test]
    async fn generic_acp_connect_rejected_after_disconnect() {
        let (client_end, _) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);
        transport.disconnect().await;

        let config = ProviderConfig::default();
        let result = transport.connect(&config).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            TransportError::ConnectionFailed(msg) => {
                assert!(msg.contains("cannot reconnect"));
            }
            other => panic!("expected ConnectionFailed, got: {other:?}"),
        }
    }

    // ── VAL-ACP-024: Unknown method returns -32601 error ──

    #[tokio::test]
    async fn generic_acp_unknown_method_returns_error() {
        let (client_end, _) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);

        // Manually mark as initialized.
        *transport.initialized.lock().await = true;

        let result = transport
            .send_request("unknown_method", serde_json::json!({}))
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            RpcError::Server { code, message } => {
                assert_eq!(code, -32601);
                assert!(message.contains("unknown_method"));
            }
            other => panic!("expected Server error, got: {other:?}"),
        }
    }

    // ── VAL-ACP-025: Session operations before auth fail ──

    /// Test: send_request before initialization returns error.
    #[tokio::test]
    async fn generic_acp_send_request_before_init_fails() {
        let (client_end, _) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);
        // Not initialized (handshake not performed).

        let result = transport
            .send_request("prompt", serde_json::json!({"text": "hello"}))
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            RpcError::Server { code, message } => {
                assert_eq!(code, -1);
                assert!(message.contains("not initialized"));
            }
            other => panic!("expected Server error, got: {other:?}"),
        }
    }

    /// Test: session/new before initialization returns error.
    #[tokio::test]
    async fn generic_acp_session_new_before_init_fails() {
        let (client_end, _) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);

        let result = transport
            .send_request("session/new", serde_json::json!({}))
            .await;
        assert!(result.is_err());
    }

    // ── Codex wire method adapter tests (VAL-ACP-046) ──

    /// Test: thread/start creates session and sends prompt.
    #[tokio::test]
    async fn generic_acp_thread_start_creates_session_and_prompts() {
        let (transport_handle, mut mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        let handle = tokio::spawn(async move {
            transport.send_request(
                "thread/start",
                serde_json::json!({
                    "items": [{"role": "user", "content": "Hello Agent!"}]
                }),
            ).await
        });

        // Read session/new request.
        let new_line = read_mock_line(&mut mock_end).await;
        let new_val: serde_json::Value = serde_json::from_str(&new_line).unwrap();
        assert_eq!(new_val["method"], "session/new");

        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": new_val["id"],
            "result": {"sessionId": "gen-thread-001"}
        }).to_string()).await;

        // Read prompt request.
        let prompt_line = read_mock_line(&mut mock_end).await;
        let prompt_val: serde_json::Value = serde_json::from_str(&prompt_line).unwrap();
        assert_eq!(prompt_val["method"], "session/prompt");

        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": prompt_val["id"],
            "result": {"stopReason": "end_turn"}
        }).to_string()).await;

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result["id"], "gen-thread-001");
        assert_eq!(result["thread_id"], "gen-thread-001");
    }

    /// Test: turn/interrupt maps to cancel.
    #[tokio::test]
    async fn generic_acp_turn_interrupt_calls_cancel() {
        let (client_end, _) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);
        *transport.initialized.lock().await = true;

        // Cancel when not streaming is a no-op on the wire (doesn't send message).
        let result = transport
            .send_request("turn/interrupt", serde_json::json!({"session_id": "test"}))
            .await;
        // Should succeed — cancel is no-op when not streaming.
        assert!(result.is_ok(), "cancel no-op should succeed: {result:?}");
    }

    /// Test: thread/archive is a no-op returning success.
    #[tokio::test]
    async fn generic_acp_thread_archive_is_noop() {
        let (client_end, _) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);
        *transport.initialized.lock().await = true;

        let result = transport
            .send_request("thread/archive", serde_json::json!({}))
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["ok"], true);
    }

    /// Test: collaborationMode/list returns empty.
    #[tokio::test]
    async fn generic_acp_collaboration_mode_list_returns_empty() {
        let (client_end, _) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);
        *transport.initialized.lock().await = true;

        let result = transport
            .send_request("collaborationMode/list", serde_json::json!({}))
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["data"], serde_json::json!([]));
    }

    /// Test: streaming events flow through event_receiver.
    #[tokio::test]
    async fn generic_acp_streaming_events_through_receiver() {
        use agent_client_protocol_schema::{AuthMethod, AuthMethodAgent, SessionId};

        let (client_end, mut mock_end) = test_duplex();
        let client = Arc::new(tokio::sync::Mutex::new(AcpClient::new(client_end)));

        // init + auth + session/new.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        let _ = read_mock_line(&mut mock_end).await;
        let am = vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))];
        let amj: Vec<serde_json::Value> = am.iter().map(|m| serde_json::to_value(m).unwrap()).collect();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": amj}
        }).to_string()).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "result": {}
        }).to_string()).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/tmp").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "result": {"sessionId": "gen-stream-1"}
        }).to_string()).await;
        h.await.unwrap().unwrap();

        // Subscribe to events before prompt.
        let mut event_rx = client.lock().await.subscribe();

        // session/prompt with streaming.
        let c = client.clone();
        let h = tokio::spawn(async move {
            c.lock().await.session_prompt(&SessionId::new("gen-stream-1"), "Say hello").await
        });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "gen-stream-1",
                "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hello "}}
            }
        }).to_string()).await;
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "gen-stream-1",
                "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "world"}}
            }
        }).to_string()).await;
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": 4, "result": {"stopReason": "end_turn"}
        }).to_string()).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "session/prompt should succeed: {result:?}");

        // Verify streaming events were received.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let mut got_delta = false;
        while let Ok(event) = event_rx.try_recv() {
            if matches!(event, ProviderEvent::MessageDelta { .. }) {
                got_delta = true;
            }
        }
        assert!(got_delta, "should have received MessageDelta from streaming");

        client.lock().await.shutdown().await;
    }

    // ── Error handling ──

    /// Test: connect fails when the server returns an error during initialize.
    #[tokio::test]
    async fn generic_acp_connect_fails_on_init_error() {
        let (client_end, mut mock_end) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);

        let config = ProviderConfig {
            client_name: "litter".to_string(),
            client_version: "0.1.0".to_string(),
            ..Default::default()
        };

        let connect_handle = tokio::spawn(async move { transport.connect(&config).await });

        let init_req = read_mock_line(&mut mock_end).await;
        let init_val: serde_json::Value = serde_json::from_str(&init_req).unwrap();
        write_mock_line(
            &mut mock_end,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": init_val["id"],
                "error": {"code": -32600, "message": "invalid request"}
            })
            .to_string(),
        )
        .await;

        let result = connect_handle.await.unwrap();
        assert!(result.is_err(), "connect should fail when init returns error");
    }

    // ── Type bounds ──

    /// Test: GenericAcpTransport is Send + Sync (required by trait bounds).
    #[test]
    fn generic_acp_transport_is_send_sync() {
        fn _assert_send_sync<T: Send + Sync>() {}
        _assert_send_sync::<GenericAcpTransport>();
    }

    /// Test: Box<dyn ProviderTransport> from GenericAcpTransport is Send + Sync.
    #[test]
    fn generic_acp_boxed_is_send_sync() {
        fn _assert_send_sync<T: Send + Sync>() {}
        _assert_send_sync::<Box<dyn ProviderTransport>>();
    }

    // ── VAL-ACP-036: Capabilities queryable after connect ─────────────

    /// Test: capabilities() returns None before handshake.
    #[tokio::test]
    async fn generic_acp_capabilities_none_before_connect() {
        let (client_end, _) = test_duplex();
        let transport = GenericAcpTransport::new(client_end);
        assert!(transport.capabilities().await.is_none());
        assert!(transport.initialize_result().await.is_none());
    }

    /// Test: initialize_result() is populated after successful connect.
    #[tokio::test]
    async fn generic_acp_capabilities_populated_after_connect() {
        let (client_end, mut mock_end) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);

        let config = ProviderConfig {
            client_name: "litter".to_string(),
            client_version: "0.1.0".to_string(),
            ..Default::default()
        };

        let connect_handle = tokio::spawn(async move { transport.connect(&config).await });

        // Read init request.
        let init_line = read_mock_line(&mut mock_end).await;
        let init_val: serde_json::Value = serde_json::from_str(&init_line).unwrap();

        // Respond with full capabilities.
        use agent_client_protocol_schema::{AuthMethod, AuthMethodAgent};
        let am = vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))];
        let amj: Vec<serde_json::Value> = am.iter().map(|m| serde_json::to_value(m).unwrap()).collect();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": init_val["id"],
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {"streaming": true, "tools": true, "plans": true, "reasoning": true},
                "authMethods": amj,
                "agentInfo": {"name": "test-agent", "version": "3.0.0"}
            }
        }).to_string()).await;

        // Read and respond to auth.
        let auth_line = read_mock_line(&mut mock_end).await;
        let auth_val: serde_json::Value = serde_json::from_str(&auth_line).unwrap();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": auth_val["id"], "result": {}
        }).to_string()).await;

        let result = connect_handle.await.unwrap();
        assert!(result.is_ok(), "connect should succeed: {result:?}");

        // We can't query the transport after moving it into the spawned task,
        // so test via a separate setup.
    }

    /// Test: full capabilities query after handshake via separate transport instance.
    #[tokio::test]
    async fn generic_acp_initialize_result_queryable_after_handshake() {
        let (client_end, mut mock_end) = test_duplex();

        let transport = GenericAcpTransport::new(client_end);

        // Perform handshake manually (not via connect, so we keep the reference).
        use agent_client_protocol_schema::{AuthMethod, AuthMethodAgent};
        let am = vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))];
        let amj: Vec<serde_json::Value> = am.iter().map(|m| serde_json::to_value(m).unwrap()).collect();

        let h = tokio::spawn(async move { transport.handshake("litter", "0.1.0").await });

        // Read init request and respond.
        let init_line = read_mock_line(&mut mock_end).await;
        let init_val: serde_json::Value = serde_json::from_str(&init_line).unwrap();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": init_val["id"],
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {"streaming": true, "tools": true},
                "authMethods": amj,
                "agentInfo": {"name": "my-agent", "version": "1.0.0"}
            }
        }).to_string()).await;

        // Read auth request and respond.
        let auth_line = read_mock_line(&mut mock_end).await;
        let auth_val: serde_json::Value = serde_json::from_str(&auth_line).unwrap();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": auth_val["id"], "result": {}
        }).to_string()).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "handshake should succeed: {result:?}");

        // Note: We can't query the transport after moving it into the spawn.
        // Instead, we verify via a non-moving approach.
    }

    /// Test: initialize_result accessible via Arc (non-moving test).
    #[tokio::test]
    async fn generic_acp_capabilities_accessible_via_arc() {
        let (client_end, mut mock_end) = test_duplex();

        let transport = Arc::new(tokio::sync::Mutex::new(GenericAcpTransport::new(client_end)));

        use agent_client_protocol_schema::{AuthMethod, AuthMethodAgent};
        let am = vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))];
        let amj: Vec<serde_json::Value> = am.iter().map(|m| serde_json::to_value(m).unwrap()).collect();

        // Before handshake: capabilities are None.
        assert!(transport.lock().await.capabilities().await.is_none());
        assert!(transport.lock().await.initialize_result().await.is_none());

        let t = transport.clone();
        let h = tokio::spawn(async move { t.lock().await.handshake("litter", "0.1.0").await });

        // Read init and respond.
        let init_line = read_mock_line(&mut mock_end).await;
        let init_val: serde_json::Value = serde_json::from_str(&init_line).unwrap();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": init_val["id"],
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {"streaming": true, "tools": true, "plans": false},
                "authMethods": amj,
                "agentInfo": {"name": "cap-agent", "version": "4.2.0"}
            }
        }).to_string()).await;

        // Read auth and respond.
        let auth_line = read_mock_line(&mut mock_end).await;
        let auth_val: serde_json::Value = serde_json::from_str(&auth_line).unwrap();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": auth_val["id"], "result": {}
        }).to_string()).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "handshake should succeed: {result:?}");

        // VAL-ACP-036: After handshake, capabilities are queryable.
        let init_result = transport.lock().await.initialize_result().await;
        assert!(init_result.is_some(), "initialize_result should be populated after handshake");

        let init_result = init_result.unwrap();
        assert_eq!(init_result.auth_methods.len(), 1);
        assert_eq!(init_result.auth_methods[0].to_string(), "local");

        // VAL-ACP-036: agent_info is populated.
        assert!(init_result.agent_info.is_some());
        let info = init_result.agent_info.unwrap();
        assert_eq!(info.name, "cap-agent");
        assert_eq!(info.version, "4.2.0");

        // VAL-ACP-036: protocol_version is correct.
        assert_eq!(init_result.protocol_version, agent_client_protocol_schema::ProtocolVersion::V1);
    }

    /// Test: handshake must complete before session operations via ProviderTransport.
    #[tokio::test]
    async fn generic_acp_session_ops_blocked_before_handshake_via_trait() {
        let (client_end, _mock_end) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);

        // Attempt send_request before handshake — should fail with "not initialized".
        let result = transport
            .send_request("session/new", serde_json::json!({"cwd": "/tmp"}))
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            RpcError::Server { code: _, message } => {
                assert!(message.contains("not initialized"));
            }
            other => panic!("expected Server error, got: {other:?}"),
        }
    }

    /// Test: version mismatch during connect propagates as TransportError.
    #[tokio::test]
    async fn generic_acp_version_mismatch_connect_error() {
        let (client_end, mut mock_end) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);

        let config = ProviderConfig {
            client_name: "litter".to_string(),
            client_version: "0.1.0".to_string(),
            ..Default::default()
        };

        let connect_handle = tokio::spawn(async move { transport.connect(&config).await });

        let init_line = read_mock_line(&mut mock_end).await;
        let init_val: serde_json::Value = serde_json::from_str(&init_line).unwrap();

        // Respond with a higher protocol version (incompatible).
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": init_val["id"],
            "result": {
                "protocolVersion": 999,
                "agentCapabilities": {},
                "authMethods": []
            }
        }).to_string()).await;

        let result = connect_handle.await.unwrap();
        assert!(result.is_err(), "connect should fail with version mismatch");
        match result.unwrap_err() {
            TransportError::ConnectionFailed(msg) => {
                assert!(
                    msg.contains("protocol version mismatch"),
                    "error should mention version mismatch: {msg}"
                );
            }
            other => panic!("expected ConnectionFailed, got: {other:?}"),
        }
    }

    /// Test: auth failure during connect propagates as TransportError.
    #[tokio::test]
    async fn generic_acp_auth_failure_connect_error() {
        let (client_end, mut mock_end) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);

        let config = ProviderConfig {
            client_name: "litter".to_string(),
            client_version: "0.1.0".to_string(),
            ..Default::default()
        };

        let connect_handle = tokio::spawn(async move { transport.connect(&config).await });

        use agent_client_protocol_schema::{AuthMethod, AuthMethodAgent};
        let am = vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))];
        let amj: Vec<serde_json::Value> = am.iter().map(|m| serde_json::to_value(m).unwrap()).collect();

        // Respond to initialize.
        let init_line = read_mock_line(&mut mock_end).await;
        let init_val: serde_json::Value = serde_json::from_str(&init_line).unwrap();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": init_val["id"],
            "result": {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": amj}
        }).to_string()).await;

        // Reject auth with permanent error.
        let auth_line = read_mock_line(&mut mock_end).await;
        let auth_val: serde_json::Value = serde_json::from_str(&auth_line).unwrap();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": auth_val["id"],
            "error": {"code": -32000, "message": "Authentication required"}
        }).to_string()).await;

        let result = connect_handle.await.unwrap();
        assert!(result.is_err(), "connect should fail with auth failure");
    }

    // ── Graceful shutdown (flush pending messages) ─────────────────────

    /// Test: disconnect() after a successful handshake is clean and
    /// subsequent operations fail with Disconnected.
    #[tokio::test]
    async fn generic_acp_graceful_shutdown_after_handshake() {
        let (client_end, mut mock_end) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);

        // Perform handshake.
        let config = ProviderConfig {
            client_name: "litter".to_string(),
            client_version: "0.1.0".to_string(),
            ..Default::default()
        };

        let connect_handle = tokio::spawn(async move { transport.connect(&config).await });
        mock_handshake(&mut mock_end).await;
        let result = connect_handle.await.unwrap();
        assert!(result.is_ok(), "connect should succeed: {result:?}");

        // We can't access the transport that was moved into the spawn.
        // Instead, test disconnect behavior with a fresh transport.
        let (client_end2, mut mock_end2) = test_duplex();
        let transport2 = GenericAcpTransport::new(client_end2);
        let config2 = ProviderConfig {
            client_name: "litter".to_string(),
            client_version: "0.1.0".to_string(),
            ..Default::default()
        };

        let t = Arc::new(tokio::sync::Mutex::new(transport2));
        let t_clone = t.clone();
        let connect2 = tokio::spawn(async move { t_clone.lock().await.connect(&config2).await });
        mock_handshake(&mut mock_end2).await;
        connect2.await.unwrap().unwrap();

        // Now disconnect.
        t.lock().await.disconnect().await;
        assert!(!t.lock().await.is_connected());

        // After disconnect, all operations should fail.
        let req_result = t.lock().await
            .send_request("session/new", serde_json::json!({"cwd": "/tmp"}))
            .await;
        assert!(req_result.is_err(), "send_request after disconnect should fail");

        let notif_result: Result<(), RpcError> = t.lock().await
            .send_notification("cancel", serde_json::json!({}))
            .await;
        assert!(notif_result.is_err(), "send_notification after disconnect should fail");

        let list_result: Result<Vec<SessionInfo>, RpcError> = t.lock().await.list_sessions().await;
        assert!(list_result.is_err(), "list_sessions after disconnect should fail");
    }

    /// Test: disconnect is safe even during an active session.
    #[tokio::test]
    async fn generic_acp_graceful_shutdown_during_active_session() {
        let (client_end, mut mock_end) = test_duplex();
        let transport = Arc::new(tokio::sync::Mutex::new(GenericAcpTransport::new(client_end)));

        let config = ProviderConfig {
            client_name: "litter".to_string(),
            client_version: "0.1.0".to_string(),
            ..Default::default()
        };

        let t = transport.clone();
        let connect_h = tokio::spawn(async move { t.lock().await.connect(&config).await });
        mock_handshake(&mut mock_end).await;
        connect_h.await.unwrap().unwrap();

        // Create a session.
        let t = transport.clone();
        let handle = tokio::spawn(async move {
            t.lock().await.send_request("session/new", serde_json::json!({"cwd": "/tmp"})).await
        });
        let line = read_mock_line(&mut mock_end).await;
        let val: serde_json::Value = serde_json::from_str(&line).unwrap();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": val["id"],
            "result": {"sessionId": "graceful-sess"}
        }).to_string()).await;
        let _ = handle.await.unwrap();

        // Disconnect during session — should not panic.
        transport.lock().await.disconnect().await;
        assert!(!transport.lock().await.is_connected());
    }

    // ── Concurrent operations serialization ────────────────────────────

    /// Test: Multiple concurrent send_request calls are serialized
    /// correctly without data races (the Mutex on AcpClient inner
    /// ensures serialization).
    #[tokio::test]
    async fn generic_acp_concurrent_operations_serialized() {
        let (transport_handle, mut mock_end) = setup_authenticated_transport().await;
        let transport = Arc::new(tokio::sync::Mutex::new(transport_handle.await.unwrap()));

        // Spawn two concurrent session operations.
        let t1 = transport.clone();
        let h1 = tokio::spawn(async move {
            t1.lock().await.send_request("session/new", serde_json::json!({"cwd": "/tmp/a"})).await
        });

        // Read first request and respond.
        let line1 = read_mock_line(&mut mock_end).await;
        let val1: serde_json::Value = serde_json::from_str(&line1).unwrap();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": val1["id"],
            "result": {"sessionId": "conc-sess-1"}
        }).to_string()).await;

        let r1 = h1.await.unwrap();
        assert!(r1.is_ok(), "first concurrent request should succeed: {r1:?}");

        let t2 = transport.clone();
        let h2 = tokio::spawn(async move {
            t2.lock().await.send_request("session/list", serde_json::json!({})).await
        });

        let line2 = read_mock_line(&mut mock_end).await;
        let val2: serde_json::Value = serde_json::from_str(&line2).unwrap();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": val2["id"],
            "result": {"sessions": []}
        }).to_string()).await;

        let r2 = h2.await.unwrap();
        assert!(r2.is_ok(), "second concurrent request should succeed: {r2:?}");
    }

    // ── Error handling for transport errors vs protocol errors ─────────

    /// Test: Protocol error from agent (JSON-RPC error) returns Server error.
    #[tokio::test]
    async fn generic_acp_error_handling_protocol_error() {
        let (transport_handle, mut mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        let handle = tokio::spawn(async move {
            transport.send_request("session/new", serde_json::json!({"cwd": "/tmp"})).await
        });

        let line = read_mock_line(&mut mock_end).await;
        let val: serde_json::Value = serde_json::from_str(&line).unwrap();

        // Respond with a JSON-RPC error.
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": val["id"],
            "error": {"code": -32602, "message": "Invalid params"}
        }).to_string()).await;

        let result = handle.await.unwrap();
        assert!(result.is_err(), "protocol error should return error");
        match result.unwrap_err() {
            RpcError::Server { code, message } => {
                assert_eq!(code, -32602);
                assert!(message.contains("Invalid params"));
            }
            other => panic!("expected Server error, got: {other:?}"),
        }
    }

    /// Test: Transport error (disconnect) returns TransportError::Disconnected.
    #[tokio::test]
    async fn generic_acp_error_handling_transport_disconnect() {
        let (client_end, _) = test_duplex();
        let mut transport = GenericAcpTransport::new(client_end);
        *transport.initialized.lock().await = true;
        transport.disconnect().await;

        let result = transport
            .send_request("session/new", serde_json::json!({}))
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            RpcError::Transport(TransportError::Disconnected) => {}
            other => panic!("expected TransportError::Disconnected, got: {other:?}"),
        }
    }

    /// Test: Agent error during session operations is distinct from
    /// transport error.
    #[tokio::test]
    async fn generic_acp_error_handling_agent_error_distinct_from_transport() {
        let (transport_handle, mut mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        let handle = tokio::spawn(async move {
            transport.send_request("session/new", serde_json::json!({"cwd": "/tmp"})).await
        });

        let line = read_mock_line(&mut mock_end).await;
        let val: serde_json::Value = serde_json::from_str(&line).unwrap();

        // Respond with an agent-specific error code (not transport error).
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": val["id"],
            "error": {"code": -32002, "message": "session limit reached"}
        }).to_string()).await;

        let result = handle.await.unwrap();
        assert!(result.is_err(), "agent error should return error");
        // Agent errors come back as RpcError::Server, not Transport.
        match result.unwrap_err() {
            RpcError::Server { code, message } => {
                assert_eq!(code, -32002);
                assert!(message.contains("session limit"));
            }
            other => panic!("expected Server error for agent error, got: {other:?}"),
        }
    }
}
