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
            // ── Codex wire method adapters ───────────────────────────────
            // These map upstream Codex JSON-RPC methods to ACP session operations.
            //
            // VAL-PIACP-MAP-001: thread/start → ACP session/new + prompt
            "thread/start" => {
                let text = crate::provider::pi::transport::extract_text_from_params(&params)?;
                // Create a new ACP session.
                let cwd = params
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or(DEFAULT_PI_CWD);
                let session_id = self.create_session(cwd).await?;
                // Send prompt in the new session.
                let stop_reason = self.send_prompt(&session_id, &text).await?;
                // Return synthetic ThreadStart result with session ID as thread ID.
                Ok(serde_json::json!({
                    "id": session_id,
                    "thread_id": session_id,
                    "stop_reason": stop_reason,
                }))
            }
            // VAL-PIACP-MAP-002: turn/start → ACP prompt (no session/new)
            "turn/start" => {
                let text = crate::provider::pi::transport::extract_text_from_params(&params)?;
                let session_id = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                self.send_prompt(session_id, &text).await?;
                Ok(serde_json::Value::Null)
            }
            // VAL-PIACP-MAP-003: turn/interrupt → ACP cancel
            "turn/interrupt" => {
                let session_id = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                self.cancel_prompt(session_id).await?;
                Ok(serde_json::Value::Null)
            }
            // VAL-PIACP-MAP-004: thread/list → ACP session/list with format conversion
            "thread/list" => {
                let sessions = self.list_sessions().await?;
                // Convert ACP sessions to Codex ThreadList format.
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
            // VAL-PIACP-MAP-005: thread/read → ACP session/load
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
                    .unwrap_or(DEFAULT_PI_CWD);
                self.load_session(session_id, cwd).await?;
                Ok(serde_json::Value::Null)
            }
            // VAL-PIACP-MAP-006: no-op methods return success
            "thread/archive" | "thread/rollback" | "thread/name/set" => {
                Ok(serde_json::json!({"ok": true}))
            }
            // VAL-PIACP-MAP-007: collaborationMode/list returns empty
            "collaborationMode/list" => {
                Ok(serde_json::json!({"data": []}))
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

    // ── VAL-PI-001: ACP transport lifecycle ─────────────────────────────

    /// Test: PiAcpTransport can be constructed and reports connected.
    #[tokio::test]
    async fn pi_acp_lifecycle() {
        let (client_end, _) = test_duplex();
        let transport = PiAcpTransport::new(client_end);
        assert!(transport.is_connected());
    }

    /// Test: PiAcpTransport can be constructed from an existing AcpClient.
    #[tokio::test]
    async fn pi_acp_from_client() {
        let (client_end, _) = test_duplex();
        let acp_client = AcpClient::new(client_end);
        let transport = PiAcpTransport::from_client(acp_client);
        assert!(transport.is_connected());
        assert!(!transport.is_initialized().await);
    }

    /// Test: Transport is not initialized initially.
    #[tokio::test]
    async fn pi_acp_not_initialized_initially() {
        let (client_end, _) = test_duplex();
        let transport = PiAcpTransport::new(client_end);
        assert!(!transport.is_initialized().await);
    }

    /// Test: Full ACP lifecycle simulating Pi ACP transport.
    ///
    /// Uses the AcpClient directly (same pattern as ACP client tests)
    /// to verify the full Pi ACP lifecycle: initialize → authenticate →
    /// session/new → session/list → shutdown.
    ///
    /// VAL-PI-001: ACP initialize succeeds, session/new creates Pi session,
    /// session/list returns available sessions.
    #[tokio::test]
    async fn pi_acp_session_lifecycle() {
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
            "result": {"protocolVersion": 1, "capabilities": {}, "authMethods": amj}
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
        let h = tokio::spawn(async move { c.lock().await.session_new("/home/ubuntu").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "result": {"sessionId": "pi-sess-001"}
        }).to_string()).await;
        let sess = h.await.unwrap().unwrap();
        assert_eq!(sess.session_id.to_string(), "pi-sess-001");

        // session/list.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_list().await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": 4,
            "result": {"sessions": [{"sessionId": "pi-sess-001", "cwd": "/home/ubuntu", "title": "Test"}]}
        }).to_string()).await;
        let list = h.await.unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "pi-sess-001");

        client.lock().await.shutdown().await;
    }

    /// Test: Pi ACP prompt with streaming response.
    ///
    /// Tests the streaming portion of the Pi ACP lifecycle.
    /// Mirrors the existing `acp_session_prompt_streams_updates` test.
    /// VAL-PI-001: session/prompt sends prompt, streams response via session/update.
    #[tokio::test]
    async fn pi_acp_prompt_streams_response() {
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
            "result": {"protocolVersion": 1, "capabilities": {}, "authMethods": amj}
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
            "jsonrpc": "2.0", "id": 3, "result": {"sessionId": "pi-stream-1"}
        }).to_string()).await;
        h.await.unwrap().unwrap();

        // Subscribe to events before prompt.
        let mut event_rx = client.lock().await.subscribe();

        // session/prompt with streaming — same pattern as ACP client test.
        let c = client.clone();
        let h = tokio::spawn(async move {
            c.lock().await.session_prompt(&SessionId::new("pi-stream-1"), "Say hello").await
        });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "pi-stream-1",
                "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hello "}}
            }
        }).to_string()).await;
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "pi-stream-1",
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

    /// Test: ACP session/cancel aborts Pi prompt.
    ///
    /// Test: ACP session/cancel on idle session succeeds (no-op).
    ///
    /// VAL-PI-001: session/cancel when no active turn succeeds without error.
    #[tokio::test]
    async fn pi_acp_cancel_no_active_turn() {
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
            "result": {"protocolVersion": 1, "capabilities": {}, "authMethods": amj}
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
            "jsonrpc": "2.0", "id": 3, "result": {"sessionId": "pi-sess-cancel"}
        }).to_string()).await;
        h.await.unwrap().unwrap();

        // Cancel with no active turn — should succeed as a no-op (no wire message).
        let result = client.lock().await.session_cancel(&SessionId::new("pi-sess-cancel")).await;
        assert!(result.is_ok(), "cancel should succeed: {result:?}");

        client.lock().await.shutdown().await;
    }

    /// Test: session/load resumes past session via ACP.
    #[tokio::test]
    async fn pi_acp_session_load() {
        use agent_client_protocol_schema::{AuthMethod, AuthMethodAgent, SessionId};

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
            "result": {"protocolVersion": 1, "capabilities": {}, "authMethods": amj}
        }).to_string()).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "result": {}
        }).to_string()).await;
        h.await.unwrap().unwrap();

        // session/load.
        let c = client.clone();
        let load_h = tokio::spawn(async move {
            c.lock().await.session_load(&SessionId::new("pi-sess-resume"), "/home/ubuntu").await
        });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "result": {}
        }).to_string()).await;

        let load_result = load_h.await.unwrap();
        assert!(load_result.is_ok(), "session/load should succeed: {load_result:?}");

        client.lock().await.shutdown().await;
    }

    // ── Construction and edge case tests ─────────────────────────────────

    #[tokio::test]
    async fn pi_acp_transport_disconnect_idempotent() {
        let (client_end, _) = test_duplex();
        let mut transport = PiAcpTransport::new(client_end);

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

    #[tokio::test]
    async fn pi_acp_unknown_method_returns_error() {
        let (client_end, _server_end) = test_duplex();
        let mut transport = PiAcpTransport::new(client_end);

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

    #[tokio::test]
    async fn pi_acp_event_receiver_returns_broadcast_receiver() {
        let (client_end, _) = test_duplex();
        let transport = PiAcpTransport::new(client_end);
        let _rx = transport.event_receiver();
    }

    /// Test: connect() fails when the ACP server returns an error during initialize.
    #[tokio::test]
    async fn pi_acp_connect_fails_on_init_error() {
        let (client_end, mut mock_end) = test_duplex();
        let mut transport = PiAcpTransport::new(client_end);

        let config = ProviderConfig {
            client_name: "litter".to_string(),
            client_version: "0.1.0".to_string(),
            ..Default::default()
        };

        // Spawn connect in a task.
        let connect_handle = tokio::spawn(async move { transport.connect(&config).await });

        // Read initialize request, send error response.
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

    // ── Codex Wire Method Mapping (VAL-PIACP-MAP-*) ──────────────────────

    /// Helper: perform ACP initialize + authenticate handshake on the mock side.
    /// Reads the initialize and authenticate requests, sends back valid responses.
    async fn mock_handshake(mock: &mut tokio::io::DuplexStream) {
        use agent_client_protocol_schema::{AuthMethod, AuthMethodAgent};

        // Read initialize request, respond.
        let init_line = read_mock_line(mock).await;
        let init_val: serde_json::Value = serde_json::from_str(&init_line).unwrap();
        let am = vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))];
        let amj: Vec<serde_json::Value> = am.iter().map(|m| serde_json::to_value(m).unwrap()).collect();
        write_mock_line(mock, &serde_json::json!({
            "jsonrpc": "2.0", "id": init_val["id"],
            "result": {"protocolVersion": 1, "capabilities": {}, "authMethods": amj}
        }).to_string()).await;

        // Read authenticate request, respond.
        let auth_line = read_mock_line(mock).await;
        let auth_val: serde_json::Value = serde_json::from_str(&auth_line).unwrap();
        write_mock_line(mock, &serde_json::json!({
            "jsonrpc": "2.0", "id": auth_val["id"], "result": {}
        }).to_string()).await;
    }

    /// Helper: create a fully-authenticated PiAcpTransport + mock stream.
    /// Performs the full ACP handshake (initialize + authenticate).
    /// Returns (JoinHandle<PiAcpTransport>, mock_end).
    /// Caller should `let mut transport = handle.await.unwrap();` to get transport.
    async fn setup_authenticated_transport() -> (
        tokio::task::JoinHandle<PiAcpTransport>,
        tokio::io::DuplexStream,
    ) {
        let (client_end, mut mock_end) = test_duplex();

        let handle = tokio::spawn(async move {
            let config = ProviderConfig {
                client_name: "litter".to_string(),
                client_version: "0.1.0".to_string(),
                ..Default::default()
            };
            let mut transport = PiAcpTransport::new(client_end);
            transport.connect(&config).await.expect("handshake should succeed");
            transport
        });

        // Respond to handshake.
        mock_handshake(&mut mock_end).await;

        (handle, mock_end)
    }

    /// Helper: create a transport with only `initialized` flag set (no ACP handshake).
    /// Use for no-op method tests that don't need actual ACP communication.
    async fn setup_minimal_transport() -> (
        PiAcpTransport,
        tokio::io::DuplexStream,
    ) {
        let (client_end, mock_end) = test_duplex();
        let transport = PiAcpTransport::new(client_end);
        *transport.initialized.lock().await = true;
        (transport, mock_end)
    }

    // VAL-PIACP-MAP-001: thread/start → ACP session/new + prompt

    #[tokio::test]
    async fn pi_acp_codex_thread_start_calls_session_new_then_prompt() {
        let (transport_handle, mut mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        let handle = tokio::spawn(async move {
            transport.send_request(
                "thread/start",
                serde_json::json!({
                    "items": [{"role": "user", "content": "Hello Pi!"}]
                }),
            ).await
        });

        // Read session/new request.
        let new_line = read_mock_line(&mut mock_end).await;
        let new_val: serde_json::Value = serde_json::from_str(&new_line).unwrap();
        assert_eq!(new_val["method"], "session/new");

        // Reply with session ID.
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": new_val["id"],
            "result": {"sessionId": "acp-sess-001"}
        }).to_string()).await;

        // Read prompt request.
        let prompt_line = read_mock_line(&mut mock_end).await;
        let prompt_val: serde_json::Value = serde_json::from_str(&prompt_line).unwrap();
        assert_eq!(prompt_val["method"], "session/prompt");
        // ACP serializes prompt text as: params.prompt[0].text
        let text = prompt_val["params"]["prompt"][0]["text"]
            .as_str()
            .unwrap_or_else(|| {
                prompt_val["params"]["content"][0]["text"]
                    .as_str()
                    .unwrap_or_else(|| {
                        prompt_val["params"]["text"].as_str().unwrap_or("")
                    })
            });
        assert_eq!(text, "Hello Pi!");

        // Reply with prompt result.
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": prompt_val["id"],
            "result": {"stopReason": "end_turn"}
        }).to_string()).await;

        let result = handle.await.unwrap().unwrap();
        assert!(result["id"].is_string(), "should have 'id' field: {result}");
        assert_eq!(result["id"], "acp-sess-001");
        assert_eq!(result["thread_id"], "acp-sess-001");
    }

    #[tokio::test]
    async fn pi_acp_codex_thread_start_text_from_content_field() {
        let (transport_handle, mut mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        let handle = tokio::spawn(async move {
            transport.send_request(
                "thread/start",
                serde_json::json!({"content": "Direct content"}),
            ).await
        });

        let new_line = read_mock_line(&mut mock_end).await;
        let new_val: serde_json::Value = serde_json::from_str(&new_line).unwrap();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": new_val["id"],
            "result": {"sessionId": "sess-c"}
        }).to_string()).await;

        let prompt_line = read_mock_line(&mut mock_end).await;
        let prompt_val: serde_json::Value = serde_json::from_str(&prompt_line).unwrap();
        // ACP serializes prompt text as: params.prompt[0].text
        let text = prompt_val["params"]["prompt"][0]["text"]
            .as_str()
            .unwrap_or_else(|| prompt_val["params"]["text"].as_str().unwrap_or(""));
        assert_eq!(text, "Direct content");
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": prompt_val["id"],
            "result": {"stopReason": "end_turn"}
        }).to_string()).await;

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result["id"], "sess-c");
    }

    #[tokio::test]
    async fn pi_acp_codex_thread_start_no_text_returns_error() {
        let (mut transport, _) = setup_minimal_transport().await;

        let result = transport
            .send_request("thread/start", serde_json::json!({}))
            .await;

        assert!(result.is_err(), "should error when no text in params");
        match result {
            Err(RpcError::Deserialization(msg)) => {
                assert!(msg.contains("no text found"));
            }
            other => panic!("expected Deserialization error, got {other:?}"),
        }
    }

    // VAL-PIACP-MAP-002: turn/start → ACP prompt (no session/new)

    #[tokio::test]
    async fn pi_acp_codex_turn_start_calls_prompt_only() {
        let (transport_handle, mut mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        let handle = tokio::spawn(async move {
            transport.send_request(
                "turn/start",
                serde_json::json!({
                    "items": [{"role": "user", "content": "Follow-up"}]
                }),
            ).await
        });

        // Should be prompt, NOT session/new.
        let first_line = read_mock_line(&mut mock_end).await;
        let first_val: serde_json::Value = serde_json::from_str(&first_line).unwrap();
        assert_eq!(
            first_val["method"], "session/prompt",
            "turn/start should call prompt, not session/new"
        );
        let text = first_val["params"]["prompt"][0]["text"]
            .as_str()
            .unwrap_or_else(|| first_val["params"]["text"].as_str().unwrap_or(""));
        assert_eq!(text, "Follow-up");

        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": first_val["id"],
            "result": {"stopReason": "end_turn"}
        }).to_string()).await;

        let result = handle.await.unwrap().unwrap();
        assert!(result.is_null(), "turn/start should return null: {result}");
    }

    #[tokio::test]
    async fn pi_acp_codex_turn_start_with_session_id() {
        let (transport_handle, mut mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        let handle = tokio::spawn(async move {
            transport.send_request(
                "turn/start",
                serde_json::json!({
                    "items": [{"role": "user", "content": "Next message"}],
                    "session_id": "existing-sess"
                }),
            ).await
        });

        let line = read_mock_line(&mut mock_end).await;
        let val: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(val["method"], "session/prompt");
        let text = val["params"]["prompt"][0]["text"]
            .as_str()
            .unwrap_or_else(|| val["params"]["text"].as_str().unwrap_or(""));
        assert_eq!(text, "Next message");

        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": val["id"],
            "result": {"stopReason": "end_turn"}
        }).to_string()).await;

        handle.await.unwrap().unwrap();
    }

    // VAL-PIACP-MAP-003: turn/interrupt → ACP cancel

    #[tokio::test]
    async fn pi_acp_codex_turn_interrupt_calls_cancel() {
        let (transport_handle, _mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        // When no prompt is active, session_cancel is a no-op (returns Ok).
        // The transport still returns Ok(Value::Null).
        let result = transport
            .send_request("turn/interrupt", serde_json::json!({}))
            .await
            .unwrap();
        assert!(result.is_null(), "interrupt should return null on idle");
    }

    #[tokio::test]
    async fn pi_acp_codex_turn_interrupt_with_session_id() {
        let (transport_handle, _mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        // Cancel on idle is a no-op — just Ok(null).
        let result = transport
            .send_request(
                "turn/interrupt",
                serde_json::json!({"session_id": "sess-42"}),
            )
            .await
            .unwrap();
        assert!(result.is_null(), "interrupt with session_id should return null on idle");
    }

    // VAL-PIACP-MAP-004: thread/list → ACP session/list with format conversion

    #[tokio::test]
    async fn pi_acp_codex_thread_list_calls_session_list() {
        let (transport_handle, mut mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        let handle = tokio::spawn(async move {
            transport.send_request("thread/list", serde_json::json!({})).await
        });

        let line = read_mock_line(&mut mock_end).await;
        let val: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(val["method"], "session/list");

        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": val["id"],
            "result": {
                "sessions": [
                    {"sessionId": "s1", "cwd": "/home", "title": "Session 1"},
                    {"sessionId": "s2", "cwd": "/tmp", "title": "Session 2"}
                ]
            }
        }).to_string()).await;

        let result = handle.await.unwrap().unwrap();
        let threads = result["threads"].as_array().expect("should have threads array");
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0]["id"], "s1");
        assert_eq!(threads[0]["title"], "Session 1");
        assert_eq!(threads[1]["id"], "s2");
        assert_eq!(threads[1]["title"], "Session 2");
    }

    #[tokio::test]
    async fn pi_acp_codex_thread_list_empty() {
        let (transport_handle, mut mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        let handle = tokio::spawn(async move {
            transport.send_request("thread/list", serde_json::json!({})).await
        });

        let line = read_mock_line(&mut mock_end).await;
        let val: serde_json::Value = serde_json::from_str(&line).unwrap();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": val["id"],
            "result": {"sessions": []}
        }).to_string()).await;

        let result = handle.await.unwrap().unwrap();
        let threads = result["threads"].as_array().expect("should have threads array");
        assert!(threads.is_empty());
    }

    // VAL-PIACP-MAP-005: thread/read → ACP session/load

    #[tokio::test]
    async fn pi_acp_codex_thread_read_calls_session_load() {
        let (transport_handle, mut mock_end) = setup_authenticated_transport().await;
        let mut transport = transport_handle.await.unwrap();

        let handle = tokio::spawn(async move {
            transport.send_request(
                "thread/read",
                serde_json::json!({"thread_id": "load-sess-123"}),
            ).await
        });

        let line = read_mock_line(&mut mock_end).await;
        let val: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(val["method"], "session/load");
        assert_eq!(val["params"]["sessionId"], "load-sess-123");

        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": val["id"], "result": {}
        }).to_string()).await;

        let result = handle.await.unwrap().unwrap();
        assert!(result.is_null(), "thread/read should return null: {result}");
    }

    #[tokio::test]
    async fn pi_acp_codex_thread_read_missing_thread_id() {
        let (mut transport, _) = setup_minimal_transport().await;

        let result = transport
            .send_request("thread/read", serde_json::json!({}))
            .await;

        assert!(result.is_err(), "should error without thread_id");
    }

    // VAL-PIACP-MAP-006: no-op methods return success

    #[tokio::test]
    async fn pi_acp_codex_thread_archive_noop() {
        let (mut transport, mut mock_end) = setup_minimal_transport().await;

        let result = transport
            .send_request("thread/archive", serde_json::json!({"thread_id": "t1"}))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);

        // Verify no ACP JSON-RPC was sent.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 1];
        let nothing = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            mock_end.read(&mut buf),
        )
        .await;
        assert!(nothing.is_err(), "no ACP message should be sent for thread/archive");
    }

    #[tokio::test]
    async fn pi_acp_codex_thread_rollback_noop() {
        let (mut transport, _) = setup_minimal_transport().await;

        let result = transport
            .send_request("thread/rollback", serde_json::json!({"thread_id": "t1"}))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
    }

    #[tokio::test]
    async fn pi_acp_codex_thread_name_set_noop() {
        let (mut transport, _) = setup_minimal_transport().await;

        let result = transport
            .send_request(
                "thread/name/set",
                serde_json::json!({"thread_id": "t1", "name": "New Name"}),
            )
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
    }

    // VAL-PIACP-MAP-007: collaborationMode/list returns empty

    #[tokio::test]
    async fn pi_acp_codex_collaboration_mode_list_empty() {
        let (mut transport, mut mock_end) = setup_minimal_transport().await;

        let result = transport
            .send_request("collaborationMode/list", serde_json::json!({}))
            .await
            .unwrap();

        assert_eq!(result["data"], serde_json::json!([]));

        // Verify no ACP JSON-RPC was sent.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 1];
        let nothing = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            mock_end.read(&mut buf),
        )
        .await;
        assert!(nothing.is_err(), "no ACP message should be sent for collaborationMode/list");
    }

    /// Test: NDJSON framing is correct — messages exchanged as newline-delimited JSON.
    ///
    /// Verifies that the mock channel shows correctly framed NDJSON messages
    /// in both directions (part of VAL-PI-001).
    #[tokio::test]
    async fn pi_acp_ndjson_framing_correct() {
        use agent_client_protocol_schema::{AuthMethod, AuthMethodAgent};

        let (client_end, mut mock_end) = test_duplex();
        let client = Arc::new(tokio::sync::Mutex::new(AcpClient::new(client_end)));

        // Step 1: initialize — verify NDJSON framing.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        let init_line = read_mock_line(&mut mock_end).await;
        let init_val: serde_json::Value =
            serde_json::from_str(&init_line).expect("init should be valid JSON");
        assert_eq!(init_val["jsonrpc"].as_str(), Some("2.0"));
        assert!(init_val["method"].as_str() == Some("initialize"));
        assert!(init_val.get("id").is_some());

        let am = vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))];
        let amj: Vec<serde_json::Value> = am.iter().map(|m| serde_json::to_value(m).unwrap()).collect();
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": init_val["id"],
            "result": {"protocolVersion": 1, "capabilities": {}, "authMethods": amj}
        }).to_string()).await;
        h.await.unwrap().unwrap();

        // Step 2: authenticate — verify NDJSON framing.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        let auth_line = read_mock_line(&mut mock_end).await;
        let auth_val: serde_json::Value =
            serde_json::from_str(&auth_line).expect("auth should be valid JSON");
        assert_eq!(auth_val["jsonrpc"].as_str(), Some("2.0"));
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0", "id": auth_val["id"], "result": {}
        }).to_string()).await;
        h.await.unwrap().unwrap();

        client.lock().await.shutdown().await;
    }
}
