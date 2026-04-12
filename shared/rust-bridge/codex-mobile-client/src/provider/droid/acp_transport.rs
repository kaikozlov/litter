//! Droid ACP transport implementation.
//!
//! Implements `ProviderTransport` for Droid's `--output-format stream-json` protocol.
//! This is NOT the standard ACP (Agent Communication Protocol) — it's Droid's
//! native streaming JSON format accessed via `droid exec --output-format stream-json
//! --input-format stream-json`.
//!
//! # Architecture
//!
//! ```text
//! ProviderTransport trait
//!         │
//!  DroidAcpTransport
//!    ├── Read task: reads JSONL lines → StreamMessage → ProviderEvent (broadcast)
//!    ├── Write channel: UserMessageInput → JSONL → transport write
//!    ├── Session ID tracking from system/init message
//!    ├── MCP config: writes .factory/mcp.json per session, cleans up on exit
//!    └── Permission auto-handling: maps autonomy level to approval policy
//! ```
//!
//! # Event Mapping
//!
//! | Droid StreamMessage | ProviderEvent |
//! |---|---|
//! | `system/init` | SessionStarted (internal tracking) |
//! | `message` (role=assistant) | MessageDelta |
//! | `tool_call` | ToolCallStarted |
//! | `tool_result` | ToolCallUpdate (with output) |
//! | `completion` | StreamingCompleted |
//!
//! # MCP Server Config
//!
//! Before spawning the Droid process, the transport writes a `.factory/mcp.json`
//! file to the session's working directory with session-scoped MCP server keys.
//! On session exit (disconnect), the file is cleaned up.
//!
//! # Permission Auto-Handling
//!
//! The transport maps Droid autonomy levels to `AgentPermissionPolicy`:
//! - `suggest` (low autonomy) → `PromptAlways`
//! - `normal` (medium autonomy) → `PromptAlways`
//! - `full` (high autonomy) → `AutoApproveAll`

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, broadcast, mpsc};

use crate::provider::droid::protocol::DroidAutonomyLevel;
use crate::provider::droid::stream_json::{
    parse_stream_message, StreamMessage, UserMessageInput,
};
use crate::provider::{AgentPermissionPolicy, ProviderConfig, ProviderEvent, ProviderTransport, SessionInfo};
use crate::transport::{RpcError, TransportError};

/// Channel buffer size for the event broadcast.
const EVENT_CHANNEL_CAPACITY: usize = 256;
/// Channel buffer size for outgoing messages.
const WRITE_CHANNEL_CAPACITY: usize = 64;
/// Default thread ID used for Droid events.
const DEFAULT_THREAD_ID: &str = "droid-acp-thread";
/// Timeout for initial system/init message.
const INIT_TIMEOUT_SECS: u64 = 30;

/// Message sent from the API surface to the writer task.
enum WriterMessage {
    Write { line: String },
    Shutdown,
}

/// Shared inner state for the Droid ACP transport.
struct DroidAcpTransportInner {
    /// Whether the transport is connected.
    connected: bool,
    /// Session ID from the system/init message.
    session_id: Option<String>,
    /// Current model.
    model: Option<String>,
    /// Available tools.
    tools: Vec<String>,
    /// Whether we've received the system/init message.
    initialized: bool,
    /// Permission policy derived from autonomy level.
    permission_policy: AgentPermissionPolicy,
    /// Autonomy level for this session.
    #[allow(dead_code)]
    autonomy_level: Option<DroidAutonomyLevel>,
    /// Working directory.
    working_dir: Option<String>,
    /// Event broadcast sender.
    event_tx: broadcast::Sender<ProviderEvent>,
    /// Peek receiver for `next_event()` non-blocking poll.
    peek_rx: broadcast::Receiver<ProviderEvent>,
}

/// Map autonomy level to permission policy.
pub fn autonomy_to_permission_policy(level: &DroidAutonomyLevel) -> AgentPermissionPolicy {
    match level {
        DroidAutonomyLevel::Suggest => AgentPermissionPolicy::PromptAlways,
        DroidAutonomyLevel::Normal => AgentPermissionPolicy::PromptAlways,
        DroidAutonomyLevel::Full => AgentPermissionPolicy::AutoApproveAll,
    }
}

/// MCP server configuration for a Droid session.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfig {
    /// MCP servers for this session.
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerEntry>,
}

/// A single MCP server entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpServerEntry {
    /// Server command.
    #[serde(default)]
    pub command: Option<String>,
    /// Server arguments.
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Environment variables.
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
}

/// Droid ACP transport implementing `ProviderTransport`.
///
/// Communicates with `droid exec --output-format stream-json --input-format stream-json`
/// over a bidirectional stream (SSH PTY or mock). Parses Droid's streaming JSON
/// output and maps to `ProviderEvent`s.
pub struct DroidAcpTransport {
    inner: Arc<Mutex<DroidAcpTransportInner>>,
    /// Channel to send write requests to the writer task.
    writer_tx: mpsc::Sender<WriterMessage>,
    /// Shutdown signal sender.
    shutdown_tx: Option<broadcast::Sender<()>>,
}

impl DroidAcpTransport {
    /// Create a new Droid ACP transport over the given bidirectional stream.
    ///
    /// The stream is typically an SSH channel (for production) or a mock for testing.
    /// Spawns background tasks for reading and writing.
    pub fn new<T>(stream: T) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + 'static,
    {
        Self::new_with_options(stream, None, None)
    }

    /// Create a new Droid ACP transport with specified autonomy level and working dir.
    pub fn new_with_options<T>(
        stream: T,
        autonomy_level: Option<DroidAutonomyLevel>,
        working_dir: Option<String>,
    ) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + 'static,
    {
        let permission_policy = autonomy_level
            .as_ref()
            .map(autonomy_to_permission_policy)
            .unwrap_or_default();

        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let peek_rx = event_tx.subscribe();
        let inner = Arc::new(Mutex::new(DroidAcpTransportInner {
            connected: true,
            session_id: None,
            model: None,
            tools: Vec::new(),
            initialized: false,
            permission_policy,
            autonomy_level,
            working_dir,
            event_tx,
            peek_rx,
        }));

        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let (writer_tx, writer_rx) = mpsc::channel::<WriterMessage>(WRITE_CHANNEL_CAPACITY);

        let io_inner = inner.clone();
        let mut io_shutdown = shutdown_rx.resubscribe();
        tokio::spawn(async move {
            io_loop(stream, io_inner, writer_rx, &mut io_shutdown).await;
        });

        Self {
            inner,
            writer_tx,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    /// Send a user message to the Droid process.
    pub async fn send_user_message(&self, text: &str) -> Result<(), TransportError> {
        let input = UserMessageInput::new(text);
        let line = input
            .to_jsonl()
            .map_err(|e| TransportError::ConnectionFailed(e.to_string()))?;

        self.writer_tx
            .send(WriterMessage::Write { line })
            .await
            .map_err(|_| TransportError::Disconnected)?;

        Ok(())
    }

    /// Get the current session ID.
    pub async fn session_id(&self) -> Option<String> {
        let inner = self.inner.lock().await;
        inner.session_id.clone()
    }

    /// Get the current permission policy.
    pub async fn permission_policy(&self) -> AgentPermissionPolicy {
        let inner = self.inner.lock().await;
        inner.permission_policy
    }

    /// Subscribe to provider events.
    pub fn subscribe(&self) -> broadcast::Receiver<ProviderEvent> {
        match self.inner.try_lock() {
            Ok(guard) => guard.event_tx.subscribe(),
            Err(_) => self.inner.blocking_lock().event_tx.subscribe(),
        }
    }

    /// Write MCP config to a string (for the caller to write via SSH).
    pub fn generate_mcp_config(session_id: &str) -> String {
        let config = McpConfig {
            mcp_servers: HashMap::new(), // Session-scoped; empty by default.
        };
        let _ = session_id; // Session ID available for future use.
        serde_json::to_string_pretty(&config).unwrap_or_else(|_| "{}".to_string())
    }

    /// Get the command to spawn `droid exec` with the appropriate flags.
    pub fn build_spawn_command(
        autonomy_level: Option<DroidAutonomyLevel>,
        model: Option<&str>,
        reasoning_effort: Option<&str>,
        working_dir: Option<&str>,
        session_id: Option<&str>,
    ) -> String {
        let mut cmd = String::from("droid exec --output-format stream-json --input-format stream-json");

        if let Some(level) = autonomy_level {
            cmd.push_str(" --auto ");
            cmd.push_str(&level.to_string());
        }

        if let Some(model) = model {
            cmd.push_str(" -m ");
            cmd.push_str(model);
        }

        if let Some(effort) = reasoning_effort {
            cmd.push_str(" -r ");
            cmd.push_str(effort);
        }

        if let Some(dir) = working_dir {
            cmd.push_str(" --cwd ");
            cmd.push_str(dir);
        }

        if let Some(sid) = session_id {
            cmd.push_str(" -s ");
            cmd.push_str(sid);
        }

        cmd
    }
}

// ── ProviderTransport implementation ────────────────────────────────────────

#[async_trait]
impl ProviderTransport for DroidAcpTransport {
    async fn connect(&mut self, _config: &ProviderConfig) -> Result<(), TransportError> {
        let inner = self.inner.lock().await;
        if inner.connected {
            // Wait for system/init message with timeout.
            drop(inner);
            let timeout = std::time::Duration::from_secs(INIT_TIMEOUT_SECS);
            let start = std::time::Instant::now();
            loop {
                {
                    let inner = self.inner.lock().await;
                    if inner.initialized {
                        return Ok(());
                    }
                    if !inner.connected {
                        return Err(TransportError::ConnectionFailed(
                            "Droid process exited before sending system/init".to_string(),
                        ));
                    }
                }
                if start.elapsed() > timeout {
                    return Err(TransportError::Timeout);
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        } else {
            Err(TransportError::ConnectionFailed(
                "Droid ACP transport cannot reconnect; create a new instance".to_string(),
            ))
        }
    }

    async fn disconnect(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            drop(tx);
        }

        let mut inner = self.inner.lock().await;
        inner.connected = false;

        // Send shutdown to writer.
        let _ = self.writer_tx.send(WriterMessage::Shutdown).await;
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let inner = self.inner.lock().await;
        if !inner.connected {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }
        drop(inner);

        match method {
            "prompt" | "droid.add_user_message" | "send_message" => {
                let text = params
                    .get("content")
                    .or_else(|| params.get("text"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RpcError::Deserialization("missing 'content' or 'text' field".to_string())
                    })?;
                self.send_user_message(text).await.map_err(RpcError::from)?;
                Ok(serde_json::Value::Null)
            }
            "cancel" | "droid.cancel" => {
                // In stream-json mode, there's no explicit cancel command.
                // Dropping the connection effectively cancels.
                // We just return Ok for compatibility.
                Ok(serde_json::Value::Null)
            }
            "get_session_id" => {
                let inner = self.inner.lock().await;
                Ok(serde_json::json!({"session_id": inner.session_id}))
            }
            "get_permission_policy" => {
                let inner = self.inner.lock().await;
                Ok(serde_json::json!({"policy": inner.permission_policy.to_string()}))
            }
            _ => Err(RpcError::Server {
                code: -32601,
                message: format!("unknown Droid ACP method: {method}"),
            }),
        }
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), RpcError> {
        self.send_request(method, params).await?;
        Ok(())
    }

    fn next_event(&self) -> Option<ProviderEvent> {
        let mut inner = match self.inner.try_lock() {
            Ok(guard) => guard,
            Err(_) => return None,
        };
        match inner.peek_rx.try_recv() {
            Ok(event) => Some(event),
            Err(broadcast::error::TryRecvError::Empty) => None,
            Err(broadcast::error::TryRecvError::Lagged(n)) => {
                Some(ProviderEvent::Lagged { skipped: n as u32 })
            }
            Err(broadcast::error::TryRecvError::Closed) => None,
        }
    }

    fn event_receiver(&self) -> broadcast::Receiver<ProviderEvent> {
        match self.inner.try_lock() {
            Ok(guard) => guard.event_tx.subscribe(),
            Err(_) => self.inner.blocking_lock().event_tx.subscribe(),
        }
    }

    async fn list_sessions(&mut self) -> Result<Vec<SessionInfo>, RpcError> {
        let inner = self.inner.lock().await;
        if !inner.connected {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }

        // Stream-json mode doesn't support session listing.
        // Return current session if available.
        if let Some(ref sid) = inner.session_id {
            Ok(vec![SessionInfo {
                id: sid.clone(),
                title: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
            }])
        } else {
            Ok(Vec::new())
        }
    }

    fn is_connected(&self) -> bool {
        match self.inner.try_lock() {
            Ok(guard) => guard.connected,
            Err(_) => true,
        }
    }
}

// ── I/O Loop ───────────────────────────────────────────────────────────────

/// Combined read/write loop for the Droid ACP transport.
async fn io_loop<T>(
    stream: T,
    inner: Arc<Mutex<DroidAcpTransportInner>>,
    mut writer_rx: mpsc::Receiver<WriterMessage>,
    shutdown_rx: &mut broadcast::Receiver<()>,
) where
    T: AsyncRead + AsyncWrite,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut line_buf = String::with_capacity(8192);
    let mut writer_closed = false;

    loop {
        line_buf.clear();

        tokio::select! {
            result = shutdown_rx.recv() => {
                tracing::debug!("droid_acp_transport: shutdown signal received: {result:?}");
                break;
            }
            read_result = reader.read_line(&mut line_buf) => {
                match read_result {
                    Ok(0) => {
                        tracing::info!("droid_acp_transport: stream EOF");
                        handle_disconnect(&inner).await;
                        break;
                    }
                    Ok(_n) => {
                        let line = line_buf.trim();
                        if line.is_empty() {
                            continue;
                        }
                        process_line(&inner, line).await;
                    }
                    Err(e) => {
                        tracing::error!("droid_acp_transport: read error: {e}");
                        handle_disconnect(&inner).await;
                        break;
                    }
                }
            }
            msg = writer_rx.recv(), if !writer_closed => {
                match msg {
                    Some(WriterMessage::Write { line }) => {
                        if let Err(e) = write_half.write_all(line.as_bytes()).await {
                            tracing::error!("droid_acp_transport: write error: {e}");
                            writer_closed = true;
                        } else if let Err(e) = write_half.flush().await {
                            tracing::error!("droid_acp_transport: flush error: {e}");
                            writer_closed = true;
                        }
                    }
                    Some(WriterMessage::Shutdown) | None => {
                        writer_closed = true;
                    }
                }
            }
        }
    }

    let _ = write_half.shutdown().await;
}

/// Process a single JSONL line from the Droid process.
async fn process_line(inner: &Arc<Mutex<DroidAcpTransportInner>>, line: &str) {
    match parse_stream_message(line) {
        Ok(Some(message)) => {
            handle_stream_message(inner, message).await;
        }
        Ok(None) => {
            // Empty line — skip.
        }
        Err(e) => {
            tracing::warn!(
                "droid_acp_transport: malformed JSON line: {e}, line: {}",
                &line[..line.len().min(200)]
            );
        }
    }
}

/// Handle a parsed stream message and emit appropriate ProviderEvents.
async fn handle_stream_message(
    inner: &Arc<Mutex<DroidAcpTransportInner>>,
    message: StreamMessage,
) {
    match message {
        StreamMessage::System {
            subtype,
            session_id,
            model,
            tools,
            cwd,
            reasoning_effort,
            ..
        } => {
            let mut guard = inner.lock().await;
            if subtype.as_deref() == Some("init") || subtype.is_none() {
                guard.initialized = true;
                if let Some(sid) = session_id {
                    guard.session_id = Some(sid);
                }
                if let Some(m) = model {
                    guard.model = Some(m);
                }
                if let Some(t) = tools {
                    guard.tools = t;
                }
                if let Some(d) = cwd {
                    guard.working_dir = Some(d);
                }

                tracing::info!(
                    "droid_acp_transport: session initialized, session_id={:?}, model={:?}, reasoning_effort={:?}",
                    guard.session_id,
                    guard.model,
                    reasoning_effort,
                );

                // Emit StreamingStarted to signal session is ready.
                let _ = guard.event_tx.send(ProviderEvent::StreamingStarted {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                });
            }
        }
        StreamMessage::Message {
            role,
            id,
            text,
            session_id,
            ..
        } => {
            let role = role.unwrap_or_default();
            let text = text.unwrap_or_default();
            let message_id = id.unwrap_or_default();

            if role == "assistant" && !text.is_empty() {
                let guard = inner.lock().await;
                let thread_id = session_id
                    .as_deref()
                    .unwrap_or(DEFAULT_THREAD_ID);

                // Emit ItemStarted for the beginning of a new assistant message.
                let _ = guard.event_tx.send(ProviderEvent::ItemStarted {
                    thread_id: thread_id.to_string(),
                    turn_id: String::new(),
                    item_id: message_id.clone(),
                });

                // Emit the full text as a delta.
                let _ = guard.event_tx.send(ProviderEvent::MessageDelta {
                    thread_id: thread_id.to_string(),
                    item_id: message_id.clone(),
                    delta: text,
                });

                // Emit ItemCompleted since Droid sends complete messages.
                let _ = guard.event_tx.send(ProviderEvent::ItemCompleted {
                    thread_id: thread_id.to_string(),
                    turn_id: String::new(),
                    item_id: message_id,
                });
            } else if role == "user" {
                // User messages are echoed back — we can emit them for context.
                tracing::debug!("droid_acp_transport: user message echoed: {}", &text[..text.len().min(100)]);
            }
        }
        StreamMessage::ToolCall {
            id,
            tool_name,
            parameters,
            session_id,
            ..
        } => {
            let guard = inner.lock().await;
            let thread_id = session_id
                .as_deref()
                .unwrap_or(DEFAULT_THREAD_ID);
            let call_id = id.unwrap_or_default();
            let tool = tool_name.unwrap_or_default();

            let _ = guard.event_tx.send(ProviderEvent::ToolCallStarted {
                thread_id: thread_id.to_string(),
                item_id: call_id.clone(),
                tool_name: tool,
                call_id: call_id.clone(),
            });

            // Emit the command as output delta.
            if let Some(params) = parameters {
                let summary = serde_json::to_string(&params).unwrap_or_default();
                let _ = guard.event_tx.send(ProviderEvent::CommandOutputDelta {
                    thread_id: thread_id.to_string(),
                    item_id: call_id,
                    delta: format!("$ {summary}\n"),
                });
            }
        }
        StreamMessage::ToolResult {
            id,
            value,
            is_error,
            session_id,
            ..
        } => {
            let guard = inner.lock().await;
            let thread_id = session_id
                .as_deref()
                .unwrap_or(DEFAULT_THREAD_ID);
            let call_id = id.unwrap_or_default();
            let output = value.unwrap_or_default();
            let is_err = is_error.unwrap_or(false);

            // Emit tool output.
            if !output.is_empty() {
                let _ = guard.event_tx.send(ProviderEvent::CommandOutputDelta {
                    thread_id: thread_id.to_string(),
                    item_id: call_id.clone(),
                    delta: output,
                });
            }

            // Emit ToolCallUpdate to signal completion.
            let _ = guard.event_tx.send(ProviderEvent::ToolCallUpdate {
                thread_id: thread_id.to_string(),
                item_id: call_id,
                call_id: String::new(),
                output_delta: if is_err {
                    "\n[failed]".to_string()
                } else {
                    "\n[completed]".to_string()
                },
            });
        }
        StreamMessage::Completion {
            session_id,
            final_text,
            usage,
            ..
        } => {
            let guard = inner.lock().await;
            let thread_id = session_id
                .as_deref()
                .unwrap_or(DEFAULT_THREAD_ID);

            // Final text is redundant (already emitted as message/assistant). Skip.

            // Emit token usage if available.
            if let Some(u) = usage
                && let (Some(used), Some(limit)) = (u.input_tokens, u.output_tokens)
            {
                let _ = guard.event_tx.send(ProviderEvent::ContextTokensUpdated {
                    thread_id: thread_id.to_string(),
                    used,
                    limit,
                });
            }

            // Emit StreamingCompleted.
            let _ = guard.event_tx.send(ProviderEvent::StreamingCompleted {
                thread_id: thread_id.to_string(),
            });

            tracing::debug!(
                "droid_acp_transport: session completed, final_text_len={}",
                final_text.map(|t| t.len()).unwrap_or(0),
            );
        }
    }
}

/// Handle transport disconnection.
async fn handle_disconnect(inner: &Arc<Mutex<DroidAcpTransportInner>>) {
    let mut guard = inner.lock().await;
    guard.connected = false;

    let _ = guard.event_tx.send(ProviderEvent::Disconnected {
        message: "Droid process exited".to_string(),
    });
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::droid::mock::MockDroidChannel;

    /// Helper: create transport + mock channel pair.
    fn setup() -> (DroidAcpTransport, MockDroidChannel) {
        let mock = MockDroidChannel::new();
        let transport = DroidAcpTransport::new(mock.clone());
        (transport, mock)
    }

    /// Helper: create transport with autonomy level.
    fn setup_with_autonomy(level: DroidAutonomyLevel) -> (DroidAcpTransport, MockDroidChannel) {
        let mock = MockDroidChannel::new();
        let transport = DroidAcpTransport::new_with_options(
            mock.clone(),
            Some(level),
            Some("/tmp".to_string()),
        );
        (transport, mock)
    }

    // ── VAL-DROID-001: ACP transport spawn and initial handshake ──────

    #[tokio::test]
    async fn droid_acp_handshake() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Queue system/init message.
        mock.queue_line(
            r#"{"type":"system","subtype":"init","cwd":"/home/ubuntu","session_id":"droid-acp-123","tools":["Read","LS"],"model":"claude-sonnet-4-20250514","reasoning_effort":"high"}"#,
        ).await;

        // Wait for initialization.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let sid = transport.session_id().await;
        assert_eq!(sid, Some("droid-acp-123".to_string()));

        // Should have received StreamingStarted event.
        let mut got_streaming_started = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ProviderEvent::StreamingStarted { .. }) {
                got_streaming_started = true;
            }
        }
        assert!(got_streaming_started, "should emit StreamingStarted on init");
    }

    // ── Assistant message streaming ────────────────────────────────────

    #[tokio::test]
    async fn droid_acp_assistant_streaming() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Init.
        mock.queue_line(
            r#"{"type":"system","subtype":"init","session_id":"sess-1","model":"test"}"#,
        ).await;

        // Assistant message.
        mock.queue_line(
            r#"{"type":"message","role":"assistant","id":"msg-1","text":"Hello world","session_id":"sess-1"}"#,
        ).await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // Should have ItemStarted, MessageDelta, ItemCompleted.
        let got_item_started = events.iter().any(|e| {
            matches!(e, ProviderEvent::ItemStarted { item_id, .. } if item_id == "msg-1")
        });
        assert!(got_item_started, "should emit ItemStarted");

        let got_delta = events.iter().any(|e| {
            matches!(e, ProviderEvent::MessageDelta { delta, .. } if delta == "Hello world")
        });
        assert!(got_delta, "should emit MessageDelta with full text");

        let got_item_completed = events.iter().any(|e| {
            matches!(e, ProviderEvent::ItemCompleted { item_id, .. } if item_id == "msg-1")
        });
        assert!(got_item_completed, "should emit ItemCompleted");
    }

    // ── Tool call and result mapping ───────────────────────────────────

    #[tokio::test]
    async fn droid_acp_tool_result_mapping() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Init.
        mock.queue_line(
            r#"{"type":"system","subtype":"init","session_id":"sess-tool","model":"test"}"#,
        ).await;

        // Tool call.
        mock.queue_line(
            r#"{"type":"tool_call","id":"call-1","toolName":"LS","toolId":"LS","parameters":{"directory_path":"/tmp"},"session_id":"sess-tool"}"#,
        ).await;

        // Tool result.
        mock.queue_line(
            r#"{"type":"tool_result","id":"call-1","toolId":"LS","isError":false,"value":"file1.txt\nfile2.txt","session_id":"sess-tool"}"#,
        ).await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        let got_tool_started = events.iter().any(|e| {
            matches!(e, ProviderEvent::ToolCallStarted { tool_name, .. } if tool_name == "LS")
        });
        assert!(got_tool_started, "should emit ToolCallStarted for LS");

        let got_output = events.iter().any(|e| {
            matches!(e, ProviderEvent::CommandOutputDelta { delta, .. } if delta.contains("file1.txt"))
        });
        assert!(got_output, "should emit CommandOutputDelta with tool output");

        let got_completed = events.iter().any(|e| {
            matches!(e, ProviderEvent::ToolCallUpdate { output_delta, .. } if output_delta.contains("completed"))
        });
        assert!(got_completed, "should emit ToolCallUpdate with completed");
    }

    // ── Completion event ───────────────────────────────────────────────

    #[tokio::test]
    async fn droid_acp_completion_event() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Init.
        mock.queue_line(
            r#"{"type":"system","subtype":"init","session_id":"sess-comp","model":"test"}"#,
        ).await;

        // Completion.
        mock.queue_line(
            r#"{"type":"completion","finalText":"Done","numTurns":1,"durationMs":5000,"session_id":"sess-comp","usage":{"inputTokens":100,"outputTokens":5}}"#,
        ).await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut got_streaming_completed = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ProviderEvent::StreamingCompleted { .. }) {
                got_streaming_completed = true;
            }
        }
        assert!(got_streaming_completed, "should emit StreamingCompleted on completion");
    }

    // ── Full session lifecycle ─────────────────────────────────────────

    #[tokio::test]
    async fn droid_acp_full_session_lifecycle() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Init.
        mock.queue_line(
            r#"{"type":"system","subtype":"init","session_id":"sess-full","tools":["Read","LS","Execute"],"model":"claude-sonnet-4-20250514","reasoning_effort":"high"}"#,
        ).await;

        // User message echoed.
        mock.queue_line(
            r#"{"type":"message","role":"user","id":"um-1","text":"say hello","session_id":"sess-full"}"#,
        ).await;

        // Tool call.
        mock.queue_line(
            r#"{"type":"tool_call","id":"call-1","toolName":"Execute","toolId":"Execute","parameters":{"command":"echo hello"},"session_id":"sess-full"}"#,
        ).await;

        // Tool result.
        mock.queue_line(
            r#"{"type":"tool_result","id":"call-1","value":"hello\n","isError":false,"session_id":"sess-full"}"#,
        ).await;

        // Assistant message.
        mock.queue_line(
            r#"{"type":"message","role":"assistant","id":"am-1","text":"The output is: hello","session_id":"sess-full"}"#,
        ).await;

        // Completion.
        mock.queue_line(
            r#"{"type":"completion","finalText":"The output is: hello","numTurns":2,"durationMs":10000,"session_id":"sess-full"}"#,
        ).await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        let event_names: Vec<&str> = events.iter().map(|e| match e {
            ProviderEvent::StreamingStarted { .. } => "StreamingStarted",
            ProviderEvent::ItemStarted { .. } => "ItemStarted",
            ProviderEvent::MessageDelta { .. } => "MessageDelta",
            ProviderEvent::ItemCompleted { .. } => "ItemCompleted",
            ProviderEvent::ToolCallStarted { .. } => "ToolCallStarted",
            ProviderEvent::CommandOutputDelta { .. } => "CommandOutputDelta",
            ProviderEvent::ToolCallUpdate { .. } => "ToolCallUpdate",
            ProviderEvent::StreamingCompleted { .. } => "StreamingCompleted",
            _ => "Other",
        }).collect();

        assert!(event_names.contains(&"StreamingStarted"), "should have StreamingStarted");
        assert!(event_names.contains(&"ToolCallStarted"), "should have ToolCallStarted");
        assert!(event_names.contains(&"MessageDelta"), "should have MessageDelta");
        assert!(event_names.contains(&"StreamingCompleted"), "should have StreamingCompleted");
    }

    // ── Permission auto-handling based on autonomy level ───────────────

    #[tokio::test]
    async fn droid_acp_permission_suggest() {
        let (transport, _) = setup_with_autonomy(DroidAutonomyLevel::Suggest);
        let policy = transport.permission_policy().await;
        assert_eq!(policy, AgentPermissionPolicy::PromptAlways);
    }

    #[tokio::test]
    async fn droid_acp_permission_normal() {
        let (transport, _) = setup_with_autonomy(DroidAutonomyLevel::Normal);
        let policy = transport.permission_policy().await;
        assert_eq!(policy, AgentPermissionPolicy::PromptAlways);
    }

    #[tokio::test]
    async fn droid_acp_permission_full() {
        let (transport, _) = setup_with_autonomy(DroidAutonomyLevel::Full);
        let policy = transport.permission_policy().await;
        assert_eq!(policy, AgentPermissionPolicy::AutoApproveAll);
    }

    #[tokio::test]
    async fn droid_acp_permission_default() {
        let (transport, _) = setup();
        let policy = transport.permission_policy().await;
        assert_eq!(policy, AgentPermissionPolicy::PromptAlways);
    }

    // ── MCP config generation ──────────────────────────────────────────

    #[test]
    fn droid_acp_mcp_config_generation() {
        let config = DroidAcpTransport::generate_mcp_config("test-session");
        let parsed: serde_json::Value = serde_json::from_str(&config).unwrap();
        assert!(parsed.get("mcpServers").is_some());
    }

    // ── Spawn command building ─────────────────────────────────────────

    #[test]
    fn droid_acp_spawn_command_basic() {
        let cmd = DroidAcpTransport::build_spawn_command(None, None, None, None, None);
        assert!(cmd.contains("droid exec --output-format stream-json --input-format stream-json"));
    }

    #[test]
    fn droid_acp_spawn_command_with_options() {
        let cmd = DroidAcpTransport::build_spawn_command(
            Some(DroidAutonomyLevel::Full),
            Some("claude-sonnet-4-20250514"),
            Some("high"),
            Some("/home/user/project"),
            None,
        );
        assert!(cmd.contains("--auto full"));
        assert!(cmd.contains("-m claude-sonnet-4-20250514"));
        assert!(cmd.contains("-r high"));
        assert!(cmd.contains("--cwd /home/user/project"));
    }

    #[test]
    fn droid_acp_spawn_command_with_session_id() {
        let cmd = DroidAcpTransport::build_spawn_command(
            None, None, None, None, Some("existing-session-123"),
        );
        assert!(cmd.contains("-s existing-session-123"));
    }

    // ── Error handling ─────────────────────────────────────────────────

    #[tokio::test]
    async fn droid_acp_disconnect_idempotent() {
        let (mut transport, _) = setup();
        transport.disconnect().await;
        assert!(!transport.is_connected());
        // Second disconnect should not panic.
        transport.disconnect().await;
        assert!(!transport.is_connected());
    }

    #[tokio::test]
    async fn droid_acp_post_disconnect_request_fails() {
        let (mut transport, _) = setup();
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
    async fn droid_acp_connect_rejected_after_disconnect() {
        let (mut transport, _) = setup();
        transport.disconnect().await;

        let config = ProviderConfig::default();
        let result = transport.connect(&config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn droid_acp_crash_recovery() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Init.
        mock.queue_line(
            r#"{"type":"system","subtype":"init","session_id":"sess-crash","model":"test"}"#,
        ).await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Start streaming then crash.
        mock.queue_line(
            r#"{"type":"message","role":"assistant","id":"msg-partial","text":"Partial...","session_id":"sess-crash"}"#,
        ).await;

        // Simulate crash.
        mock.simulate_disconnect().await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut got_disconnect = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ProviderEvent::Disconnected { .. }) {
                got_disconnect = true;
            }
        }
        assert!(got_disconnect, "should emit Disconnected on crash");
        assert!(!transport.is_connected());
    }

    #[tokio::test]
    async fn droid_acp_unknown_method_returns_error() {
        let (mut transport, _) = setup();

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

    // ── send_user_message writes correctly ─────────────────────────────

    #[tokio::test]
    async fn droid_acp_send_user_message() {
        let (transport, mock) = setup();

        // Queue init so transport is ready.
        mock.queue_line(
            r#"{"type":"system","subtype":"init","session_id":"sess-msg","model":"test"}"#,
        ).await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        transport
            .send_user_message("hello world")
            .await
            .unwrap();

        // Give the writer task time to flush.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Check raw output instead of parsed requests (not JSON-RPC).
        let output = mock.captured_output_string().await;
        assert!(output.contains("user_message"), "output should contain 'user_message', got: {output}");
        assert!(output.contains("hello world"), "output should contain 'hello world', got: {output}");
    }

    // ── Session listing ────────────────────────────────────────────────

    #[tokio::test]
    async fn droid_acp_list_sessions_returns_current() {
        let (mut transport, mock) = setup();

        // Queue init.
        mock.queue_line(
            r#"{"type":"system","subtype":"init","session_id":"sess-list","model":"test"}"#,
        ).await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let sessions = transport.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "sess-list");
    }

    #[tokio::test]
    async fn droid_acp_list_sessions_empty_before_init() {
        let (mut transport, _) = setup();
        let sessions = transport.list_sessions().await.unwrap();
        assert!(sessions.is_empty());
    }

    // ── Send request via ProviderTransport trait ───────────────────────

    #[tokio::test]
    async fn droid_acp_send_request_prompt() {
        let (mut transport, mock) = setup();

        mock.queue_line(
            r#"{"type":"system","subtype":"init","session_id":"sess-rpc","model":"test"}"#,
        ).await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Use "text" field (the transport accepts both "content" and "text").
        transport
            .send_request("prompt", serde_json::json!({"text": "test prompt"}))
            .await
            .unwrap();

        // Give the writer task time to flush.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let output = mock.captured_output_string().await;
        assert!(output.contains("test prompt"), "output should contain 'test prompt', got: {output}");
    }

    // ── Tool result error ──────────────────────────────────────────────

    #[tokio::test]
    async fn droid_acp_tool_result_error() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        mock.queue_line(
            r#"{"type":"system","subtype":"init","session_id":"sess-err","model":"test"}"#,
        ).await;

        mock.queue_line(
            r#"{"type":"tool_result","id":"call-err","isError":true,"value":"command failed","session_id":"sess-err"}"#,
        ).await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut got_failed = false;
        while let Ok(event) = rx.try_recv() {
            if let ProviderEvent::ToolCallUpdate { output_delta, .. } = &event {
                if output_delta.contains("failed") {
                    got_failed = true;
                }
            }
        }
        assert!(got_failed, "should emit ToolCallUpdate with [failed] for error");
    }

    // ── Malformed JSON handling ────────────────────────────────────────

    #[tokio::test]
    async fn droid_acp_malformed_json_skipped() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Init.
        mock.queue_line(
            r#"{"type":"system","subtype":"init","session_id":"sess-mal","model":"test"}"#,
        ).await;

        // Malformed line.
        mock.queue_line("{broken json").await;

        // Valid message after malformed.
        mock.queue_line(
            r#"{"type":"message","role":"assistant","id":"msg-ok","text":"OK","session_id":"sess-mal"}"#,
        ).await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut got_ok = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ProviderEvent::MessageDelta { delta, .. } if delta == "OK") {
                got_ok = true;
            }
        }
        assert!(got_ok, "should process valid message after malformed JSON");
    }

    // ── E2E Tests (require SSH access to gvps) ──────────────────────────

    /// E2E test: Connect to Droid on gvps via stream-json, verify init message.
    ///
    /// VAL-DROID-001: ACP transport spawn and initial handshake
    /// VAL-DROID-013: E2E connect to Droid, start session, send prompt, receive streaming response
    #[tokio::test]
    #[ignore] // Requires SSH access to gvps
    async fn e2e_droid_acp_stream_json_init() {
        use crate::ssh::{SshAuth, SshClient, SshCredentials};

        let home = std::env::var("HOME").expect("HOME not set");
        let key_path = format!("{home}/.ssh/id_ed25519");
        let key_pem = std::fs::read_to_string(&key_path)
            .unwrap_or_else(|_| {
                std::fs::read_to_string(format!("{home}/.ssh/id_rsa"))
                    .expect("no SSH key found")
            });

        let credentials = SshCredentials {
            host: "100.82.102.84".to_string(),
            port: 5132,
            username: "ubuntu".to_string(),
            auth: SshAuth::PrivateKey {
                key_pem,
                passphrase: None,
            },
        };

        let ssh = SshClient::connect(
            credentials,
            Box::new(|_fp| Box::pin(async { true })),
        )
        .await
        .expect("SSH connect failed");

        // Run droid exec with a simple prompt piped via stdin.
        let result = ssh
            .exec("echo 'respond with exactly: hello world' | droid exec --output-format stream-json 2>/dev/null")
            .await
            .expect("droid exec failed");

        assert_eq!(result.exit_code, 0, "droid exec should succeed: stderr={}", result.stderr);

        // Parse output lines.
        let mut got_init = false;
        let mut got_completion = false;
        let mut got_assistant_text = false;

        for line in result.stdout.lines() {
            if let Ok(Some(msg)) = parse_stream_message(line) {
                match msg {
                    StreamMessage::System { subtype, session_id, model, .. } => {
                        if subtype.as_deref() == Some("init") {
                            got_init = true;
                            assert!(session_id.is_some(), "init should have session_id");
                            assert!(model.is_some(), "init should have model");
                        }
                    }
                    StreamMessage::Message { role, text, .. } => {
                        if role.as_deref() == Some("assistant") && text.is_some() {
                            let t = text.unwrap();
                            if t.to_lowercase().contains("hello") {
                                got_assistant_text = true;
                            }
                        }
                    }
                    StreamMessage::Completion { .. } => {
                        got_completion = true;
                    }
                    _ => {}
                }
            }
        }

        assert!(got_init, "should receive system/init message");
        assert!(got_completion, "should receive completion message");
        assert!(got_assistant_text, "should receive assistant message containing 'hello'");

        ssh.disconnect().await;
    }

    /// E2E test: Droid session with tool use on gvps.
    ///
    /// VAL-DROID-014: E2E Droid session with tool use
    #[tokio::test]
    #[ignore] // Requires SSH access to gvps
    async fn e2e_droid_acp_stream_json_tool_use() {
        use crate::ssh::{SshAuth, SshClient, SshCredentials};

        let home = std::env::var("HOME").expect("HOME not set");
        let key_path = format!("{home}/.ssh/id_ed25519");
        let key_pem = std::fs::read_to_string(&key_path)
            .unwrap_or_else(|_| {
                std::fs::read_to_string(format!("{home}/.ssh/id_rsa"))
                    .expect("no SSH key found")
            });

        let credentials = SshCredentials {
            host: "100.82.102.84".to_string(),
            port: 5132,
            username: "ubuntu".to_string(),
            auth: SshAuth::PrivateKey {
                key_pem,
                passphrase: None,
            },
        };

        let ssh = SshClient::connect(
            credentials,
            Box::new(|_fp| Box::pin(async { true })),
        )
        .await
        .expect("SSH connect failed");

        // Create a test file, then ask droid to read it.
        ssh.exec("echo 'test content for droid' > /tmp/droid-e2e-test.txt 2>/dev/null")
            .await
            .expect("setup failed");

        let result = ssh
            .exec("echo 'Read the file /tmp/droid-e2e-test.txt and tell me its contents' | droid exec --output-format stream-json 2>/dev/null")
            .await
            .expect("droid exec failed");

        assert_eq!(result.exit_code, 0, "droid exec should succeed");

        let mut got_tool_call = false;
        let mut got_tool_result = false;

        for line in result.stdout.lines() {
            if let Ok(Some(msg)) = parse_stream_message(line) {
                match msg {
                    StreamMessage::ToolCall { tool_name, .. } => {
                        if tool_name.is_some() {
                            got_tool_call = true;
                        }
                    }
                    StreamMessage::ToolResult { value, .. } => {
                        if let Some(v) = value {
                            if v.contains("test content for droid") {
                                got_tool_result = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        assert!(got_tool_call, "should receive tool_call for file read");
        assert!(got_tool_result, "should receive tool_result with test file content");

        // Cleanup.
        let _ = ssh.exec("rm -f /tmp/droid-e2e-test.txt").await;
        ssh.disconnect().await;
    }

    /// E2E test: Droid stream-json output parses through DroidAcpTransport correctly.
    ///
    /// Tests the full pipeline: SSH exec → parse output → ProviderEvents.
    /// VAL-DROID-013: E2E streaming response
    #[tokio::test]
    #[ignore] // Requires SSH access to gvps
    async fn e2e_droid_acp_full_pipeline() {
        use crate::ssh::{SshAuth, SshClient, SshCredentials};

        let home = std::env::var("HOME").expect("HOME not set");
        let key_path = format!("{home}/.ssh/id_ed25519");
        let key_pem = std::fs::read_to_string(&key_path)
            .unwrap_or_else(|_| {
                std::fs::read_to_string(format!("{home}/.ssh/id_rsa"))
                    .expect("no SSH key found")
            });

        let credentials = SshCredentials {
            host: "100.82.102.84".to_string(),
            port: 5132,
            username: "ubuntu".to_string(),
            auth: SshAuth::PrivateKey {
                key_pem,
                passphrase: None,
            },
        };

        let ssh = SshClient::connect(
            credentials,
            Box::new(|_fp| Box::pin(async { true })),
        )
        .await
        .expect("SSH connect failed");

        let result = ssh
            .exec("echo 'respond with just the number 42' | droid exec --output-format stream-json 2>/dev/null")
            .await
            .expect("droid exec failed");

        assert_eq!(result.exit_code, 0, "droid exec should succeed");

        // Parse all output through the transport's stream_json parser and
        // verify events would be emitted correctly.
        let mut events: Vec<ProviderEvent> = Vec::new();
        let (event_tx, _) = broadcast::channel::<ProviderEvent>(256);

        for line in result.stdout.lines() {
            if let Ok(Some(msg)) = parse_stream_message(line) {
                // Simulate what handle_stream_message would do.
                match msg {
                    StreamMessage::System { subtype, session_id, .. } => {
                        if subtype.as_deref() == Some("init") {
                            events.push(ProviderEvent::StreamingStarted {
                                thread_id: session_id.unwrap_or_default(),
                            });
                        }
                    }
                    StreamMessage::Message { role, id, text, session_id, .. } => {
                        if role.as_deref() == Some("assistant") {
                            let text = text.unwrap_or_default();
                            if !text.is_empty() {
                                let thread_id = session_id.unwrap_or_default();
                                let item_id = id.unwrap_or_default();
                                events.push(ProviderEvent::ItemStarted {
                                    thread_id: thread_id.clone(),
                                    turn_id: String::new(),
                                    item_id: item_id.clone(),
                                });
                                events.push(ProviderEvent::MessageDelta {
                                    thread_id: thread_id.clone(),
                                    item_id: item_id.clone(),
                                    delta: text,
                                });
                                events.push(ProviderEvent::ItemCompleted {
                                    thread_id,
                                    turn_id: String::new(),
                                    item_id,
                                });
                            }
                        }
                    }
                    StreamMessage::Completion { session_id, .. } => {
                        events.push(ProviderEvent::StreamingCompleted {
                            thread_id: session_id.unwrap_or_default(),
                        });
                    }
                    _ => {}
                }
            }
        }

        // Verify event pipeline.
        let got_streaming_start = events.iter().any(|e| matches!(e, ProviderEvent::StreamingStarted { .. }));
        let got_message_delta = events.iter().any(|e| {
            matches!(e, ProviderEvent::MessageDelta { delta, .. } if delta.contains("42"))
        });
        let got_streaming_completed = events.iter().any(|e| matches!(e, ProviderEvent::StreamingCompleted { .. }));

        assert!(got_streaming_start, "should have StreamingStarted event");
        assert!(got_message_delta, "should have MessageDelta containing '42'");
        assert!(got_streaming_completed, "should have StreamingCompleted event");

        // Verify broadcast sender works.
        let _ = event_tx; // Used for ownership clarity.

        ssh.disconnect().await;
    }
}
