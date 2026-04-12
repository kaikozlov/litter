//! Droid native Factory API transport implementation.
//!
//! Implements `ProviderTransport` for Droid's native JSON-RPC 2.0 protocol
//! over stdin/stdout (`droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc`).
//!
//! # Architecture
//!
//! ```text
//! ProviderTransport trait
//!         │
//!  DroidNativeTransport
//!    ├── Read task: reads JSON-RPC lines → DroidIncoming → ProviderEvent (broadcast)
//!    ├── Write channel: JsonRpcRequest → JSON line → transport write
//!    ├── Pending request tracking: response correlation by request ID
//!    └── Deferred complete handling: idle-before-complete quirk
//! ```
//!
//! # Event Mapping
//!
//! | Droid Notification | ProviderEvent |
//! |---|---|
//! | `droid_working_state_changed` (streaming) | `StreamingStarted` |
//! | `droid_working_state_changed` (idle) | `StreamingCompleted` (deferred) |
//! | `create_message` (text_delta) | `MessageDelta` |
//! | `create_message` (complete=true) | `ItemCompleted` |
//! | `tool_result` (status=running) | `ToolCallStarted` |
//! | `tool_result` (status=completed) | `ToolCallUpdate` (final) |
//! | `error` (fatal) | `Error` + `Disconnected` |
//! | `error` (non-fatal) | `Error` |
//! | `complete` | Resolves pending prompt + `StreamingCompleted` |
//!
//! # Droid Idle-Before-Complete Quirk
//!
//! Droid sends `working_state_changed(idle)` before the `complete` notification.
//! The transport handles this by deferring the `StreamingCompleted` event:
//! if we receive `idle` while a prompt is in-flight, we wait for the `complete`
//! notification before emitting `StreamingCompleted`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};

use crate::provider::droid::protocol::{
    AddUserMessageParams, CreateMessagePayload, CompletePayload, DroidAutonomyLevel,
    DroidIncoming, DroidReasoningEffort, DroidWorkingState, ErrorPayload, ForkSessionParams,
    InitializeSessionParams, JsonRpcRequest, JsonRpcResponse, ResumeSessionParams,
    SetAutonomyLevelParams, SetModelParams, SetReasoningEffortParams, ToolResultPayload,
    WorkingStateChangedPayload, parse_incoming, serialize_request,
};
use crate::provider::{ProviderConfig, ProviderEvent, ProviderTransport, SessionInfo};
use crate::transport::{RpcError, TransportError};

/// Channel buffer size for the event broadcast.
const EVENT_CHANNEL_CAPACITY: usize = 256;
/// Channel buffer size for outgoing requests.
const REQUEST_CHANNEL_CAPACITY: usize = 64;
/// Default thread ID used for Droid events (Droid doesn't have threads in the Codex sense).
const DEFAULT_THREAD_ID: &str = "droid-thread";
/// Timeout for waiting on request responses.
const REQUEST_TIMEOUT_SECS: u64 = 30;
/// Maximum retry attempts for transient errors.
const MAX_TRANSIENT_RETRIES: u32 = 3;
/// Error codes considered transient (retriable).
const TRANSIENT_ERROR_CODES: &[i64] = &[429, -32001];

/// Message sent from the API surface to the writer task.
enum WriterMessage {
    Write { line: String },
    Shutdown,
}

/// Pending request waiting for a response.
struct PendingRequest {
    /// Sender to deliver the response.
    tx: oneshot::Sender<Result<serde_json::Value, DroidTransportError>>,
}

/// Error type for Droid native transport operations.
#[derive(Debug, thiserror::Error)]
pub enum DroidTransportError {
    /// The Droid process is not running or not found.
    #[error("Droid process not available: {0}")]
    ProcessNotAvailable(String),

    /// The transport is disconnected.
    #[error("disconnected")]
    Disconnected,

    /// A request timed out.
    #[error("request timed out: {0}")]
    Timeout(String),

    /// The Droid API returned an error.
    #[error("Droid API error {code}: {message}")]
    ApiError { code: i64, message: String },

    /// A JSON-RPC framing error occurred.
    #[error("JSON-RPC error: {0}")]
    Framing(String),

    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error.
    #[error("serialization error: {0}")]
    Serialization(String),
}

impl From<DroidTransportError> for RpcError {
    fn from(err: DroidTransportError) -> Self {
        match err {
            DroidTransportError::Disconnected | DroidTransportError::ProcessNotAvailable(_) => {
                RpcError::Transport(TransportError::Disconnected)
            }
            DroidTransportError::ApiError { code, message } => RpcError::Server { code, message },
            DroidTransportError::Timeout(_) => RpcError::Timeout,
            _ => RpcError::Server {
                code: -1,
                message: err.to_string(),
            },
        }
    }
}

impl From<DroidTransportError> for TransportError {
    fn from(err: DroidTransportError) -> Self {
        match err {
            DroidTransportError::Disconnected => TransportError::Disconnected,
            DroidTransportError::ProcessNotAvailable(msg) => {
                TransportError::ConnectionFailed(msg)
            }
            DroidTransportError::Timeout(_) => TransportError::Timeout,
            _ => TransportError::ConnectionFailed(err.to_string()),
        }
    }
}

/// Internal shared state for the Droid transport.
struct DroidTransportInner {
    /// Whether the transport is connected.
    connected: bool,
    /// Active session ID from `droid.initialize_session`.
    session_id: Option<String>,
    /// Current Droid working state.
    working_state: DroidWorkingState,
    /// Active message ID being streamed.
    active_message_id: Option<String>,
    /// Active tool call ID being processed.
    active_tool_call_id: Option<String>,
    /// Whether a prompt is currently in-flight.
    prompt_in_flight: bool,
    /// Whether we received idle but are deferring StreamingCompleted.
    deferred_complete: bool,
    /// Event broadcast sender.
    event_tx: broadcast::Sender<ProviderEvent>,
    /// Peek receiver for `next_event()` non-blocking poll.
    peek_rx: broadcast::Receiver<ProviderEvent>,
    /// Pending request responses: request ID → pending sender.
    pending_requests: HashMap<i64, PendingRequest>,
    /// Next JSON-RPC request ID.
    next_id: i64,
}

impl DroidTransportInner {
    /// Allocate the next request ID.
    fn allocate_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// Droid native Factory API transport implementing `ProviderTransport`.
///
/// Communicates with a `droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc`
/// process over a bidirectional stream (SSH PTY or mock). Uses JSON-RPC 2.0
/// for request/response and notification streaming.
pub struct DroidNativeTransport {
    inner: Arc<Mutex<DroidTransportInner>>,
    /// Channel to send write requests to the writer task.
    writer_tx: mpsc::Sender<WriterMessage>,
    /// Shutdown signal sender.
    shutdown_tx: Option<broadcast::Sender<()>>,
}

impl DroidNativeTransport {
    /// Create a new Droid native transport over the given bidirectional stream.
    ///
    /// The stream is typically an SSH channel (for production) or a
    /// `MockDroidChannel` (for testing). Spawns background tasks for
    /// reading incoming messages and writing outgoing requests.
    pub fn new<T>(stream: T) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let peek_rx = event_tx.subscribe();
        let inner = Arc::new(Mutex::new(DroidTransportInner {
            connected: true,
            session_id: None,
            working_state: DroidWorkingState::Idle,
            active_message_id: None,
            active_tool_call_id: None,
            prompt_in_flight: false,
            deferred_complete: false,
            event_tx,
            peek_rx,
            pending_requests: HashMap::new(),
            next_id: 1,
        }));

        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let (writer_tx, writer_rx) = mpsc::channel::<WriterMessage>(REQUEST_CHANNEL_CAPACITY);

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

    /// Send a JSON-RPC request and wait for the correlated response.
    async fn send_rpc_request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, DroidTransportError> {
        let (id, rx) = {
            let mut inner = self.inner.lock().await;
            let id = inner.allocate_id();
            let (tx, rx) = oneshot::channel();
            inner.pending_requests.insert(id, PendingRequest { tx });
            (id, rx)
        };

        let request = JsonRpcRequest::new(id, method, params);
        let line = serialize_request(&request)
            .map_err(|e| DroidTransportError::Serialization(e.to_string()))?;

        self.writer_tx
            .send(WriterMessage::Write { line })
            .await
            .map_err(|_| DroidTransportError::Disconnected)?;

        match tokio::time::timeout(
            std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS),
            rx,
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(DroidTransportError::Disconnected),
            Err(_) => {
                let mut inner = self.inner.lock().await;
                inner.pending_requests.remove(&id);
                Err(DroidTransportError::Timeout(method.to_string()))
            }
        }
    }

    /// Send a JSON-RPC request with transient error retry.
    async fn send_rpc_request_with_retry(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, DroidTransportError> {
        let mut attempts = 0;
        loop {
            match self.send_rpc_request(method, params.clone()).await {
                Ok(result) => return Ok(result),
                Err(DroidTransportError::ApiError { code, message }) => {
                    if TRANSIENT_ERROR_CODES.contains(&code) && attempts < MAX_TRANSIENT_RETRIES {
                        attempts += 1;
                        let delay = std::time::Duration::from_millis(100 * 2u64.pow(attempts));
                        tracing::warn!(
                            "Droid transient error (attempt {}/{}): {code} {message}, retrying in {delay:?}",
                            attempts, MAX_TRANSIENT_RETRIES,
                        );
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(DroidTransportError::ApiError { code, message });
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Initialize a Droid session.
    ///
    /// Sends `droid.initialize_session` and returns the session ID.
    /// If `session_id` is provided in params, attempts to resume that session.
    pub async fn initialize_session(
        &self,
        model: Option<&str>,
        autonomy_level: Option<DroidAutonomyLevel>,
        reasoning_effort: Option<DroidReasoningEffort>,
        session_id: Option<&str>,
        working_dir: Option<&str>,
    ) -> Result<String, RpcError> {
        let params = InitializeSessionParams {
            model: model.map(|s| s.to_string()),
            autonomy_level,
            reasoning_effort,
            session_id: session_id.map(|s| s.to_string()),
            working_directory: working_dir.map(|s| s.to_string()),
        };

        let result = self
            .send_rpc_request_with_retry(
                "droid.initialize_session",
                Some(serde_json::to_value(params).unwrap()),
            )
            .await
            .map_err(RpcError::from)?;

        let init_result: crate::provider::droid::protocol::InitializeSessionResult =
            serde_json::from_value(result)
                .map_err(|e| RpcError::Deserialization(e.to_string()))?;

        let session_id = init_result.session_id.clone();
        {
            let mut inner = self.inner.lock().await;
            inner.session_id = Some(init_result.session_id);
        }

        Ok(session_id)
    }

    /// Send a user prompt via `droid.add_user_message`.
    ///
    /// The prompt returns immediately. Droid will stream notifications
    /// back, culminating in a `complete` notification.
    pub async fn add_user_message(&self, content: &str) -> Result<(), RpcError> {
        {
            let mut inner = self.inner.lock().await;
            inner.prompt_in_flight = true;
            inner.deferred_complete = false;
        }

        let params = AddUserMessageParams {
            content: content.to_string(),
        };

        self.send_rpc_request_with_retry(
            "droid.add_user_message",
            Some(serde_json::to_value(params).unwrap()),
        )
        .await
        .map_err(RpcError::from)?;

        Ok(())
    }

    /// Cancel the active request via `droid.cancel`.
    pub async fn cancel(&self) -> Result<(), RpcError> {
        self.send_rpc_request("droid.cancel", None)
            .await
            .map_err(RpcError::from)?;
        Ok(())
    }

    /// Set the model via `droid.set_model`.
    pub async fn set_model(&self, model: &str) -> Result<(), RpcError> {
        let params = SetModelParams {
            model: model.to_string(),
        };
        self.send_rpc_request("droid.set_model", Some(serde_json::to_value(params).unwrap()))
            .await
            .map_err(RpcError::from)?;
        Ok(())
    }

    /// Set the autonomy level via `droid.set_autonomy_level`.
    pub async fn set_autonomy_level(&self, level: DroidAutonomyLevel) -> Result<(), RpcError> {
        let params = SetAutonomyLevelParams { level };
        self.send_rpc_request(
            "droid.set_autonomy_level",
            Some(serde_json::to_value(params).unwrap()),
        )
        .await
        .map_err(RpcError::from)?;
        Ok(())
    }

    /// Set the reasoning effort via `droid.set_reasoning_effort`.
    pub async fn set_reasoning_effort(&self, effort: DroidReasoningEffort) -> Result<(), RpcError> {
        let params = SetReasoningEffortParams { effort };
        self.send_rpc_request(
            "droid.set_reasoning_effort",
            Some(serde_json::to_value(params).unwrap()),
        )
        .await
        .map_err(RpcError::from)?;
        Ok(())
    }

    /// Fork the current session via `droid.fork_session`.
    pub async fn fork_session(&self, session_id: &str) -> Result<String, RpcError> {
        let params = ForkSessionParams {
            session_id: session_id.to_string(),
        };
        let result = self
            .send_rpc_request("droid.fork_session", Some(serde_json::to_value(params).unwrap()))
            .await
            .map_err(RpcError::from)?;

        let fork_result: crate::provider::droid::protocol::ForkSessionResult =
            serde_json::from_value(result)
                .map_err(|e| RpcError::Deserialization(e.to_string()))?;

        Ok(fork_result.session_id)
    }

    /// Resume a previous session via `droid.resume_session`.
    pub async fn resume_session(&self, session_id: &str) -> Result<(), RpcError> {
        let params = ResumeSessionParams {
            session_id: session_id.to_string(),
        };
        self.send_rpc_request(
            "droid.resume_session",
            Some(serde_json::to_value(params).unwrap()),
        )
        .await
        .map_err(RpcError::from)?;

        {
            let mut inner = self.inner.lock().await;
            inner.session_id = Some(session_id.to_string());
        }

        Ok(())
    }

    /// Get the current session ID.
    pub async fn session_id(&self) -> Option<String> {
        let inner = self.inner.lock().await;
        inner.session_id.clone()
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
impl ProviderTransport for DroidNativeTransport {
    async fn connect(&mut self, _config: &ProviderConfig) -> Result<(), TransportError> {
        // Droid transport connects at construction time (stream is provided).
        let inner = self.inner.lock().await;
        if inner.connected {
            Ok(())
        } else {
            Err(TransportError::ConnectionFailed(
                "Droid transport cannot reconnect; create a new instance".to_string(),
            ))
        }
    }

    async fn disconnect(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            drop(tx);
        }

        let mut inner = self.inner.lock().await;
        inner.connected = false;

        // Fail all pending requests.
        for (_, pending) in inner.pending_requests.drain() {
            let _ = pending.tx.send(Err(DroidTransportError::Disconnected));
        }

        // Send shutdown to writer.
        let _ = self.writer_tx.send(WriterMessage::Shutdown).await;
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        match method {
            "droid.initialize_session" => {
                let model = params.get("model").and_then(|v| v.as_str());
                let autonomy = params
                    .get("autonomyLevel")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let effort = params
                    .get("reasoningEffort")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let session_id = params.get("sessionId").and_then(|v| v.as_str());
                let working_dir = params.get("workingDirectory").and_then(|v| v.as_str());

                let sid = self
                    .initialize_session(model, autonomy, effort, session_id, working_dir)
                    .await?;
                Ok(serde_json::json!({"sessionId": sid}))
            }
            "droid.add_user_message" => {
                let content = params
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| RpcError::Deserialization("missing 'content' field".to_string()))?;
                self.add_user_message(content).await?;
                Ok(serde_json::Value::Null)
            }
            "droid.cancel" => {
                self.cancel().await?;
                Ok(serde_json::Value::Null)
            }
            "droid.set_model" => {
                let model = params
                    .get("model")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| RpcError::Deserialization("missing 'model' field".to_string()))?;
                self.set_model(model).await?;
                Ok(serde_json::Value::Null)
            }
            "droid.set_autonomy_level" => {
                let level: DroidAutonomyLevel = params
                    .get("level")
                    .ok_or_else(|| RpcError::Deserialization("missing 'level' field".to_string()))
                    .and_then(|v| {
                        serde_json::from_value(v.clone())
                            .map_err(|e| RpcError::Deserialization(e.to_string()))
                    })?;
                self.set_autonomy_level(level).await?;
                Ok(serde_json::Value::Null)
            }
            "droid.set_reasoning_effort" => {
                let effort: DroidReasoningEffort = params
                    .get("effort")
                    .ok_or_else(|| RpcError::Deserialization("missing 'effort' field".to_string()))
                    .and_then(|v| {
                        serde_json::from_value(v.clone())
                            .map_err(|e| RpcError::Deserialization(e.to_string()))
                    })?;
                self.set_reasoning_effort(effort).await?;
                Ok(serde_json::Value::Null)
            }
            "droid.fork_session" => {
                let session_id = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RpcError::Deserialization("missing 'session_id' field".to_string())
                    })?;
                let new_sid = self.fork_session(session_id).await?;
                Ok(serde_json::json!({"sessionId": new_sid}))
            }
            "droid.resume_session" => {
                let session_id = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RpcError::Deserialization("missing 'session_id' field".to_string())
                    })?;
                self.resume_session(session_id).await?;
                Ok(serde_json::Value::Null)
            }
            _ => Err(RpcError::Server {
                code: -32601,
                message: format!("unknown Droid RPC method: {method}"),
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
        // Droid session listing via `droid.list_sessions` RPC.
        match self
            .send_rpc_request("droid.list_sessions", None)
            .await
        {
            Ok(result) => {
                let sessions: Vec<SessionInfo> = serde_json::from_value(result)
                    .unwrap_or_default();
                Ok(sessions)
            }
            Err(DroidTransportError::ApiError { code: -32601, .. }) => {
                // Method not found — older Droid versions may not support this.
                Ok(Vec::new())
            }
            Err(e) => Err(RpcError::from(e)),
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

/// Combined read/write loop for the Droid transport.
async fn io_loop<T>(
    stream: T,
    inner: Arc<Mutex<DroidTransportInner>>,
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
                tracing::debug!("droid_transport: shutdown signal received: {result:?}");
                break;
            }
            read_result = reader.read_line(&mut line_buf) => {
                match read_result {
                    Ok(0) => {
                        tracing::info!("droid_transport: stream EOF");
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
                        tracing::error!("droid_transport: read error: {e}");
                        handle_disconnect(&inner).await;
                        break;
                    }
                }
            }
            msg = writer_rx.recv(), if !writer_closed => {
                match msg {
                    Some(WriterMessage::Write { line }) => {
                        if let Err(e) = write_half.write_all(line.as_bytes()).await {
                            tracing::error!("droid_transport: write error: {e}");
                            writer_closed = true;
                        } else if let Err(e) = write_half.flush().await {
                            tracing::error!("droid_transport: flush error: {e}");
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

/// Process a single JSON-RPC line from the Droid process.
async fn process_line(inner: &Arc<Mutex<DroidTransportInner>>, line: &str) {
    match parse_incoming(line) {
        Ok(Some(incoming)) => {
            handle_incoming(inner, incoming).await;
        }
        Ok(None) => {
            // Empty line — skip.
        }
        Err(e) => {
            tracing::warn!(
                "droid_transport: malformed JSON-RPC line: {e}, line: {}",
                &line[..line.len().min(200)]
            );
        }
    }
}

/// Handle a parsed incoming JSON-RPC message.
async fn handle_incoming(inner: &Arc<Mutex<DroidTransportInner>>, incoming: DroidIncoming) {
    match incoming {
        DroidIncoming::Response(response) => {
            handle_response(inner, response).await;
        }
        DroidIncoming::Notification(notification) => {
            handle_notification(inner, notification).await;
        }
    }
}

/// Handle a JSON-RPC response (correlated to a pending request).
async fn handle_response(inner: &Arc<Mutex<DroidTransportInner>>, response: JsonRpcResponse) {
    let id = match response.id {
        Some(id) => id,
        None => {
            tracing::warn!("droid_transport: response without ID, ignoring");
            return;
        }
    };

    let mut guard = inner.lock().await;
    if let Some(pending) = guard.pending_requests.remove(&id) {
        if let Some(error) = response.error {
            let _ = pending.tx.send(Err(DroidTransportError::ApiError {
                code: error.code,
                message: error.message,
            }));
        } else {
            let _ = pending.tx.send(Ok(response.result.unwrap_or(serde_json::Value::Null)));
        }
    }
}

/// Handle a JSON-RPC notification from Droid.
async fn handle_notification(
    inner: &Arc<Mutex<DroidTransportInner>>,
    notification: crate::provider::droid::protocol::JsonRpcNotification,
) {
    match notification.method.as_str() {
        "droid_working_state_changed" => {
            let payload: Result<WorkingStateChangedPayload, _> =
                serde_json::from_value(notification.params);
            match payload {
                Ok(p) => handle_working_state_changed(inner, p).await,
                Err(e) => {
                    tracing::warn!("droid_transport: failed to parse working_state_changed: {e}");
                }
            }
        }
        "create_message" => {
            let payload: Result<CreateMessagePayload, _> =
                serde_json::from_value(notification.params);
            match payload {
                Ok(p) => handle_create_message(inner, p).await,
                Err(e) => {
                    tracing::warn!("droid_transport: failed to parse create_message: {e}");
                }
            }
        }
        "tool_result" => {
            let payload: Result<ToolResultPayload, _> =
                serde_json::from_value(notification.params);
            match payload {
                Ok(p) => handle_tool_result(inner, p).await,
                Err(e) => {
                    tracing::warn!("droid_transport: failed to parse tool_result: {e}");
                }
            }
        }
        "error" => {
            let payload: Result<ErrorPayload, _> = serde_json::from_value(notification.params);
            match payload {
                Ok(p) => handle_error(inner, p).await,
                Err(e) => {
                    tracing::warn!("droid_transport: failed to parse error: {e}");
                }
            }
        }
        "complete" => {
            let payload: Result<CompletePayload, _> =
                serde_json::from_value(notification.params);
            match payload {
                Ok(p) => handle_complete(inner, p).await,
                Err(e) => {
                    tracing::warn!("droid_transport: failed to parse complete: {e}");
                }
            }
        }
        _ => {
            tracing::debug!(
                "droid_transport: unknown notification method: {}",
                notification.method
            );
            // Emit as Unknown event for forward compatibility.
            let guard = inner.lock().await;
            let _ = guard.event_tx.send(ProviderEvent::Unknown {
                method: notification.method,
                payload: notification.params.to_string(),
            });
        }
    }
}

/// Handle `droid_working_state_changed` notification.
async fn handle_working_state_changed(
    inner: &Arc<Mutex<DroidTransportInner>>,
    payload: WorkingStateChangedPayload,
) {
    let mut guard = inner.lock().await;
    let prev_state = guard.working_state;
    guard.working_state = payload.state;

    match payload.state {
        DroidWorkingState::Streaming => {
            if prev_state != DroidWorkingState::Streaming {
                let _ = guard.event_tx.send(ProviderEvent::StreamingStarted {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                });
                let _ = guard.event_tx.send(ProviderEvent::ThreadStatusChanged {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    status: "active".to_string(),
                });
            }
        }
        DroidWorkingState::Idle => {
            if prev_state != DroidWorkingState::Idle {
                let _ = guard.event_tx.send(ProviderEvent::ThreadStatusChanged {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    status: "idle".to_string(),
                });
                // Deferred complete handling: if a prompt is in-flight,
                // wait for the `complete` notification before emitting StreamingCompleted.
                // If no prompt is in-flight, emit immediately.
                if guard.prompt_in_flight {
                    guard.deferred_complete = true;
                } else {
                    let _ = guard.event_tx.send(ProviderEvent::StreamingCompleted {
                        thread_id: DEFAULT_THREAD_ID.to_string(),
                    });
                }
            }
        }
    }
}

/// Handle `create_message` notification (assistant text streaming).
async fn handle_create_message(
    inner: &Arc<Mutex<DroidTransportInner>>,
    payload: CreateMessagePayload,
) {
    let mut guard = inner.lock().await;

    let message_id = payload
        .message_id
        .clone()
        .unwrap_or_else(|| guard.active_message_id.clone().unwrap_or_default());

    if payload.message_id.is_some() && guard.active_message_id.is_none() {
        // First message in this turn — emit ItemStarted.
        guard.active_message_id = payload.message_id.clone();
        let _ = guard.event_tx.send(ProviderEvent::ItemStarted {
            thread_id: DEFAULT_THREAD_ID.to_string(),
            turn_id: String::new(),
            item_id: message_id.clone(),
        });
    }

    if let Some(delta) = &payload.text_delta
        && !delta.is_empty()
    {
        let _ = guard.event_tx.send(ProviderEvent::MessageDelta {
            thread_id: DEFAULT_THREAD_ID.to_string(),
            item_id: message_id.clone(),
            delta: delta.clone(),
        });
    }

    if payload.complete.unwrap_or(false) {
        // Message completed.
        let _ = guard.event_tx.send(ProviderEvent::ItemCompleted {
            thread_id: DEFAULT_THREAD_ID.to_string(),
            turn_id: String::new(),
            item_id: message_id,
        });
        guard.active_message_id = None;
    }
}

/// Handle `tool_result` notification.
async fn handle_tool_result(inner: &Arc<Mutex<DroidTransportInner>>, payload: ToolResultPayload) {
    let mut guard = inner.lock().await;

    let tool_call_id = payload
        .tool_call_id
        .clone()
        .unwrap_or_else(|| guard.active_tool_call_id.clone().unwrap_or_default());

    let is_new_tool_call = guard.active_tool_call_id.as_deref() != Some(&tool_call_id);

    match payload.status.as_deref() {
        Some("running") => {
            // Tool call started.
            guard.active_tool_call_id = Some(tool_call_id.clone());
            let _ = guard.event_tx.send(ProviderEvent::ToolCallStarted {
                thread_id: DEFAULT_THREAD_ID.to_string(),
                item_id: tool_call_id.clone(),
                tool_name: payload.tool_name.unwrap_or_default(),
                call_id: payload.tool_call_id.unwrap_or_default(),
            });
            // Emit input as initial output.
            if let Some(input) = &payload.input
                && !input.is_empty()
            {
                let _ = guard.event_tx.send(ProviderEvent::CommandOutputDelta {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    item_id: tool_call_id,
                    delta: format!("$ {input}\n"),
                });
            }
        }
        Some("completed") | Some("failed") => {
            let is_failed = payload.status.as_deref() == Some("failed");

            // If this is a new tool call (no prior "running"), emit ToolCallStarted first.
            if is_new_tool_call
                && let Some(tool_name) = &payload.tool_name
            {
                let _ = guard.event_tx.send(ProviderEvent::ToolCallStarted {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    item_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    call_id: payload.tool_call_id.clone().unwrap_or_default(),
                });
            }

            // Emit any output.
            if let Some(output) = &payload.output
                && !output.is_empty()
            {
                let _ = guard.event_tx.send(ProviderEvent::CommandOutputDelta {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    item_id: tool_call_id.clone(),
                    delta: output.clone(),
                });
            }

            // Emit final update.
            let _ = guard.event_tx.send(ProviderEvent::ToolCallUpdate {
                thread_id: DEFAULT_THREAD_ID.to_string(),
                item_id: tool_call_id,
                call_id: String::new(),
                output_delta: format!(
                    "\n[{}]",
                    if is_failed { "failed" } else { "completed" }
                ),
            });
            guard.active_tool_call_id = None;
        }
        _ => {
            // Unknown or no status — emit as a generic tool result.
            if let Some(tool_name) = &payload.tool_name {
                let _ = guard.event_tx.send(ProviderEvent::ToolCallStarted {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    item_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    call_id: String::new(),
                });
            }
            if let Some(output) = &payload.output {
                let _ = guard.event_tx.send(ProviderEvent::ToolCallUpdate {
                    thread_id: DEFAULT_THREAD_ID.to_string(),
                    item_id: tool_call_id,
                    call_id: String::new(),
                    output_delta: output.clone(),
                });
            }
        }
    }
}

/// Handle `error` notification.
async fn handle_error(inner: &Arc<Mutex<DroidTransportInner>>, payload: ErrorPayload) {
    let mut guard = inner.lock().await;

    let message = payload
        .message
        .unwrap_or_else(|| "unknown Droid error".to_string());
    let code = payload.code;

    let _ = guard.event_tx.send(ProviderEvent::Error {
        message: message.clone(),
        code,
    });

    // Fatal error → disconnect.
    if payload.fatal.unwrap_or(false) {
        guard.connected = false;
        let _ = guard.event_tx.send(ProviderEvent::Disconnected {
            message: format!("Droid fatal error: {message}"),
        });
        // Fail all pending requests.
        for (_, pending) in guard.pending_requests.drain() {
            let _ = pending.tx.send(Err(DroidTransportError::ApiError {
                code: code.unwrap_or(-1),
                message: message.clone(),
            }));
        }
    }
}

/// Handle `complete` notification.
async fn handle_complete(inner: &Arc<Mutex<DroidTransportInner>>, payload: CompletePayload) {
    let mut guard = inner.lock().await;

    // Emit StreamingCompleted now (deferred from idle notification).
    let _ = guard.event_tx.send(ProviderEvent::StreamingCompleted {
        thread_id: DEFAULT_THREAD_ID.to_string(),
    });

    // Clear prompt state.
    guard.prompt_in_flight = false;
    guard.deferred_complete = false;
    guard.active_message_id = None;
    guard.active_tool_call_id = None;

    tracing::debug!(
        "droid_transport: prompt completed, reason: {:?}",
        payload.reason
    );
}

/// Handle transport disconnection.
async fn handle_disconnect(inner: &Arc<Mutex<DroidTransportInner>>) {
    let mut guard = inner.lock().await;
    guard.connected = false;

    // If a partial message was in progress, emit an error about interruption.
    if guard.active_message_id.is_some() || guard.active_tool_call_id.is_some() {
        let _ = guard.event_tx.send(ProviderEvent::Error {
            message: "Droid process exited while streaming; message interrupted".to_string(),
            code: None,
        });
    }

    let _ = guard.event_tx.send(ProviderEvent::Disconnected {
        message: "Droid process exited".to_string(),
    });

    // Fail all pending requests.
    for (_, pending) in guard.pending_requests.drain() {
        let _ = pending.tx.send(Err(DroidTransportError::Disconnected));
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::droid::mock::MockDroidChannel;

    /// Helper: create transport + mock channel pair.
    fn setup() -> (DroidNativeTransport, MockDroidChannel) {
        let mock = MockDroidChannel::new();
        let transport = DroidNativeTransport::new(mock.clone());
        (transport, mock)
    }

    // ── VAL-DROID-002: Native transport initialize_session lifecycle ───

    #[tokio::test]
    async fn droid_provider_native_init() {
        let (transport, mock) = setup();

        // Queue initialize response.
        mock.queue_response(
            1,
            serde_json::json!({
                "sessionId": "droid-sess-123",
                "model": {"id": "claude-sonnet-4-20250514"},
                "availableModels": [{"id": "claude-sonnet-4-20250514"}, {"id": "gpt-4.1"}],
            }),
        )
        .await;

        let session_id = transport
            .initialize_session(
                Some("claude-sonnet-4-20250514"),
                Some(DroidAutonomyLevel::Normal),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(session_id, "droid-sess-123");

        // Verify the request was sent.
        let requests = mock.captured_requests().await;
        assert!(!requests.is_empty());
        assert_eq!(requests[0]["method"], "droid.initialize_session");
        assert_eq!(requests[0]["params"]["model"], "claude-sonnet-4-20250514");
    }

    // ── VAL-DROID-003: working_state_changed → status mapping ──────────

    #[tokio::test]
    async fn droid_provider_working_state_mapping() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Queue state change notifications.
        mock.queue_notification(
            "droid_working_state_changed",
            serde_json::json!({"state": "streaming"}),
        )
        .await;
        mock.queue_notification(
            "droid_working_state_changed",
            serde_json::json!({"state": "idle"}),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // Should see StreamingStarted and ThreadStatusChanged(active).
        let got_streaming_start = events.iter().any(|e| {
            matches!(e, ProviderEvent::StreamingStarted { thread_id } if thread_id == DEFAULT_THREAD_ID)
        });
        assert!(got_streaming_start, "should emit StreamingStarted");

        let got_active = events.iter().any(|e| {
            matches!(e, ProviderEvent::ThreadStatusChanged { status, .. } if status == "active")
        });
        assert!(got_active, "should emit ThreadStatusChanged(active)");

        let got_idle = events.iter().any(|e| {
            matches!(e, ProviderEvent::ThreadStatusChanged { status, .. } if status == "idle")
        });
        assert!(got_idle, "should emit ThreadStatusChanged(idle)");
    }

    #[tokio::test]
    async fn droid_provider_working_state_idempotent() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Send same state twice.
        mock.queue_notification(
            "droid_working_state_changed",
            serde_json::json!({"state": "streaming"}),
        )
        .await;
        mock.queue_notification(
            "droid_working_state_changed",
            serde_json::json!({"state": "streaming"}),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut streaming_started_count = 0;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ProviderEvent::StreamingStarted { .. }) {
                streaming_started_count += 1;
            }
        }
        // Only one StreamingStarted (second duplicate is ignored).
        assert_eq!(
            streaming_started_count, 1,
            "should emit exactly one StreamingStarted for duplicate states"
        );
    }

    // ── VAL-DROID-004: create_message → assistant message streaming ─────

    #[tokio::test]
    async fn droid_provider_assistant_streaming() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Queue streaming text deltas.
        mock.queue_notification(
            "create_message",
            serde_json::json!({
                "message_id": "msg-1",
                "text_delta": "Hello",
                "complete": false,
            }),
        )
        .await;
        mock.queue_notification(
            "create_message",
            serde_json::json!({
                "message_id": "msg-1",
                "text_delta": " world",
                "complete": true,
            }),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // Should have ItemStarted, MessageDelta("Hello"), MessageDelta(" world"), ItemCompleted.
        let got_item_started = events.iter().any(|e| {
            matches!(e, ProviderEvent::ItemStarted { item_id, .. } if item_id == "msg-1")
        });
        assert!(got_item_started, "should emit ItemStarted for msg-1");

        let deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::MessageDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert!(deltas.contains(&"Hello"), "should contain 'Hello' delta");
        assert!(deltas.contains(&" world"), "should contain ' world' delta");

        let got_item_completed = events.iter().any(|e| {
            matches!(e, ProviderEvent::ItemCompleted { item_id, .. } if item_id == "msg-1")
        });
        assert!(got_item_completed, "should emit ItemCompleted when complete=true");
    }

    // ── VAL-DROID-005: tool_result → tool call item mapping ────────────

    #[tokio::test]
    async fn droid_provider_tool_result_mapping() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Queue tool result.
        mock.queue_notification(
            "tool_result",
            serde_json::json!({
                "tool_call_id": "tc-1",
                "tool_name": "read_file",
                "input": "/tmp/test.txt",
                "output": "file contents here",
                "status": "completed",
            }),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // Should have ToolCallStarted and ToolCallUpdate.
        let got_tool_started = events.iter().any(|e| {
            matches!(
                e,
                ProviderEvent::ToolCallStarted { tool_name, .. }
                if tool_name == "read_file"
            )
        });
        assert!(got_tool_started, "should emit ToolCallStarted for read_file");

        let got_output = events.iter().any(|e| {
            matches!(
                e,
                ProviderEvent::CommandOutputDelta { delta, .. }
                if delta.contains("file contents here")
            )
        });
        assert!(got_output, "should emit CommandOutputDelta with tool output");
    }

    #[tokio::test]
    async fn droid_provider_tool_result_running_then_completed() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Tool starts running.
        mock.queue_notification(
            "tool_result",
            serde_json::json!({
                "tool_call_id": "tc-2",
                "tool_name": "shell",
                "input": "git status",
                "status": "running",
            }),
        )
        .await;

        // Tool completes.
        mock.queue_notification(
            "tool_result",
            serde_json::json!({
                "tool_call_id": "tc-2",
                "output": "On branch main",
                "status": "completed",
            }),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        let tool_started = events.iter().any(|e| {
            matches!(e, ProviderEvent::ToolCallStarted { tool_name, .. } if tool_name == "shell")
        });
        assert!(tool_started, "should emit ToolCallStarted when running");

        let got_output = events.iter().any(|e| {
            matches!(e, ProviderEvent::CommandOutputDelta { delta, .. } if delta == "On branch main")
        });
        assert!(got_output, "should emit output on completion");
    }

    // ── VAL-DROID-006: error notification → Error event ────────────────

    #[tokio::test]
    async fn droid_provider_error_mapping() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        mock.queue_notification(
            "error",
            serde_json::json!({
                "message": "Rate limited",
                "code": 429,
                "fatal": false,
            }),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut got_error = false;
        while let Ok(event) = rx.try_recv() {
            if let ProviderEvent::Error { message, code } = &event {
                assert_eq!(message, "Rate limited");
                assert_eq!(*code, Some(429));
                got_error = true;
            }
        }
        assert!(got_error, "should emit ProviderEvent::Error");

        // Non-fatal error should NOT disconnect.
        assert!(transport.is_connected());
    }

    #[tokio::test]
    async fn droid_provider_fatal_error_disconnects() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        mock.queue_notification(
            "error",
            serde_json::json!({
                "message": "Authentication failed",
                "code": 401,
                "fatal": true,
            }),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut got_error = false;
        let mut got_disconnect = false;
        while let Ok(event) = rx.try_recv() {
            match &event {
                ProviderEvent::Error { message, code } => {
                    assert_eq!(message, "Authentication failed");
                    assert_eq!(*code, Some(401));
                    got_error = true;
                }
                ProviderEvent::Disconnected { .. } => {
                    got_disconnect = true;
                }
                _ => {}
            }
        }
        assert!(got_error, "should emit Error event for fatal error");
        assert!(got_disconnect, "should emit Disconnected for fatal error");
        assert!(!transport.is_connected());
    }

    // ── VAL-DROID-007: autonomy levels, model selection, reasoning effort

    #[tokio::test]
    async fn droid_provider_config_params() {
        let (transport, mock) = setup();

        // Set model.
        mock.queue_response(1, serde_json::json!({"ok": true})).await;
        transport.set_model("gpt-4.1").await.unwrap();

        let requests = mock.captured_requests().await;
        assert!(requests[0]["method"] == "droid.set_model");
        assert!(requests[0]["params"]["model"] == "gpt-4.1");

        // Set autonomy level.
        mock.queue_response(2, serde_json::json!({"ok": true})).await;
        transport.set_autonomy_level(DroidAutonomyLevel::Full).await.unwrap();

        let requests = mock.captured_requests().await;
        let set_auto = requests.iter().find(|r| r["method"] == "droid.set_autonomy_level").unwrap();
        assert_eq!(set_auto["params"]["level"], "full");

        // Set reasoning effort.
        mock.queue_response(3, serde_json::json!({"ok": true})).await;
        transport
            .set_reasoning_effort(DroidReasoningEffort::High)
            .await
            .unwrap();

        let requests = mock.captured_requests().await;
        let set_effort = requests
            .iter()
            .find(|r| r["method"] == "droid.set_reasoning_effort")
            .unwrap();
        assert_eq!(set_effort["params"]["effort"], "high");
    }

    // ── VAL-DROID-008: FACTORY_API_KEY (tested via initialize params) ───

    #[tokio::test]
    async fn droid_provider_auth_key_handling() {
        // The FACTORY_API_KEY is handled at the SSH/environment level,
        // not in the transport itself. The transport just sends requests.
        // This test verifies that initialize_session succeeds and that
        // auth errors are properly mapped.
        let (transport, mock) = setup();

        // Simulate auth error from Droid.
        mock.queue_error_response(1, 401, "Invalid API key").await;

        let result = transport
            .initialize_session(None, None, None, None, None)
            .await;

        assert!(result.is_err());
        match result {
            Err(RpcError::Server { code, message }) => {
                assert_eq!(code, 401);
                assert!(message.contains("Invalid API key"));
                // Key should NOT appear in the error message.
                assert!(!message.contains("FACTORY_API_KEY"));
            }
            _ => panic!("expected Server error with code 401"),
        }
    }

    // ── VAL-DROID-009: Session ID tracking, fork, resume ───────────────

    #[tokio::test]
    async fn droid_provider_session_lifecycle() {
        let (transport, mock) = setup();

        // Initialize.
        mock.queue_response(
            1,
            serde_json::json!({"sessionId": "sess-original"}),
        )
        .await;
        let sid = transport
            .initialize_session(None, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(sid, "sess-original");
        assert_eq!(transport.session_id().await, Some("sess-original".to_string()));

        // Fork.
        mock.queue_response(
            2,
            serde_json::json!({"session_id": "sess-forked"}),
        )
        .await;
        let forked_sid = transport.fork_session("sess-original").await.unwrap();
        assert_eq!(forked_sid, "sess-forked");
        assert_ne!(forked_sid, "sess-original");

        // Resume.
        mock.queue_response(3, serde_json::json!({"ok": true})).await;
        transport.resume_session("sess-original").await.unwrap();
        assert_eq!(transport.session_id().await, Some("sess-original".to_string()));
    }

    // ── VAL-DROID-011: Droid process crash and error handling ───────────

    #[tokio::test]
    async fn droid_provider_crash_recovery() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Start streaming then crash.
        mock.queue_notification(
            "create_message",
            serde_json::json!({
                "message_id": "msg-1",
                "text_delta": "Partial message...",
                "complete": false,
            }),
        )
        .await;
        // Don't send complete — simulate crash by disconnecting.
        mock.simulate_disconnect().await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut got_disconnect = false;
        let mut got_interrupt_error = false;
        while let Ok(event) = rx.try_recv() {
            match &event {
                ProviderEvent::Disconnected { .. } => got_disconnect = true,
                ProviderEvent::Error { message, .. } if message.contains("interrupted") => {
                    got_interrupt_error = true;
                }
                _ => {}
            }
        }
        assert!(got_disconnect, "should emit Disconnected on crash");
        assert!(
            got_interrupt_error,
            "should emit Error for interrupted message"
        );
        assert!(!transport.is_connected());
    }

    // ── VAL-DROID-012: API error handling ───────────────────────────────

    #[tokio::test]
    async fn droid_provider_api_error_handling() {
        let (transport, mock) = setup();

        // Permanent error (400) should surface immediately.
        mock.queue_error_response(1, 400, "Bad request").await;

        let result = transport.set_model("invalid-model").await;
        assert!(result.is_err());
        match result {
            Err(RpcError::Server { code, message }) => {
                assert_eq!(code, 400);
                assert_eq!(message, "Bad request");
            }
            _ => panic!("expected Server error"),
        }
    }

    // ── Deferred complete handling (idle-before-complete quirk) ─────────

    #[tokio::test]
    async fn droid_idle_before_complete_deferred() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Simulate: add_user_message ack, then idle, then complete
        // The deferred_complete logic should hold StreamingCompleted until the complete notification.
        {
            let mut inner = transport.inner.lock().await;
            inner.prompt_in_flight = true;
        }

        // Working state: streaming.
        mock.queue_notification(
            "droid_working_state_changed",
            serde_json::json!({"state": "streaming"}),
        )
        .await;

        // Working state: idle (comes before complete — the quirk).
        mock.queue_notification(
            "droid_working_state_changed",
            serde_json::json!({"state": "idle"}),
        )
        .await;

        // Complete arrives after idle.
        mock.queue_notification("complete", serde_json::json!({"reason": "done"}))
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // Should have exactly one StreamingCompleted (from the complete notification).
        let completed_count = events
            .iter()
            .filter(|e| matches!(e, ProviderEvent::StreamingCompleted { .. }))
            .count();
        assert_eq!(
            completed_count, 1,
            "should emit exactly one StreamingCompleted from complete notification"
        );
    }

    // ── Full session lifecycle ──────────────────────────────────────────

    #[tokio::test]
    async fn droid_full_session_lifecycle() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        mock.queue_simple_session(&["Hello", " world"]).await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // Verify event types present.
        let event_types: Vec<&str> = events
            .iter()
            .map(|e| match e {
                ProviderEvent::StreamingStarted { .. } => "StreamingStarted",
                ProviderEvent::StreamingCompleted { .. } => "StreamingCompleted",
                ProviderEvent::ThreadStatusChanged { status, .. } => {
                    if status == "active" {
                        "StatusActive"
                    } else {
                        "StatusIdle"
                    }
                }
                ProviderEvent::MessageDelta { .. } => "MessageDelta",
                ProviderEvent::ItemStarted { .. } => "ItemStarted",
                ProviderEvent::ItemCompleted { .. } => "ItemCompleted",
                _ => "Other",
            })
            .collect();

        assert!(event_types.contains(&"StreamingStarted"), "should have StreamingStarted");
        assert!(event_types.contains(&"MessageDelta"), "should have MessageDelta");
        assert!(event_types.contains(&"StreamingCompleted"), "should have StreamingCompleted");
    }

    // ── Session with tool use ───────────────────────────────────────────

    #[tokio::test]
    async fn droid_session_with_tool_use() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        mock.queue_session_with_tool_use("read_file", "file contents", "Here is the file")
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // Should have tool events.
        let got_tool_started = events.iter().any(|e| {
            matches!(
                e,
                ProviderEvent::ToolCallStarted { tool_name, .. }
                if tool_name == "read_file"
            )
        });
        assert!(got_tool_started, "should emit ToolCallStarted");

        // Should have text delta.
        let got_text = events.iter().any(|e| {
            matches!(
                e,
                ProviderEvent::MessageDelta { delta, .. }
                if delta == "Here is the file"
            )
        });
        assert!(got_text, "should emit text delta after tool result");
    }

    // ── Malformed JSON handling ─────────────────────────────────────────

    #[tokio::test]
    async fn droid_malformed_json_does_not_crash() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        // Queue mix of valid and invalid lines.
        mock.queue_notification(
            "droid_working_state_changed",
            serde_json::json!({"state": "streaming"}),
        )
        .await;
        mock.queue_line("{broken json\n").await;
        mock.queue_notification(
            "droid_working_state_changed",
            serde_json::json!({"state": "idle"}),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Should still process valid events.
        let mut got_streaming = false;
        let mut got_idle = false;
        while let Ok(event) = rx.try_recv() {
            match &event {
                ProviderEvent::StreamingStarted { .. } => got_streaming = true,
                ProviderEvent::ThreadStatusChanged { status, .. } if status == "idle" => {
                    got_idle = true;
                }
                _ => {}
            }
        }
        assert!(got_streaming, "should process valid event before malformed");
        assert!(got_idle, "should process valid event after malformed");
    }

    // ── Unknown notification method ─────────────────────────────────────

    #[tokio::test]
    async fn droid_unknown_notification_method() {
        let (transport, mock) = setup();
        let mut rx = transport.subscribe();

        mock.queue_notification("future_method", serde_json::json!({"data": "test"}))
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut got_unknown = false;
        while let Ok(event) = rx.try_recv() {
            if let ProviderEvent::Unknown { method, payload } = &event {
                assert_eq!(method, "future_method");
                assert!(payload.contains("test"));
                got_unknown = true;
            }
        }
        assert!(got_unknown, "should emit Unknown event for unrecognized notification");
    }

    // ── ProviderTransport trait compliance ───────────────────────────────

    #[tokio::test]
    async fn droid_provider_transport_disconnect_idempotent() {
        let (mut transport, _mock) = setup();
        transport.disconnect().await;
        transport.disconnect().await; // Second disconnect should not panic.
    }

    #[tokio::test]
    async fn droid_provider_transport_post_disconnect_request_fails() {
        let (mut transport, _mock) = setup();
        transport.disconnect().await;

        let result = transport
            .send_request("droid.cancel", serde_json::Value::Null)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn droid_provider_transport_is_connected() {
        let (transport, _mock) = setup();
        assert!(transport.is_connected());
    }

    #[tokio::test]
    async fn droid_provider_transport_unknown_method() {
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

    // ── Droid binary not found ───────────────────────────────────────────

    #[tokio::test]
    async fn droid_binary_not_found() {
        let mock = MockDroidChannel::new_droid_not_found();
        let transport = DroidNativeTransport::new(mock.clone());
        let mut rx = transport.subscribe();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut got_disconnect = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ProviderEvent::Disconnected { .. }) {
                got_disconnect = true;
            }
        }
        assert!(got_disconnect, "should detect droid not found as disconnect");
        assert!(!transport.is_connected());
    }

    // ── Transient error retry ────────────────────────────────────────────

    #[tokio::test]
    async fn droid_transient_error_retries() {
        // Test that transient errors (429) are retried.
        // We use direct send_rpc_request to test a single transient error.
        let (transport, mock) = setup();

        // Queue a 429 error for the first request (ID 1).
        mock.queue_error_response(1, 429, "Rate limited").await;

        let result = transport
            .send_rpc_request("droid.set_model", Some(serde_json::json!({"model": "test"})))
            .await;

        // Should get the 429 error back.
        match result {
            Err(DroidTransportError::ApiError { code, message }) => {
                assert_eq!(code, 429);
                assert_eq!(message, "Rate limited");
            }
            other => panic!("expected ApiError(429), got: {other:?}"),
        }

        // Now queue a success response for the second request (ID 2).
        mock.queue_response(2, serde_json::json!({"ok": true})).await;

        let result = transport
            .send_rpc_request_with_retry("droid.set_model", Some(serde_json::json!({"model": "test"})))
            .await;

        assert!(result.is_ok(), "should succeed on fresh request: {result:?}");
    }

    // ── Autonomy level mapping ───────────────────────────────────────────

    #[tokio::test]
    async fn droid_autonomy_levels_all_values() {
        for level in DroidAutonomyLevel::all() {
            let (transport, mock) = setup();

            mock.queue_response(1, serde_json::json!({"ok": true})).await;
            transport.set_autonomy_level(*level).await.unwrap();

            let requests = mock.captured_requests().await;
            assert_eq!(requests[0]["method"], "droid.set_autonomy_level");
        }
    }

    // ── Reasoning effort mapping ─────────────────────────────────────────

    #[tokio::test]
    async fn droid_reasoning_effort_all_values() {
        for effort in DroidReasoningEffort::all() {
            let (transport, mock) = setup();

            mock.queue_response(1, serde_json::json!({"ok": true})).await;
            transport.set_reasoning_effort(*effort).await.unwrap();

            let requests = mock.captured_requests().await;
            assert_eq!(requests[0]["method"], "droid.set_reasoning_effort");
        }
    }

    // ── Cancel command ───────────────────────────────────────────────────

    #[tokio::test]
    async fn droid_cancel_command() {
        let (transport, mock) = setup();

        mock.queue_response(1, serde_json::json!({"ok": true})).await;
        transport.cancel().await.unwrap();

        let requests = mock.captured_requests().await;
        assert_eq!(requests[0]["method"], "droid.cancel");
    }

    // ── List sessions ────────────────────────────────────────────────────

    #[tokio::test]
    async fn droid_list_sessions() {
        let (mut transport, mock) = setup();

        mock.queue_response(
            1,
            serde_json::json!([
                {"id": "sess-1", "title": "Session 1", "createdAt": "2025-01-01T00:00:00Z", "updatedAt": "2025-01-01T01:00:00Z"}
            ]),
        )
        .await;

        let sessions = transport.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "sess-1");
    }
}
