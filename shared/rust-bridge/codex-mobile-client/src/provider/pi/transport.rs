//! Pi native RPC transport implementation.
//!
//! Implements `ProviderTransport` for Pi's `--mode rpc` JSONL protocol.
//! The transport communicates over a bidirectional stream (SSH PTY or mock)
//! using newline-delimited JSON messages.
//!
//! # Architecture
//!
//! ```text
//! ProviderTransport trait
//!         │
//!  PiNativeTransport
//!    ├── Read task: reads JSONL lines → PiEvent → ProviderEvent (broadcast)
//!    ├── Write channel: PiCommand → JSONL line → transport write
//!    └── Pending command tracking: response correlation by command type
//! ```
//!
//! # Event Mapping
//!
//! | Pi Event | ProviderEvent |
//! |---|---|
//! | `agent_start` | `StreamingStarted` |
//! | `agent_end` | `StreamingCompleted` |
//! | `message_update` (text_delta) | `MessageDelta` |
//! | `message_update` (thinking_delta) | `ReasoningDelta` |
//! | `message_start` | `ItemStarted` |
//! | `message_end` | `ItemCompleted` |
//! | `tool_execution_start` | `ToolCallStarted` |
//! | `tool_execution_update` | `ToolCallUpdate` |
//! | `tool_execution_end` | `ToolCallStarted` → completed |
//! | `toolcall_start` | `ToolCallStarted` |
//! | `toolcall_delta` | `ToolCallUpdate` |
//! | `toolcall_end` | `ToolCallUpdate` (final) |
//! | `response` (correlated) | command result |
//! | `error` | `Error` |

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};

use crate::provider::pi::protocol::{
    PiCommand, PiEvent, deserialize_event, serialize_command,
};
use crate::provider::{ProviderConfig, ProviderEvent, ProviderTransport, SessionInfo};
use crate::transport::{RpcError, TransportError};

/// Channel buffer size for the event broadcast.
const EVENT_CHANNEL_CAPACITY: usize = 256;
/// Channel buffer size for outgoing commands.
const COMMAND_CHANNEL_CAPACITY: usize = 64;
/// Default thread ID used for Pi events (Pi doesn't have threads in the Codex sense).
const DEFAULT_THREAD_ID: &str = "pi-thread";
/// Timeout for waiting on command responses.
const COMMAND_TIMEOUT_SECS: u64 = 30;

/// Message sent from the API surface to the writer task.
enum WriterMessage {
    Write { line: String },
    Shutdown,
}

/// Pending command waiting for a response.
struct PendingCommand {
    /// Sender to deliver the response.
    tx: oneshot::Sender<Result<serde_json::Value, PiTransportError>>,
}

/// Error type for Pi native transport operations.
#[derive(Debug, thiserror::Error)]
pub enum PiTransportError {
    /// The Pi process is not running or not found.
    #[error("Pi process not available: {0}")]
    ProcessNotAvailable(String),

    /// The transport is disconnected.
    #[error("disconnected")]
    Disconnected,

    /// A command timed out.
    #[error("command timed out: {0}")]
    Timeout(String),

    /// The Pi process returned an error.
    #[error("Pi error {code}: {message}")]
    PiError { code: i64, message: String },

    /// A JSONL framing error occurred.
    #[error("JSONL error: {0}")]
    Framing(String),

    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error.
    #[error("serialization error: {0}")]
    Serialization(String),
}

impl From<PiTransportError> for RpcError {
    fn from(err: PiTransportError) -> Self {
        match err {
            PiTransportError::Disconnected | PiTransportError::ProcessNotAvailable(_) => {
                RpcError::Transport(TransportError::Disconnected)
            }
            PiTransportError::PiError { code, message } => RpcError::Server { code, message },
            PiTransportError::Timeout(_) => RpcError::Timeout,
            _ => RpcError::Server {
                code: -1,
                message: err.to_string(),
            },
        }
    }
}

impl From<PiTransportError> for TransportError {
    fn from(err: PiTransportError) -> Self {
        match err {
            PiTransportError::Disconnected => TransportError::Disconnected,
            PiTransportError::ProcessNotAvailable(msg) => {
                TransportError::ConnectionFailed(msg)
            }
            PiTransportError::Timeout(_) => TransportError::Timeout,
            _ => TransportError::ConnectionFailed(err.to_string()),
        }
    }
}

/// Internal shared state for the Pi transport.
struct PiTransportInner {
    /// Whether the transport is connected.
    connected: bool,
    /// Active session ID (from agent_start).
    session_id: Option<String>,
    /// Current message ID being streamed.
    active_message_id: Option<String>,
    /// Active tool execution ID.
    active_execution_id: Option<String>,
    /// Event broadcast sender.
    event_tx: broadcast::Sender<ProviderEvent>,
    /// Peek receiver for `next_event()` non-blocking poll.
    peek_rx: broadcast::Receiver<ProviderEvent>,
    /// Pending command responses: command type → pending sender.
    pending_commands: HashMap<String, PendingCommand>,
}

/// Pi native RPC transport implementing `ProviderTransport`.
///
/// Communicates with a `pi --mode rpc` process over a bidirectional
/// stream (SSH PTY or mock). Uses JSONL framing for message exchange.
pub struct PiNativeTransport {
    inner: Arc<Mutex<PiTransportInner>>,
    /// Channel to send write requests to the writer task.
    writer_tx: mpsc::Sender<WriterMessage>,
    /// Shutdown signal sender.
    shutdown_tx: Option<broadcast::Sender<()>>,
}

impl PiNativeTransport {
    /// Create a new Pi native transport over the given bidirectional stream.
    ///
    /// The stream is typically an SSH channel (for production) or a
    /// `MockPiChannel` (for testing). Spawns background tasks for
    /// reading incoming events and writing outgoing commands.
    pub fn new<T>(stream: T) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let peek_rx = event_tx.subscribe();
        let inner = Arc::new(Mutex::new(PiTransportInner {
            connected: true,
            session_id: None,
            active_message_id: None,
            active_execution_id: None,
            event_tx,
            peek_rx,
            pending_commands: HashMap::new(),
        }));

        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let (writer_tx, writer_rx) = mpsc::channel::<WriterMessage>(COMMAND_CHANNEL_CAPACITY);

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

    /// Send a Pi command and optionally wait for a correlated response.
    async fn send_command(
        &self,
        command: PiCommand,
        expect_response: bool,
    ) -> Result<serde_json::Value, PiTransportError> {
        let command_type = command.command_type().to_string();
        let line = serialize_command(&command)
            .map_err(|e| PiTransportError::Serialization(e.to_string()))?;

        let rx = if expect_response {
            let (tx, rx) = oneshot::channel();
            {
                let mut inner = self.inner.lock().await;
                inner.pending_commands.insert(
                    command_type.clone(),
                    PendingCommand {
                        tx,
                    },
                );
            }
            Some(rx)
        } else {
            None
        };

        // Send the command.
        self.writer_tx
            .send(WriterMessage::Write { line })
            .await
            .map_err(|_| PiTransportError::Disconnected)?;

        if let Some(rx) = rx {
            // Wait for the response with timeout.
            match tokio::time::timeout(
                std::time::Duration::from_secs(COMMAND_TIMEOUT_SECS),
                rx,
            )
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err(PiTransportError::Disconnected),
                Err(_) => {
                    // Timeout — clean up pending command.
                    let mut inner = self.inner.lock().await;
                    inner.pending_commands.remove(&command_type);
                    Err(PiTransportError::Timeout(command_type))
                }
            }
        } else {
            Ok(serde_json::Value::Null)
        }
    }

    /// Send a prompt and return immediately. Events will stream via `event_receiver()`.
    pub async fn prompt(&self, text: &str, session_id: Option<&str>) -> Result<(), RpcError> {
        self.send_command(
            PiCommand::Prompt {
                text: text.to_string(),
                session_id: session_id.map(|s| s.to_string()),
            },
            false,
        )
        .await
        .map_err(RpcError::from)?;
        Ok(())
    }

    /// Abort the active prompt.
    pub async fn abort(&self) -> Result<(), RpcError> {
        self.send_command(PiCommand::Abort, false)
            .await
            .map_err(RpcError::from)?;
        Ok(())
    }

    /// Set the thinking level for subsequent prompts.
    pub async fn set_thinking_level(
        &self,
        level: crate::provider::pi::protocol::PiThinkingLevel,
    ) -> Result<(), RpcError> {
        self.send_command(PiCommand::SetThinkingLevel { level }, true)
            .await
            .map_err(RpcError::from)?;
        Ok(())
    }

    /// Set the model for subsequent prompts.
    pub async fn set_model(&self, model: &str) -> Result<(), RpcError> {
        self.send_command(
            PiCommand::SetModel {
                model: model.to_string(),
            },
            true,
        )
        .await
        .map_err(RpcError::from)?;
        Ok(())
    }

    /// Get the list of available models.
    pub async fn get_available_models(&self) -> Result<Vec<String>, RpcError> {
        let response = self
            .send_command(PiCommand::GetAvailableModels, true)
            .await
            .map_err(RpcError::from)?;
        // Parse model list from response data.
        let models = response
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Ok(models)
    }

    /// Set the steering mode.
    pub async fn set_steering_mode(&self, mode: &str) -> Result<(), RpcError> {
        self.send_command(
            PiCommand::SetSteeringMode {
                mode: mode.to_string(),
            },
            true,
        )
        .await
        .map_err(RpcError::from)?;
        Ok(())
    }

    /// Compact the session.
    pub async fn compact(&self) -> Result<(), RpcError> {
        self.send_command(PiCommand::Compact, true)
            .await
            .map_err(RpcError::from)?;
        Ok(())
    }

    /// Get the current session state.
    pub async fn get_state(&self) -> Result<serde_json::Value, RpcError> {
        self.send_command(PiCommand::GetState, true)
            .await
            .map_err(RpcError::from)
    }

    /// Subscribe to provider events.
    pub fn subscribe(&self) -> broadcast::Receiver<ProviderEvent> {
        match self.inner.try_lock() {
            Ok(guard) => guard.event_tx.subscribe(),
            Err(_) => self.inner.blocking_lock().event_tx.subscribe(),
        }
    }
}

// ── ProviderTransport implementation ────────────────────────────────────────

#[async_trait]
impl ProviderTransport for PiNativeTransport {
    async fn connect(&mut self, _config: &ProviderConfig) -> Result<(), TransportError> {
        // Pi transport connects at construction time (stream is provided).
        // This is a no-op if already connected.
        let inner = self.inner.lock().await;
        if inner.connected {
            Ok(())
        } else {
            Err(TransportError::ConnectionFailed(
                "Pi transport cannot reconnect; create a new instance".to_string(),
            ))
        }
    }

    async fn disconnect(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            drop(tx);
        }

        let mut inner = self.inner.lock().await;
        inner.connected = false;

        // Fail all pending commands.
        for (_, pending) in inner.pending_commands.drain() {
            let _ = pending.tx.send(Err(PiTransportError::Disconnected));
        }

        // Send shutdown to writer.
        let _ = self.writer_tx.send(WriterMessage::Shutdown).await;
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        // Map generic RPC methods to Pi commands.
        match method {
            "prompt" => {
                let text = params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RpcError::Deserialization("missing 'text' field".to_string())
                    })?;
                let session_id = params.get("session_id").and_then(|v| v.as_str());
                self.prompt(text, session_id).await?;
                Ok(serde_json::Value::Null)
            }
            "abort" => {
                self.abort().await?;
                Ok(serde_json::Value::Null)
            }
            "set_thinking_level" => {
                let level_str = params
                    .get("level")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RpcError::Deserialization("missing 'level' field".to_string())
                    })?;
                let level: crate::provider::pi::protocol::PiThinkingLevel =
                    serde_json::from_value(serde_json::Value::String(level_str.to_string()))
                        .map_err(|e| RpcError::Deserialization(e.to_string()))?;
                self.set_thinking_level(level).await?;
                Ok(serde_json::Value::Null)
            }
            "set_model" => {
                let model = params
                    .get("model")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RpcError::Deserialization("missing 'model' field".to_string())
                    })?;
                self.set_model(model).await?;
                Ok(serde_json::Value::Null)
            }
            "get_available_models" => {
                let models = self.get_available_models().await?;
                Ok(serde_json::to_value(models).unwrap_or(serde_json::Value::Null))
            }
            "get_state" => self.get_state().await,
            "set_steering_mode" => {
                let mode = params
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RpcError::Deserialization("missing 'mode' field".to_string())
                    })?;
                self.set_steering_mode(mode).await?;
                Ok(serde_json::Value::Null)
            }
            "compact" => {
                self.compact().await?;
                Ok(serde_json::Value::Null)
            }
            _ => Err(RpcError::Server {
                code: -32601,
                message: format!("unknown Pi RPC method: {method}"),
            }),
        }
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), RpcError> {
        // Pi doesn't distinguish requests from notifications — delegate to send_request.
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
        // Pi session listing is handled by reading ~/.pi/agent/sessions/ via SSH.
        // This is not part of the native RPC protocol, so return empty for now.
        // The full implementation is in the pi-acp-transport feature.
        Ok(Vec::new())
    }

    fn is_connected(&self) -> bool {
        match self.inner.try_lock() {
            Ok(guard) => guard.connected,
            Err(_) => true, // Assume connected if lock is busy.
        }
    }
}

// ── I/O Loop ───────────────────────────────────────────────────────────────

/// Combined read/write loop for the Pi transport.
///
/// Reads incoming JSONL lines from the transport, parses them as Pi events,
/// maps them to `ProviderEvent`s, and broadcasts to subscribers. Also
/// handles command response correlation.
async fn io_loop<T>(
    stream: T,
    inner: Arc<Mutex<PiTransportInner>>,
    mut writer_rx: mpsc::Receiver<WriterMessage>,
    shutdown_rx: &mut broadcast::Receiver<()>,
) where
    T: AsyncRead + AsyncWrite,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut line_buf = String::with_capacity(4096);
    let mut writer_closed = false;

    loop {
        line_buf.clear();

        tokio::select! {
            result = shutdown_rx.recv() => {
                tracing::debug!("pi_transport: shutdown signal received: {result:?}");
                break;
            }
            read_result = reader.read_line(&mut line_buf) => {
                match read_result {
                    Ok(0) => {
                        // EOF — Pi process exited or channel closed.
                        tracing::info!("pi_transport: stream EOF");
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
                        tracing::error!("pi_transport: read error: {e}");
                        handle_disconnect(&inner).await;
                        break;
                    }
                }
            }
            msg = writer_rx.recv(), if !writer_closed => {
                match msg {
                    Some(WriterMessage::Write { line }) => {
                        if let Err(e) = write_half.write_all(line.as_bytes()).await {
                            tracing::error!("pi_transport: write error: {e}");
                            writer_closed = true;
                        } else if let Err(e) = write_half.flush().await {
                            tracing::error!("pi_transport: flush error: {e}");
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

/// Process a single JSONL line from the Pi process.
async fn process_line(inner: &Arc<Mutex<PiTransportInner>>, line: &str) {
    match deserialize_event(line) {
        Ok(Some(event)) => {
            handle_pi_event(inner, event).await;
        }
        Ok(None) => {
            // Empty line — skip.
        }
        Err(e) => {
            // Malformed JSON — log and continue.
            tracing::warn!("pi_transport: malformed JSONL line: {e}, line: {}", &line[..line.len().min(200)]);
            // Continue processing — don't crash.
        }
    }
}

/// Handle a parsed Pi event.
///
/// Maps Pi events to `ProviderEvent`s and broadcasts them.
/// Also handles command response correlation.
async fn handle_pi_event(inner: &Arc<Mutex<PiTransportInner>>, event: PiEvent) {
    match event {
        PiEvent::AgentStart { session_id } => {
            let mut guard = inner.lock().await;
            if let Some(sid) = session_id {
                guard.session_id = Some(sid);
            }
            let _ = guard.event_tx.send(ProviderEvent::StreamingStarted {
                thread_id: DEFAULT_THREAD_ID.to_string(),
            });
        }
        PiEvent::AgentEnd { reason } => {
            let mut guard = inner.lock().await;
            let _ = guard.event_tx.send(ProviderEvent::StreamingCompleted {
                thread_id: DEFAULT_THREAD_ID.to_string(),
            });
            // Resolve any pending prompt with success.
            if let Some(pending) = guard.pending_commands.remove("prompt") {
                let _ = pending.tx.send(Ok(serde_json::json!({
                    "stop_reason": reason.unwrap_or_else(|| "complete".to_string())
                })));
            }
        }
        PiEvent::TurnStart { turn_id } => {
            let guard = inner.lock().await;
            let _ = guard.event_tx.send(ProviderEvent::TurnStarted {
                thread_id: DEFAULT_THREAD_ID.to_string(),
                turn_id: turn_id.unwrap_or_default(),
            });
        }
        PiEvent::TurnEnd { turn_id } => {
            let guard = inner.lock().await;
            let _ = guard.event_tx.send(ProviderEvent::TurnCompleted {
                thread_id: DEFAULT_THREAD_ID.to_string(),
                turn_id: turn_id.unwrap_or_default(),
            });
        }
        PiEvent::MessageStart {
            message_id,
            role,
        } => {
            let mut guard = inner.lock().await;
            if let Some(mid) = &message_id {
                guard.active_message_id = Some(mid.clone());
            }
            let item_id = message_id.unwrap_or_default();
            let _ = guard.event_tx.send(ProviderEvent::ItemStarted {
                thread_id: DEFAULT_THREAD_ID.to_string(),
                turn_id: String::new(),
                item_id: item_id.clone(),
            });
            // Also emit StreamingStarted if this is an assistant message.
            if role.as_deref() == Some("assistant") {
                let _ = guard.event_tx.send(ProviderEvent::MessageDelta {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    item_id,
                    delta: String::new(), // Marker for message start.
                });
            }
        }
        PiEvent::MessageUpdate {
            message_id,
            text_delta,
            thinking_delta,
        } => {
            let guard = inner.lock().await;
            let item_id = message_id.unwrap_or_else(|| {
                guard.active_message_id.clone().unwrap_or_default()
            });

            if let Some(delta) = text_delta
                && !delta.is_empty()
            {
                let _ = guard.event_tx.send(ProviderEvent::MessageDelta {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    item_id: item_id.clone(),
                    delta,
                });
            }
            if let Some(delta) = thinking_delta
                && !delta.is_empty()
            {
                let _ = guard.event_tx.send(ProviderEvent::ReasoningDelta {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    item_id,
                    delta,
                });
            }
        }
        PiEvent::MessageEnd { message_id } => {
            let guard = inner.lock().await;
            let item_id = message_id.unwrap_or_else(|| {
                guard.active_message_id.clone().unwrap_or_default()
            });
            let _ = guard.event_tx.send(ProviderEvent::ItemCompleted {
                thread_id: DEFAULT_THREAD_ID.to_string(),
                turn_id: String::new(),
                item_id,
            });
        }
        PiEvent::ToolCallStart {
            call_id,
            tool_name,
            input,
        } => {
            let guard = inner.lock().await;
            let _ = guard.event_tx.send(ProviderEvent::ToolCallStarted {
                thread_id: DEFAULT_THREAD_ID.to_string(),
                item_id: call_id.unwrap_or_default(),
                tool_name: tool_name.unwrap_or_default(),
                call_id: String::new(), // Pi doesn't have separate call IDs
            });
            // If there's input, emit it as an update.
            if let Some(input) = input
                && !input.is_empty()
            {
                let _ = guard.event_tx.send(ProviderEvent::ToolCallUpdate {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    item_id: String::new(),
                    call_id: String::new(),
                    output_delta: input,
                });
            }
        }
        PiEvent::ToolCallDelta {
            call_id: _,
            content_delta,
        } => {
            let guard = inner.lock().await;
            if let Some(delta) = content_delta
                && !delta.is_empty()
            {
                let _ = guard.event_tx.send(ProviderEvent::ToolCallUpdate {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    item_id: String::new(),
                    call_id: String::new(),
                    output_delta: delta,
                });
            }
        }
        PiEvent::ToolCallEnd {
            call_id: _,
            output,
        } => {
            let guard = inner.lock().await;
            if let Some(output) = output
                && !output.is_empty()
            {
                let _ = guard.event_tx.send(ProviderEvent::ToolCallUpdate {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    item_id: String::new(),
                    call_id: String::new(),
                    output_delta: output,
                });
            }
        }
        PiEvent::ToolExecutionStart {
            execution_id,
            command,
            ..
        } => {
            let mut guard = inner.lock().await;
            let exec_id = execution_id.clone().unwrap_or_default();
            guard.active_execution_id = Some(exec_id.clone());
            let _ = guard.event_tx.send(ProviderEvent::ToolCallStarted {
                thread_id: DEFAULT_THREAD_ID.to_string(),
                item_id: exec_id,
                tool_name: command.unwrap_or_default(),
                call_id: String::new(),
            });
        }
        PiEvent::ToolExecutionUpdate {
            execution_id: _,
            stdout,
            stderr,
        } => {
            let guard = inner.lock().await;
            let exec_id = guard.active_execution_id.clone().unwrap_or_default();
            if let Some(delta) = stdout
                && !delta.is_empty()
            {
                let _ = guard.event_tx.send(ProviderEvent::CommandOutputDelta {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    item_id: exec_id.clone(),
                    delta,
                });
            }
            if let Some(delta) = stderr
                && !delta.is_empty()
            {
                let _ = guard.event_tx.send(ProviderEvent::CommandOutputDelta {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    item_id: exec_id,
                    delta: format!("[stderr] {delta}"),
                });
            }
        }
        PiEvent::ToolExecutionEnd {
            execution_id: _,
            exit_code,
        } => {
            let mut guard = inner.lock().await;
            let exec_id = guard.active_execution_id.clone().unwrap_or_default();
            guard.active_execution_id = None;
            // Emit completion as a ToolCallUpdate with exit code info.
            let _ = guard.event_tx.send(ProviderEvent::ToolCallUpdate {
                thread_id: DEFAULT_THREAD_ID.to_string(),
                item_id: exec_id,
                call_id: String::new(),
                output_delta: format!("\n[exit code: {}]", exit_code.unwrap_or(0)),
            });
        }
        PiEvent::Response {
            command,
            data,
            error,
        } => {
            let mut guard = inner.lock().await;
            let cmd_type = command.unwrap_or_default();

            // Try to find a pending command for this response.
            if let Some(pending) = guard.pending_commands.remove(&cmd_type) {
                if let Some(err_msg) = error {
                    let _ = pending.tx.send(Err(PiTransportError::PiError {
                        code: -1,
                        message: err_msg,
                    }));
                } else {
                    let _ = pending.tx.send(Ok(data.unwrap_or(serde_json::Value::Null)));
                }
            }
            // If no pending command, it's an unsolicited response — ignore.
        }
        PiEvent::Error { message, code } => {
            let guard = inner.lock().await;
            let _ = guard.event_tx.send(ProviderEvent::Error {
                message: message.unwrap_or_else(|| "unknown Pi error".to_string()),
                code,
            });
        }
    }
}

/// Handle transport disconnection.
async fn handle_disconnect(inner: &Arc<Mutex<PiTransportInner>>) {
    let mut guard = inner.lock().await;
    guard.connected = false;
    let _ = guard.event_tx.send(ProviderEvent::Disconnected {
        message: "Pi process exited".to_string(),
    });
    // Fail all pending commands.
    for (_, pending) in guard.pending_commands.drain() {
        let _ = pending.tx.send(Err(PiTransportError::Disconnected));
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::pi::mock::MockPiChannel;
    use crate::provider::pi::protocol::PiThinkingLevel;

    /// Helper: create transport + mock channel pair.
    fn setup() -> (PiNativeTransport, MockPiChannel) {
        let mock = MockPiChannel::new();
        let transport = PiNativeTransport::new(mock.clone());
        (transport, mock)
    }

    // ── VAL-PI-002: JSONL framing and command/response correlation ─────

    #[tokio::test]
    async fn pi_native_jsonl_framing() {
        let (transport, mock) = setup();

        // Queue a response for get_state.
        mock.queue_command_response("get_state", serde_json::json!({"status": "idle"}))
            .await;

        // Send get_state command.
        let result = transport.get_state().await.unwrap();
        assert_eq!(result["status"], "idle");

        // Verify the command was sent as valid JSONL.
        let commands = mock.captured_commands().await;
        assert!(!commands.is_empty());
        let cmd: serde_json::Value = serde_json::from_str(&commands[0]).unwrap();
        assert_eq!(cmd["type"], "get_state");
    }

    // ── VAL-PI-003: text_delta → MessageDelta mapping ──────────────────

    #[tokio::test]
    async fn pi_text_delta_mapping() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        mock.queue_simple_response(&["Hello", " world"]).await;

        // Let the IO loop process events.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Collect events.
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // Find MessageDelta events.
        let text_deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::MessageDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .filter(|d| !d.is_empty())
            .collect();

        assert!(text_deltas.contains(&"Hello"), "should contain 'Hello'");
        assert!(text_deltas.contains(&" world"), "should contain ' world'");
    }

    // ── VAL-PI-004: thinking_delta → ReasoningDelta mapping ────────────

    #[tokio::test]
    async fn pi_thinking_delta_mapping() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        mock.queue_response_with_thinking("Let me analyze...", "Here is the answer")
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        let reasoning_deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::ReasoningDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect();

        assert!(
            reasoning_deltas.iter().any(|d| d.contains("Let me analyze")),
            "should contain reasoning delta"
        );

        let text_deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::MessageDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .filter(|d| !d.is_empty())
            .collect();

        assert!(
            text_deltas.iter().any(|d| d.contains("answer")),
            "should contain text delta"
        );
    }

    // ── VAL-PI-005: tool_execution events → CommandExecution mapping ───

    #[tokio::test]
    async fn pi_tool_execution_mapping() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        mock.queue_response_with_tool_execution("git status", "On branch main\n", "Done")
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // Should see ToolCallStarted for tool execution.
        let tool_started = events.iter().any(|e| matches!(
            e,
            ProviderEvent::ToolCallStarted {
                tool_name,
                ..
            } if tool_name == "git status"
        ));
        assert!(tool_started, "should emit ToolCallStarted for 'git status'");

        // Should see CommandOutputDelta for stdout.
        let output_deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::CommandOutputDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect();

        assert!(
            output_deltas.iter().any(|d| d.contains("On branch main")),
            "should contain command output delta"
        );
    }

    // ── VAL-PI-006: Pi-specific features ────────────────────────────────

    #[tokio::test]
    async fn pi_native_set_thinking_level() {
        let (transport, mock) = setup();

        mock.queue_command_response("set_thinking_level", serde_json::json!({"ok": true}))
            .await;

        transport.set_thinking_level(PiThinkingLevel::High).await.unwrap();

        let commands = mock.captured_commands().await;
        let cmd: serde_json::Value = serde_json::from_str(&commands[0]).unwrap();
        assert_eq!(cmd["type"], "set_thinking_level");
        assert_eq!(cmd["level"], "high");
    }

    #[tokio::test]
    async fn pi_native_set_model() {
        let (transport, mock) = setup();

        mock.queue_command_response("set_model", serde_json::json!({"ok": true}))
            .await;

        transport
            .set_model("claude-sonnet-4-20250514")
            .await
            .unwrap();

        let commands = mock.captured_commands().await;
        let cmd: serde_json::Value = serde_json::from_str(&commands[0]).unwrap();
        assert_eq!(cmd["type"], "set_model");
        assert_eq!(cmd["model"], "claude-sonnet-4-20250514");
    }

    #[tokio::test]
    async fn pi_native_get_available_models() {
        let (transport, mock) = setup();

        mock.queue_command_response(
            "get_available_models",
            serde_json::json!(["claude-sonnet-4-20250514", "gpt-4.1"]),
        )
        .await;

        let models = transport.get_available_models().await.unwrap();
        assert_eq!(models.len(), 2);
        assert!(models.contains(&"claude-sonnet-4-20250514".to_string()));
    }

    #[tokio::test]
    async fn pi_native_set_steering_mode() {
        let (transport, mock) = setup();

        mock.queue_command_response("set_steering_mode", serde_json::json!({"ok": true}))
            .await;

        transport.set_steering_mode("auto").await.unwrap();

        let commands = mock.captured_commands().await;
        let cmd: serde_json::Value = serde_json::from_str(&commands[0]).unwrap();
        assert_eq!(cmd["type"], "set_steering_mode");
        assert_eq!(cmd["mode"], "auto");
    }

    #[tokio::test]
    async fn pi_native_compact() {
        let (transport, mock) = setup();

        mock.queue_command_response("compact", serde_json::json!({"ok": true}))
            .await;

        transport.compact().await.unwrap();

        let commands = mock.captured_commands().await;
        let cmd: serde_json::Value = serde_json::from_str(&commands[0]).unwrap();
        assert_eq!(cmd["type"], "compact");
    }

    // ── VAL-PI-009: Pi process crash ────────────────────────────────────

    #[tokio::test]
    async fn pi_process_crash() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Queue some events then simulate crash.
        mock.queue_event(r#"{"type":"agent_start"}"#).await;
        mock.queue_event(r#"{"type":"message_start","message_id":"msg-1"}"#)
            .await;
        mock.queue_event(r#"{"type":"message_update","message_id":"msg-1","text_delta":"Hello"}"#)
            .await;
        // Don't queue agent_end — simulate crash by disconnecting.
        mock.simulate_disconnect().await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Should have received a Disconnected event.
        let mut got_disconnect = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ProviderEvent::Disconnected { .. }) {
                got_disconnect = true;
            }
        }
        assert!(got_disconnect, "should emit Disconnected event on crash");

        // Transport should report disconnected.
        assert!(!transport.is_connected());
    }

    // ── VAL-PI-010: SSH disconnect reconnect path ──────────────────────

    #[tokio::test]
    async fn pi_ssh_disconnect_emits_event() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Start streaming then disconnect.
        mock.queue_event(r#"{"type":"agent_start"}"#).await;
        mock.simulate_disconnect().await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut got_disconnect = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ProviderEvent::Disconnected { .. }) {
                got_disconnect = true;
            }
        }
        assert!(got_disconnect, "should detect SSH disconnect");
    }

    // ── VAL-PI-011: Malformed JSONL ─────────────────────────────────────

    #[tokio::test]
    async fn pi_malformed_jsonl() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Queue a mix of valid and invalid lines.
        mock.queue_events(&[
            r#"{"type":"agent_start"}"#,           // valid
            r#"{broken json"#,                       // malformed — should be skipped
            r#"{"type":"future_event","data":{}}"#,  // unknown type — may or may not parse
            r#"{"type":"agent_end","reason":"ok"}"#, // valid
        ])
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Should get StreamingStarted and StreamingCompleted (valid events).
        let mut got_start = false;
        let mut got_end = false;
        while let Ok(event) = rx.try_recv() {
            match &event {
                ProviderEvent::StreamingStarted { .. } => got_start = true,
                ProviderEvent::StreamingCompleted { .. } => got_end = true,
                _ => {}
            }
        }
        assert!(got_start, "should process valid agent_start");
        assert!(got_end, "should process valid agent_end after malformed line");
    }

    #[tokio::test]
    async fn pi_malformed_jsonl_unknown_event_type() {
        let (transport, mock) = setup();

        // Queue a line with valid JSON but unknown event type.
        // Since our PiEvent uses #[serde(tag = "type")], unknown types
        // will fail deserialization — this is expected behavior (skip + log).
        mock.queue_event(r#"{"type":"unknown_new_event","data":"test"}"#)
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Should not panic or crash.
        assert!(transport.is_connected());
    }

    // ── VAL-PI-012: Full event lifecycle ────────────────────────────────

    #[tokio::test]
    async fn pi_full_lifecycle_mapping() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Full lifecycle with text, thinking, and tool execution.
        mock.queue_events(&[
            r#"{"type":"agent_start","session_id":"sess-1"}"#,
            r#"{"type":"turn_start","turn_id":"turn-1"}"#,
            r#"{"type":"message_start","message_id":"msg-1","role":"assistant"}"#,
            r#"{"type":"message_update","message_id":"msg-1","thinking_delta":"Analyzing..."}"#,
            r#"{"type":"message_update","message_id":"msg-1","text_delta":"I will check the files."}"#,
            r#"{"type":"message_end","message_id":"msg-1"}"#,
            r#"{"type":"tool_execution_start","execution_id":"exec-1","command":"ls -la"}"#,
            r#"{"type":"tool_execution_update","execution_id":"exec-1","stdout":"file1.txt\n"}"#,
            r#"{"type":"tool_execution_end","execution_id":"exec-1","exit_code":0}"#,
            r#"{"type":"turn_end","turn_id":"turn-1"}"#,
            r#"{"type":"agent_end","reason":"complete"}"#,
        ])
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // Verify complete lifecycle.
        let event_types: Vec<String> = events
            .iter()
            .map(|e| match e {
                ProviderEvent::StreamingStarted { .. } => "StreamingStarted".to_string(),
                ProviderEvent::StreamingCompleted { .. } => "StreamingCompleted".to_string(),
                ProviderEvent::TurnStarted { .. } => "TurnStarted".to_string(),
                ProviderEvent::TurnCompleted { .. } => "TurnCompleted".to_string(),
                ProviderEvent::ItemStarted { .. } => "ItemStarted".to_string(),
                ProviderEvent::ItemCompleted { .. } => "ItemCompleted".to_string(),
                ProviderEvent::MessageDelta { .. } => "MessageDelta".to_string(),
                ProviderEvent::ReasoningDelta { .. } => "ReasoningDelta".to_string(),
                ProviderEvent::ToolCallStarted { .. } => "ToolCallStarted".to_string(),
                ProviderEvent::CommandOutputDelta { .. } => "CommandOutputDelta".to_string(),
                ProviderEvent::ToolCallUpdate { .. } => "ToolCallUpdate".to_string(),
                _ => "Other".to_string(),
            })
            .collect();

        assert!(
            event_types.contains(&"StreamingStarted".to_string()),
            "should have StreamingStarted"
        );
        assert!(
            event_types.contains(&"TurnStarted".to_string()),
            "should have TurnStarted"
        );
        assert!(
            event_types.contains(&"ReasoningDelta".to_string()),
            "should have ReasoningDelta"
        );
        assert!(
            event_types.contains(&"MessageDelta".to_string()),
            "should have MessageDelta"
        );
        assert!(
            event_types.contains(&"ItemCompleted".to_string()),
            "should have ItemCompleted"
        );
        assert!(
            event_types.contains(&"ToolCallStarted".to_string()),
            "should have ToolCallStarted"
        );
        assert!(
            event_types.contains(&"CommandOutputDelta".to_string()),
            "should have CommandOutputDelta"
        );
        assert!(
            event_types.contains(&"StreamingCompleted".to_string()),
            "should have StreamingCompleted"
        );
    }

    // ── Pi binary not found ──────────────────────────────────────────────

    #[tokio::test]
    async fn pi_binary_not_found() {
        let mock = MockPiChannel::new_pi_not_found();
        let transport = PiNativeTransport::new(mock.clone());
        let mut rx = transport.subscribe();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Should get Disconnected event (EOF on empty stream).
        let mut got_disconnect = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ProviderEvent::Disconnected { .. }) {
                got_disconnect = true;
            }
        }
        assert!(got_disconnect, "should detect pi not found as disconnect");
        assert!(!transport.is_connected());
    }

    // ── ProviderTransport trait compliance ───────────────────────────────

    #[tokio::test]
    async fn pi_provider_transport_disconnect_idempotent() {
        let (mut transport, _mock) = setup();
        transport.disconnect().await;
        // Second disconnect should not panic.
        transport.disconnect().await;
    }

    #[tokio::test]
    async fn pi_provider_transport_post_disconnect_request_fails() {
        let (mut transport, _mock) = setup();
        transport.disconnect().await;

        let result = transport
            .send_request("get_state", serde_json::Value::Null)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn pi_provider_transport_send_request_prompt() {
        let (mut transport, mock) = setup();
        let mut rx = transport.subscribe();

        mock.queue_simple_response(&["test response"]).await;

        let result = transport
            .send_request(
                "prompt",
                serde_json::json!({"text": "hello"}),
            )
            .await;
        assert!(result.is_ok());

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Should have streaming events.
        let mut got_start = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ProviderEvent::StreamingStarted { .. }) {
                got_start = true;
            }
        }
        assert!(got_start);
    }

    #[tokio::test]
    async fn pi_provider_transport_send_request_abort() {
        let (mut transport, _mock) = setup();

        let result = transport
            .send_request("abort", serde_json::Value::Null)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn pi_provider_transport_send_request_unknown_method() {
        let (mut transport, _mock) = setup();

        let result = transport
            .send_request("nonexistent_method", serde_json::Value::Null)
            .await;
        assert!(result.is_err());
        match result {
            Err(RpcError::Server { code, message }) => {
                assert_eq!(code, -32601);
                assert!(message.contains("nonexistent_method"));
            }
            _ => panic!("expected Server error with code -32601"),
        }
    }

    #[tokio::test]
    async fn pi_provider_transport_list_sessions() {
        let (mut transport, _mock) = setup();
        let sessions = transport.list_sessions().await.unwrap();
        assert!(sessions.is_empty()); // Pi native doesn't support session listing via RPC.
    }

    #[tokio::test]
    async fn pi_provider_transport_is_connected() {
        let (transport, _mock) = setup();
        assert!(transport.is_connected());
    }

    // ── Thinking levels comprehensive ────────────────────────────────────

    #[tokio::test]
    async fn pi_thinking_level_all_values() {
        let levels = PiThinkingLevel::all();
        assert_eq!(levels.len(), 6);

        for level in levels {
            let (transport, mock) = setup();
            mock.queue_command_response("set_thinking_level", serde_json::json!({"ok": true}))
                .await;

            transport.set_thinking_level(*level).await.unwrap();

            let commands = mock.captured_commands().await;
            let cmd: serde_json::Value = serde_json::from_str(&commands[0]).unwrap();
            assert_eq!(cmd["type"], "set_thinking_level");
        }
    }

    // ── Abort command ────────────────────────────────────────────────────

    #[tokio::test]
    async fn pi_abort_command_sent() {
        let (transport, mock) = setup();

        transport.abort().await.unwrap();

        // Give the IO loop time to process the write.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let commands = mock.captured_commands().await;
        assert!(!commands.is_empty(), "should have captured at least one command");
        let cmd_str = &commands[0];
        assert!(cmd_str.contains("abort"));
    }

    // ── Error response handling ──────────────────────────────────────────

    #[tokio::test]
    async fn pi_error_response_mapped_to_provider_event() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        mock.queue_error("something went wrong", 42).await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut got_error = false;
        while let Ok(event) = rx.try_recv() {
            if let ProviderEvent::Error { message, code } = &event {
                assert_eq!(message, "something went wrong");
                assert_eq!(*code, Some(42));
                got_error = true;
            }
        }
        assert!(got_error, "should map Pi error event to ProviderEvent::Error");
    }
}
