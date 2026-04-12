use super::*;

const MOBILE_IPC_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[allow(clippy::type_complexity)]
pub(super) fn make_accept_unknown_host_callback(
    accept_unknown_host: bool,
) -> Box<
    dyn Fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> + Send + Sync,
> {
    Box::new(move |_fingerprint| Box::pin(async move { accept_unknown_host }))
}

pub(super) fn ipc_reconnect_policy() -> ReconnectPolicy {
    ReconnectPolicy {
        initial_delay: std::time::Duration::ZERO,
        ..ReconnectPolicy::default()
    }
}

pub(super) async fn resolve_remote_ipc_socket_path_for_session(
    ssh_client: &Arc<SshClient>,
    server_id: &str,
    ipc_socket_path_override: Option<&str>,
) -> Option<String> {
    match ssh_client
        .remote_ipc_socket_if_present(ipc_socket_path_override)
        .await
    {
        Ok(Some(socket_path)) => {
            info!(
                "IPC socket detected server={} path={}",
                server_id, socket_path
            );
            Some(socket_path)
        }
        Ok(None) => None,
        Err(error) => {
            warn!(
                "MobileClient: failed to probe IPC socket for {}: {}",
                server_id, error
            );
            None
        }
    }
}

pub(super) async fn attach_ipc_client_via_ssh(
    ssh_client: &Arc<SshClient>,
    server_id: &str,
    socket_path: &str,
) -> Option<IpcClient> {
    let ipc_config = IpcClientConfig {
        socket_path: std::path::PathBuf::from(socket_path),
        client_type: "mobile".to_string(),
        request_timeout: MOBILE_IPC_REQUEST_TIMEOUT,
    };
    match ssh_client.open_streamlocal(socket_path).await {
        Ok(stream) => match IpcClient::connect_with_stream(&ipc_config, stream).await {
            Ok(client) => {
                info!(
                    "IPC attached server={} transport=direct-streamlocal path={}",
                    server_id, socket_path
                );
                Some(client)
            }
            Err(error) => {
                warn!(
                    "MobileClient: failed to attach IPC for {} at {}: {}",
                    server_id, socket_path, error
                );
                None
            }
        },
        Err(error) => {
            warn!(
                "MobileClient: failed to open IPC streamlocal for {} at {}: {}",
                server_id, socket_path, error
            );
            None
        }
    }
}

pub(super) async fn attach_ipc_client_via_tcp_bridge(
    _ssh_client: &Arc<SshClient>,
    _server_id: &str,
    _socket_path: &str,
) -> Option<(IpcClient, Option<u32>)> {
    None
}

pub(super) async fn attach_ipc_client_for_remote_session(
    ssh_client: &Arc<SshClient>,
    server_id: &str,
    ipc_socket_path_override: Option<&str>,
) -> (Option<IpcClient>, Option<u32>) {
    let Some(socket_path) =
        resolve_remote_ipc_socket_path_for_session(ssh_client, server_id, ipc_socket_path_override)
            .await
    else {
        return (None, None);
    };

    if let Some(client) = attach_ipc_client_via_ssh(ssh_client, server_id, &socket_path).await {
        return (Some(client), None);
    }

    if let Some((client, bridge_pid)) =
        attach_ipc_client_via_tcp_bridge(ssh_client, server_id, &socket_path).await
    {
        info!(
            "IPC attached server={} transport=tcp-bridge bridge_pid={:?}",
            server_id, bridge_pid
        );
        return (Some(client), bridge_pid);
    }

    (None, None)
}
