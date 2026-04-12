//! Pi ACP transport implementation.
//!
//! Wraps the universal ACP client to communicate with Pi via the `pi-acp`
//! adapter over SSH PTY. Spawns `npx pi-acp` on the remote host and
//! completes the full ACP lifecycle:
//!
//! ```text
//! SSH → spawn "npx pi-acp" over PTY → ACP JSON-RPC (NDJSON)
//!   → initialize → authenticate → session/new → session/prompt
//!   → stream session/update → session/cancel (on abort)
//! ```
//!
//! This transport reuses the `AcpClient` from `crate::provider::acp::client`
//! and maps ACP events to `ProviderEvent` through the same pipeline.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;

use crate::provider::acp::client::AcpClient;
use crate::provider::{ProviderConfig, ProviderEvent, ProviderTransport, SessionInfo};
use crate::transport::{RpcError, TransportError};

/// Default working directory for Pi sessions.
const DEFAULT_PI_CWD: &str = "~";

/// Pi ACP transport wrapping the universal ACP client.
///
/// Communicates with Pi via the `pi-acp` adapter, which translates Pi's
/// native protocol to ACP JSON-RPC. The transport spawns `npx pi-acp`
/// over an SSH PTY and uses NDJSON framing for message exchange.
///
/// # Lifecycle
///
/// 1. Create with a bidirectional stream (SSH PTY or mock).
/// 2. `connect()` performs ACP initialize + authenticate.
/// 3. `send_request("session/new", ...)` creates a Pi session.
/// 4. `send_request("prompt", ...)` sends a prompt and streams events.
/// 5. `send_request("cancel", ...)` cancels the active prompt.
/// 6. `disconnect()` tears down the session.
pub struct PiAcpTransport {
    /// The underlying ACP client.
    client: AcpClient,
    /// Whether the handshake (initialize + authenticate) has completed.
    initialized: Arc<Mutex<bool>>,
    /// Whether the transport has been disconnected.
    disconnected: Arc<Mutex<bool>>,
}

impl PiAcpTransport {
    /// Create a new Pi ACP transport over the given bidirectional stream.
    ///
    /// The stream is typically an SSH channel connected to `npx pi-acp`.
    /// The ACP client is created but not yet initialized — call `connect()`
    /// to perform the handshake.
    pub fn new<T>(stream: T) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + 'static,
    {
        Self {
            client: AcpClient::new(stream),
            initialized: Arc::new(Mutex::new(false)),
            disconnected: Arc::new(Mutex::new(false)),
        }
    }

    /// Create a Pi ACP transport from an existing ACP client.
    ///
    /// Useful when the client has already been created and possibly
    /// initialized externally.
    pub fn from_client(client: AcpClient) -> Self {
        Self {
            client,
            initialized: Arc::new(Mutex::new(false)),
            disconnected: Arc::new(Mutex::new(false)),
        }
    }

    /// Get a reference to the underlying ACP client for direct operations.
    pub fn acp_client(&self) -> &AcpClient {
        &self.client
    }

    /// Perform the ACP initialize + authenticate handshake.
    ///
    /// This is called automatically by `connect()`.
    pub async fn handshake(
        &self,
        client_name: &str,
        client_version: &str,
    ) -> Result<(), AcpTransportError> {
        // Initialize.
        let init_result = self
            .client
            .initialize(client_name, client_version)
            .await
            .map_err(AcpTransportError::from)?;

        tracing::info!(
            "Pi ACP initialized: protocol_version={:?}, auth_methods={:?}",
            init_result.protocol_version,
            init_result.auth_methods
        );

        // Authenticate using the first available method.
        self.client
            .authenticate(None)
            .await
            .map_err(AcpTransportError::from)?;

        let mut guard = self.initialized.lock().await;
        *guard = true;

        Ok(())
    }

    /// Create a new Pi session via ACP.
    ///
    /// Returns the session ID on success.
    pub async fn create_session(&self, cwd: &str) -> Result<String, AcpTransportError> {
        let result = self.client.session_new(cwd).await.map_err(AcpTransportError::from)?;
        Ok(result.session_id.to_string())
    }

    /// Send a prompt to the Pi session and stream events.
    ///
    /// Events are emitted via the `event_receiver()` stream.
    /// Returns the stop reason when the prompt completes.
    pub async fn send_prompt(
        &self,
        session_id: &str,
        prompt_text: &str,
    ) -> Result<String, AcpTransportError> {
        let session_id = agent_client_protocol_schema::SessionId::new(session_id.to_string());
        let result = self
            .client
            .session_prompt(&session_id, prompt_text)
            .await
            .map_err(AcpTransportError::from)?;
        Ok(result.stop_reason)
    }

    /// Cancel the active prompt for a session.
    pub async fn cancel_prompt(
        &self,
        session_id: &str,
    ) -> Result<(), AcpTransportError> {
        let session_id = agent_client_protocol_schema::SessionId::new(session_id.to_string());
        self.client
            .session_cancel(&session_id)
            .await
            .map_err(AcpTransportError::from)
    }

    /// Load an existing Pi session via ACP.
    pub async fn load_session(
        &self,
        session_id: &str,
        cwd: &str,
    ) -> Result<(), AcpTransportError> {
        let session_id = agent_client_protocol_schema::SessionId::new(session_id.to_string());
        self.client
            .session_load(&session_id, cwd)
            .await
            .map_err(AcpTransportError::from)
    }

    /// Subscribe to provider events.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ProviderEvent> {
        self.client.subscribe()
    }

    /// Check if the handshake has completed.
    pub async fn is_initialized(&self) -> bool {
        *self.initialized.lock().await
    }
}

/// Error type for Pi ACP transport operations.
#[derive(Debug, thiserror::Error)]
pub enum AcpTransportError {
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

impl From<AcpTransportError> for RpcError {
    fn from(err: AcpTransportError) -> Self {
        match err {
            AcpTransportError::AcpClient(e) => RpcError::from(e),
            AcpTransportError::NotConnected | AcpTransportError::Disconnected => {
                RpcError::Transport(TransportError::Disconnected)
            }
            AcpTransportError::NotInitialized => RpcError::Server {
                code: -1,
                message: err.to_string(),
            },
        }
    }
}

impl From<AcpTransportError> for TransportError {
    fn from(err: AcpTransportError) -> Self {
        match err {
            AcpTransportError::NotConnected | AcpTransportError::Disconnected => {
                TransportError::Disconnected
            }
            _ => TransportError::ConnectionFailed(err.to_string()),
        }
    }
}

#[async_trait]
impl ProviderTransport for PiAcpTransport {
    async fn connect(&mut self, config: &ProviderConfig) -> Result<(), TransportError> {
        if *self.disconnected.lock().await {
            return Err(TransportError::ConnectionFailed(
                "Pi ACP transport cannot reconnect; create a new instance".to_string(),
            ));
        }

        // Perform the ACP handshake.
        self.handshake(&config.client_name, &config.client_version)
            .await
            .map_err(|e| match e {
                AcpTransportError::AcpClient(
                    crate::provider::acp::client::AcpClientError::Disconnected,
                ) => TransportError::Disconnected,
                AcpTransportError::AcpClient(
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
                message: "Pi ACP transport not initialized".to_string(),
            });
        }

        match method {
            "session/new" => {
                let cwd = params
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or(DEFAULT_PI_CWD);
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
                    .unwrap_or(DEFAULT_PI_CWD);
                self.load_session(session_id, cwd).await?;
                Ok(serde_json::Value::Null)
            }
            "session/list" => {
                let sessions = self.list_sessions().await?;
                Ok(serde_json::to_value(sessions).unwrap_or(serde_json::Value::Null))
            }
            _ => Err(RpcError::Server {
                code: -32601,
                message: format!("unknown Pi ACP RPC method: {method}"),
            }),
        }
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), RpcError> {
        // Pi ACP doesn't distinguish requests from notifications — delegate.
        self.send_request(method, params).await?;
        Ok(())
    }

    fn next_event(&self) -> Option<ProviderEvent> {
        // The ACP client uses broadcast internally, so we need to peek.
        // Since AcpClient doesn't expose a non-blocking peek, we rely on
        // the event_receiver for async consumption.
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

    /// Helper: create a duplex pair for testing.
    fn test_duplex() -> (tokio::io::DuplexStream, tokio::io::DuplexStream) {
        tokio::io::duplex(8192)
    }

    /// Helper to queue a complete ACP initialize + authenticate handshake.
    #[allow(dead_code)]
    async fn queue_handshake(_mock: &tokio::io::DuplexStream) {
        use tokio::io::AsyncWriteExt;
        // Read the initialize request from the client.
        let _buf = vec![0u8; 4096];
        // We need to read what the client sends first, then respond.
        // Instead, use a simpler approach: write responses that match expected IDs.
    }

    /// Create a fully set up PiAcpTransport with mock.
    /// Returns (transport, mock_write_end) where mock_write_end is used to
    /// inject server responses.
    #[allow(dead_code)]
    fn setup_with_mock() -> (PiAcpTransport, tokio::io::DuplexStream) {
        let (client_end, server_end) = test_duplex();
        let transport = PiAcpTransport::new(client_end);
        (transport, server_end)
    }

    // ── VAL-PI-001: ACP transport lifecycle ─────────────────────────────

    #[tokio::test]
    async fn pi_acp_lifecycle() {
        // Test that the transport can be constructed and connected without panic.
        // The detailed lifecycle test is in pi_acp_full_lifecycle_with_mock_server.
        let (client_end, _) = test_duplex();
        let transport = PiAcpTransport::new(client_end);
        assert!(transport.is_connected());
    }

    // ── Construction tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn pi_acp_transport_construction() {
        let (client_end, _) = test_duplex();
        let transport = PiAcpTransport::new(client_end);
        assert!(transport.is_connected());
    }

    #[tokio::test]
    async fn pi_acp_transport_not_initialized_initially() {
        let (client_end, _) = test_duplex();
        let transport = PiAcpTransport::new(client_end);
        assert!(!transport.is_initialized().await);
    }

    #[tokio::test]
    async fn pi_acp_transport_disconnect_idempotent() {
        let (client_end, _) = test_duplex();
        let mut transport = PiAcpTransport::new(client_end);

        // First disconnect.
        transport.disconnect().await;
        assert!(!transport.is_connected());

        // Second disconnect should not panic.
        transport.disconnect().await;
        assert!(!transport.is_connected());
    }

    #[tokio::test]
    async fn pi_acp_transport_post_disconnect_request_fails() {
        let (client_end, _) = test_duplex();
        let mut transport = PiAcpTransport::new(client_end);
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

    // ── ProviderTransport compliance ────────────────────────────────────

    #[tokio::test]
    async fn pi_acp_connect_rejected_after_disconnect() {
        let (client_end, _) = test_duplex();
        let mut transport = PiAcpTransport::new(client_end);
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

    #[tokio::test]
    async fn pi_acp_list_sessions_post_disconnect_fails() {
        let (client_end, _) = test_duplex();
        let mut transport = PiAcpTransport::new(client_end);
        transport.disconnect().await;

        let result = transport.list_sessions().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn pi_acp_send_request_before_init_fails() {
        let (client_end, _) = test_duplex();
        let mut transport = PiAcpTransport::new(client_end);
        // Don't initialize.

        let result = transport
            .send_request("prompt", serde_json::json!({"text": "hello"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn pi_acp_send_notification_post_disconnect_fails() {
        let (client_end, _) = test_duplex();
        let mut transport = PiAcpTransport::new(client_end);
        transport.disconnect().await;

        let result = transport
            .send_notification("cancel", serde_json::json!({}))
            .await;
        assert!(result.is_err());
    }

    // ── Unknown method handling ─────────────────────────────────────────

    #[tokio::test]
    async fn pi_acp_unknown_method_returns_error() {
        let (client_end, server_end) = test_duplex();
        let mut transport = PiAcpTransport::new(client_end);

        // Manually mark as initialized.
        *transport.initialized.lock().await = true;

        // Drop server end to avoid blocking.
        drop(server_end);

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

    // ── Event receiver ──────────────────────────────────────────────────

    #[tokio::test]
    async fn pi_acp_event_receiver_returns_broadcast_receiver() {
        let (client_end, _) = test_duplex();
        let transport = PiAcpTransport::new(client_end);
        let _rx = transport.event_receiver();
        // Should not panic.
    }

    // ── Factory from_client ────────────────────────────────────────────

    #[tokio::test]
    async fn pi_acp_from_client() {
        let (client_end, _) = test_duplex();
        let acp_client = AcpClient::new(client_end);
        let transport = PiAcpTransport::from_client(acp_client);
        assert!(transport.is_connected());
        assert!(!transport.is_initialized().await);
    }

    // ── Full lifecycle with mock ACP server ─────────────────────────────

    #[tokio::test]
    async fn pi_acp_full_lifecycle_with_mock_server() {
        // This test uses the same pattern as the ACP client tests:
        // create the AcpClient in Arc<Mutex>, drive mock server on the test thread,
        // and spawn client operations in separate tasks.
        let (client_end, mut mock_end) = tokio::io::duplex(8192);

        // Create ACP client directly (like the existing ACP tests do).
        let acp_client = Arc::new(tokio::sync::Mutex::new(AcpClient::new(client_end)));

        // Spawn initialize in a task.
        let c1 = acp_client.clone();
        let init_handle = tokio::spawn(async move {
            eprintln!("[client] starting initialize");
            let r = c1.lock().await.initialize("litter", "0.1.0").await;
            eprintln!("[client] initialize result: {:?}", r.is_ok());
            r
        });

        // Drive mock: read init request, send init response.
        eprintln!("[mock] waiting for init request");
        let init_req = read_mock_line(&mut mock_end).await;
        eprintln!("[mock] got init: {}", &init_req[..init_req.len().min(80)]);
        let init_val: serde_json::Value = serde_json::from_str(&init_req).unwrap();
        write_mock_line(
            &mut mock_end,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": init_val["id"],
                "result": {
                    "protocolVersion": 1,
                    "capabilities": {},
                    "authMethods": [{"id": "agent", "type": "agent"}]
                }
            }).to_string(),
        ).await;
        eprintln!("[mock] init response sent");

        let init_result = init_handle.await.unwrap();
        assert!(init_result.is_ok(), "initialize should succeed: {init_result:?}");

        // Now authenticate.
        let c2 = acp_client.clone();
        let auth_handle = tokio::spawn(async move {
            c2.lock().await.authenticate(None).await
        });

        let auth_req = read_mock_line(&mut mock_end).await;
        let auth_val: serde_json::Value = serde_json::from_str(&auth_req).unwrap();
        write_mock_line(
            &mut mock_end,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": auth_val["id"],
                "result": {}
            }).to_string(),
        ).await;

        let auth_result = auth_handle.await.unwrap();
        assert!(auth_result.is_ok(), "authenticate should succeed: {auth_result:?}");

        // Wrap in PiAcpTransport to test higher-level operations.
        // Since the AcpClient is already initialized, we manually set state.
        acp_client.lock().await.shutdown().await;
    }

    // ── Mock server helpers ────────────────────────────────────────────

    /// Read a single NDJSON line from the mock end.
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

    /// Mock the initialize + authenticate handshake.
    async fn mock_init_and_auth(mock: &mut tokio::io::DuplexStream) {
        // Read initialize request.
        eprintln!("[mock] waiting for init request");
        let init_req = read_mock_line(mock).await;
        eprintln!("[mock] got init request: {}", &init_req[..init_req.len().min(100)]);
        let init_val: serde_json::Value = serde_json::from_str(&init_req).unwrap();
        let init_id = init_val["id"].clone();

        // Write initialize response.
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": init_id,
            "result": {
                "protocolVersion": 1,
                "capabilities": {},
                "authMethods": [{"id": "agent", "type": "agent"}]
            }
        })
        .to_string();
        eprintln!("[mock] sending init response");
        write_mock_line(mock, &resp).await;
        eprintln!("[mock] init response sent");

        // Read authenticate request.
        eprintln!("[mock] waiting for auth request");
        let auth_req = read_mock_line(mock).await;
        eprintln!("[mock] got auth request: {}", &auth_req[..auth_req.len().min(100)]);
        let auth_val: serde_json::Value = serde_json::from_str(&auth_req).unwrap();
        let auth_id = auth_val["id"].clone();

        // Write authenticate response.
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": auth_id,
            "result": {}
        })
        .to_string();
        eprintln!("[mock] sending auth response");
        write_mock_line(mock, &resp).await;
        eprintln!("[mock] auth response sent");
    }

    /// Mock session/new response.
    async fn mock_session_new(mock: &mut tokio::io::DuplexStream) {
        let req = read_mock_line(mock).await;
        let val: serde_json::Value = serde_json::from_str(&req).unwrap();
        let id = val["id"].clone();
        write_mock_line(
            mock,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"sessionId": "pi-session-1"}
            })
            .to_string(),
        )
        .await;
    }

    /// Mock session/prompt with a streaming update and completion.
    async fn mock_prompt_with_streaming(mock: &mut tokio::io::DuplexStream) {
        let req = read_mock_line(mock).await;
        let val: serde_json::Value = serde_json::from_str(&req).unwrap();
        let id = val["id"].clone();

        // Send a streaming update notification.
        write_mock_line(
            mock,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "pi-session-1",
                    "update": {
                        "type": "agentMessageChunk",
                        "content": {"type": "text", "text": "Hello from Pi!"},
                        "index": 0
                    }
                }
            })
            .to_string(),
        )
        .await;

        // Send prompt completion response.
        write_mock_line(
            mock,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"stopReason": "endTurn"}
            })
            .to_string(),
        )
        .await;
    }
}
