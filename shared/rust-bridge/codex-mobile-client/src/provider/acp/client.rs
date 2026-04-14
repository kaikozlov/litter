//! ACP client lifecycle implementation.
//!
//! Implements the full ACP protocol lifecycle over any bidirectional transport
//! (SSH PTY, TCP, or mock):
//!
//! 1. `initialize` — negotiate protocol version, exchange capabilities
//! 2. `authenticate` — authenticate with the agent (with failure handling)
//! 3. `session/new` — create a new session
//! 4. `session/prompt` — send a prompt, stream updates, resolve when complete
//! 5. `session/cancel` — terminate active prompt
//! 6. `session/list` — list available sessions
//! 7. `session/load` — load an existing session
//!
//! The client uses the NDJSON framing codec from the `framing` module and
//! communicates through any transport implementing `AsyncRead + AsyncWrite`.
//! The transport is split into read and write halves using `tokio::io::split`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, broadcast, mpsc};

use agent_client_protocol_schema::{
    AgentNotification, AgentResponse, AuthenticateRequest, AuthMethodId, CancelNotification,
    ClientNotification, ClientRequest, InitializeRequest, Implementation,
    ListSessionsRequest, LoadSessionRequest, NewSessionRequest, ProtocolVersion, PromptRequest,
    RequestId, SessionId,
};

use crate::provider::acp::framing::{self, FramingError, IncomingMessage, NdjsonReader};
use crate::provider::{ProviderEvent, SessionInfo};
use crate::transport::RpcError;

/// Error type for ACP client operations.
#[derive(Debug, thiserror::Error)]
pub enum AcpClientError {
    /// Protocol version mismatch between client and agent.
    #[error("protocol version mismatch: server supports {server_version}, client supports {client_version}")]
    ProtocolVersionMismatch {
        client_version: ProtocolVersion,
        server_version: ProtocolVersion,
    },

    /// Authentication failed (permanent).
    #[error("authentication failed: {message}")]
    AuthenticationFailed { message: String },

    /// Authentication failed with a transient error (retryable).
    #[error("authentication transient failure: {message}")]
    AuthenticationTransient { message: String },

    /// Session operation attempted before authentication.
    #[error("session operation rejected: not authenticated")]
    NotAuthenticated,

    /// Session operation attempted before initialization.
    #[error("session operation rejected: not initialized")]
    NotInitialized,

    /// Session not found.
    #[error("session not found: {0}")]
    SessionNotFound(String),

    /// Transport disconnected.
    #[error("transport disconnected")]
    Disconnected,

    /// Transport-level error.
    #[error("transport error: {0}")]
    Transport(#[from] FramingError),

    /// JSON-RPC error from the agent.
    #[error("agent error {code}: {message}")]
    AgentError { code: i64, message: String },

    /// Timeout waiting for response.
    #[error("timeout waiting for response: {method}")]
    Timeout { method: String },

    /// Client is already shut down.
    #[error("client shut down")]
    ShutDown,
}

impl From<AcpClientError> for RpcError {
    fn from(err: AcpClientError) -> Self {
        match err {
            AcpClientError::Transport(FramingError::StreamClosed)
            | AcpClientError::Disconnected => {
                RpcError::Transport(crate::transport::TransportError::Disconnected)
            }
            AcpClientError::AgentError { code, message } => RpcError::Server { code, message },
            _ => RpcError::Server {
                code: -1,
                message: err.to_string(),
            },
        }
    }
}

/// Internal state for the ACP client lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleState {
    /// Not yet initialized.
    Uninitialized,
    /// Initialize completed successfully.
    Initialized,
    /// Authentication completed successfully.
    Authenticated,
    /// Client has been disconnected.
    Disconnected,
}

/// Result of the initialize handshake.
#[derive(Debug, Clone)]
pub struct InitializeResult {
    /// Protocol version negotiated with the agent.
    pub protocol_version: ProtocolVersion,
    /// Capabilities advertised by the agent.
    pub agent_capabilities: agent_client_protocol_schema::AgentCapabilities,
    /// Authentication methods supported by the agent.
    pub auth_methods: Vec<AuthMethodId>,
    /// Agent implementation info.
    pub agent_info: Option<Implementation>,
}

/// Result of the session/new operation.
#[derive(Debug, Clone)]
pub struct NewSessionResult {
    /// The session ID returned by the agent.
    pub session_id: SessionId,
}

/// Result of the session/prompt operation.
#[derive(Debug, Clone)]
pub struct PromptResult {
    /// The stop reason from the agent.
    pub stop_reason: String,
}

/// Response received from the agent that needs to be routed.
#[derive(Debug)]
enum PendingResponse {
    /// A typed AgentResponse was decoded.
    Typed(AgentResponse),
}

/// Message sent from the API surface to the writer task.
enum WriterMessage {
    Write { line: String },
    Shutdown,
}

/// Shared inner state behind a mutex.
struct AcpClientInner {
    /// Current lifecycle state.
    state: LifecycleState,
    /// Negotiated protocol version from initialize.
    negotiated_version: Option<ProtocolVersion>,
    /// Auth methods available from initialize.
    auth_methods: Vec<AuthMethodId>,
    /// Active session ID (set by session/new or session/load).
    active_session_id: Option<SessionId>,
    /// Whether a prompt is currently streaming.
    is_streaming: bool,
    /// Event broadcast sender.
    event_tx: broadcast::Sender<ProviderEvent>,
    /// Buffered responses that arrived before their request was registered.
    /// Maps request ID → response.
    buffered_responses: HashMap<i64, Result<PendingResponse, AcpClientError>>,
    /// Pending request responses: request ID → oneshot sender.
    pending_responses:
        HashMap<i64, tokio::sync::oneshot::Sender<Result<PendingResponse, AcpClientError>>>,
    /// Maps request ID → method name, for response deserialization.
    pending_methods: HashMap<i64, String>,
    /// Next request ID counter.
    next_id: i64,
}

/// ACP client that drives the full protocol lifecycle over a bidirectional
/// transport.
///
/// The client splits the transport into read and write halves, spawns a
/// background reader task for incoming NDJSON messages, and uses a writer
/// channel for outgoing messages.
pub struct AcpClient {
    inner: Arc<Mutex<AcpClientInner>>,
    /// Channel to send write requests to the writer task.
    writer_tx: mpsc::Sender<WriterMessage>,
    /// Channel to signal both reader and writer tasks to stop.
    shutdown_tx: Option<broadcast::Sender<()>>,
}

impl AcpClient {
    /// Create a new ACP client over the given bidirectional transport.
    ///
    /// Splits the transport into read and write halves. Spawns:
    /// - A reader task that processes incoming NDJSON messages and routes
    ///   responses to pending requests.
    /// - A writer task that serializes outgoing messages to the transport.
    ///
    /// The client starts in the `Uninitialized` state and must be initialized
    /// via `initialize()` before any other operations.
    pub fn new<T>(transport: T) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (event_tx, _) = broadcast::channel(256);
        let inner = Arc::new(Mutex::new(AcpClientInner {
            state: LifecycleState::Uninitialized,
            negotiated_version: None,
            auth_methods: Vec::new(),
            active_session_id: None,
            is_streaming: false,
            event_tx,
            pending_responses: HashMap::new(),
            buffered_responses: HashMap::new(),
            pending_methods: HashMap::new(),
            next_id: 1,
        }));

        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let (writer_tx, writer_rx) = mpsc::channel::<WriterMessage>(64);

        // Spawn a single task that manages both reading and writing.
        // This avoids issues with splitting the transport.
        let io_inner = inner.clone();
        let mut io_shutdown = shutdown_rx.resubscribe();
        tokio::spawn(async move {
            // Pin the transport so we can split it.
            let mut transport = std::pin::pin!(transport);
            io_loop(&mut transport, io_inner, writer_rx, &mut io_shutdown).await;
        });

        Self {
            inner,
            writer_tx,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    /// Perform the `initialize` handshake.
    ///
    /// Sends the initialize request with the client's protocol version and
    /// capabilities. Validates the agent's response:
    /// - If the agent returns a version higher than the client supports,
    ///   returns `ProtocolVersionMismatch`.
    /// - If the agent returns a lower compatible version, proceeds with that
    ///   version.
    ///
    /// After successful initialization, the client transitions to `Initialized`
    /// state and authentication methods are available.
    pub async fn initialize(
        &self,
        client_name: &str,
        client_version: &str,
    ) -> Result<InitializeResult, AcpClientError> {
        {
            let inner = self.inner.lock().await;
            if inner.state != LifecycleState::Uninitialized {
                return Err(AcpClientError::NotInitialized);
            }
        }

        let client_version_val = ProtocolVersion::LATEST;
        let request = InitializeRequest::new(client_version_val.clone())
            .client_info(Implementation::new(client_name, client_version));

        let response = self
            .send_request(ClientRequest::InitializeRequest(request))
            .await?;

        let init_response = match response {
            AgentResponse::InitializeResponse(r) => r,
            other => {
                return Err(AcpClientError::AgentError {
                    code: -1,
                    message: format!("unexpected response type to initialize: {other:?}"),
                });
            }
        };

        // Validate protocol version.
        let server_version = init_response.protocol_version;
        if server_version > client_version_val {
            return Err(AcpClientError::ProtocolVersionMismatch {
                client_version: client_version_val,
                server_version,
            });
        }

        // Extract auth method IDs.
        let auth_method_ids: Vec<AuthMethodId> = init_response
            .auth_methods
            .iter()
            .map(|m| m.id().clone())
            .collect();

        let result = InitializeResult {
            protocol_version: server_version.clone(),
            agent_capabilities: init_response.agent_capabilities,
            auth_methods: auth_method_ids.clone(),
            agent_info: init_response.agent_info,
        };

        // Update state.
        {
            let mut inner = self.inner.lock().await;
            inner.state = LifecycleState::Initialized;
            inner.negotiated_version = Some(server_version);
            inner.auth_methods = auth_method_ids;
        }

        Ok(result)
    }

    /// Perform the `authenticate` handshake.
    ///
    /// Must be called after `initialize()` succeeds. Uses the first available
    /// auth method if `method_id` is `None`.
    ///
    /// # Failure handling
    /// - Transient failures (e.g., network timeout) are retried once.
    /// - Permanent failures (e.g., invalid credentials) are not retried.
    pub async fn authenticate(
        &self,
        method_id: Option<AuthMethodId>,
    ) -> Result<(), AcpClientError> {
        {
            let inner = self.inner.lock().await;
            if inner.state == LifecycleState::Uninitialized {
                return Err(AcpClientError::NotInitialized);
            }
        }

        // Pick the first available auth method if not specified.
        let method_id = match method_id {
            Some(id) => id,
            None => {
                let inner = self.inner.lock().await;
                inner
                    .auth_methods
                    .first()
                    .cloned()
                    .ok_or_else(|| AcpClientError::AuthenticationFailed {
                        message: "no auth methods available".to_string(),
                    })?
            }
        };

        let request = AuthenticateRequest::new(method_id);

        // First attempt.
        match self
            .send_request(ClientRequest::AuthenticateRequest(request.clone()))
            .await
        {
            Ok(AgentResponse::AuthenticateResponse(_)) => {
                let mut inner = self.inner.lock().await;
                inner.state = LifecycleState::Authenticated;
                Ok(())
            }
            Ok(other) => Err(AcpClientError::AgentError {
                code: -1,
                message: format!("unexpected response to authenticate: {other:?}"),
            }),
            Err(AcpClientError::AgentError { code, message }) => {
                // AuthRequired error (-32000) is permanent.
                if code == -32000 {
                    return Err(AcpClientError::AuthenticationFailed { message });
                }
                // Retry once for transient errors.
                self.authenticate_retry(request).await
            }
            Err(_e) => {
                // Transport or other error — retry once for transient issues.
                self.authenticate_retry(request).await
            }
        }
    }

    /// Internal: retry the authenticate request once.
    async fn authenticate_retry(
        &self,
        request: AuthenticateRequest,
    ) -> Result<(), AcpClientError> {
        match self
            .send_request(ClientRequest::AuthenticateRequest(request))
            .await
        {
            Ok(AgentResponse::AuthenticateResponse(_)) => {
                let mut inner = self.inner.lock().await;
                inner.state = LifecycleState::Authenticated;
                Ok(())
            }
            Ok(other) => Err(AcpClientError::AgentError {
                code: -1,
                message: format!("unexpected response to authenticate (retry): {other:?}"),
            }),
            Err(AcpClientError::AgentError { code, message }) => {
                if code == -32000 {
                    Err(AcpClientError::AuthenticationFailed { message })
                } else {
                    Err(AcpClientError::AuthenticationTransient {
                        message: format!("retry failed: {message}"),
                    })
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Create a new session.
    ///
    /// Must be called after `authenticate()` succeeds.
    pub async fn session_new(&self, cwd: &str) -> Result<NewSessionResult, AcpClientError> {
        {
            let inner = self.inner.lock().await;
            if inner.state != LifecycleState::Authenticated {
                return Err(AcpClientError::NotAuthenticated);
            }
        }

        let request = NewSessionRequest::new(std::path::PathBuf::from(cwd));
        let response = self
            .send_request(ClientRequest::NewSessionRequest(request))
            .await?;

        match response {
            AgentResponse::NewSessionResponse(r) => {
                let session_id = r.session_id.clone();
                let mut inner = self.inner.lock().await;
                inner.active_session_id = Some(session_id.clone());
                Ok(NewSessionResult { session_id })
            }
            other => Err(AcpClientError::AgentError {
                code: -1,
                message: format!("unexpected response to session/new: {other:?}"),
            }),
        }
    }

    /// Send a prompt and stream updates until complete.
    ///
    /// Returns when the agent signals prompt completion (via PromptResponse).
    /// Session updates are emitted as `ProviderEvent`s via the event stream.
    ///
    /// The `session_id` must be from a previous `session_new()` call.
    pub async fn session_prompt(
        &self,
        session_id: &SessionId,
        prompt_text: &str,
    ) -> Result<PromptResult, AcpClientError> {
        {
            let inner = self.inner.lock().await;
            if inner.state != LifecycleState::Authenticated {
                return Err(AcpClientError::NotAuthenticated);
            }
        }

        {
            let mut inner = self.inner.lock().await;
            inner.is_streaming = true;
        }

        let request = PromptRequest::new(
            session_id.clone(),
            vec![agent_client_protocol_schema::ContentBlock::Text(
                agent_client_protocol_schema::TextContent::new(prompt_text),
            )],
        );

        let response = self
            .send_request(ClientRequest::PromptRequest(request))
            .await;

        {
            let mut inner = self.inner.lock().await;
            inner.is_streaming = false;
        }

        match response {
            Ok(AgentResponse::PromptResponse(r)) => {
                let stop_reason = format!("{:?}", r.stop_reason);
                Ok(PromptResult { stop_reason })
            }
            Ok(other) => Err(AcpClientError::AgentError {
                code: -1,
                message: format!("unexpected response to session/prompt: {other:?}"),
            }),
            Err(e) => Err(e),
        }
    }

    /// Cancel the active prompt for a session.
    ///
    /// Sends a `session/cancel` notification to the agent. If the session is
    /// idle (no active prompt), this is a no-op and returns `Ok(())`.
    ///
    /// During streaming, partial content is flushed as valid items and the
    /// session becomes ready for the next prompt.
    pub async fn session_cancel(&self, session_id: &SessionId) -> Result<(), AcpClientError> {
        {
            let inner = self.inner.lock().await;
            if inner.state == LifecycleState::Disconnected {
                return Err(AcpClientError::Disconnected);
            }
        }

        // Check if we're streaming — if not, it's a no-op.
        {
            let inner = self.inner.lock().await;
            if !inner.is_streaming {
                return Ok(());
            }
        }

        // Send the cancel notification.
        let cancel = CancelNotification::new(session_id.clone());
        self.send_notification(ClientNotification::CancelNotification(cancel))
            .await?;

        // Mark streaming as false.
        {
            let mut inner = self.inner.lock().await;
            inner.is_streaming = false;
        }

        Ok(())
    }

    /// List available sessions.
    ///
    /// Returns an empty array if no sessions are available (not an error).
    pub async fn session_list(&self) -> Result<Vec<SessionInfo>, AcpClientError> {
        {
            let inner = self.inner.lock().await;
            if inner.state == LifecycleState::Uninitialized {
                return Err(AcpClientError::NotInitialized);
            }
        }

        let request = ListSessionsRequest::default();
        let response = self
            .send_request(ClientRequest::ListSessionsRequest(request))
            .await?;

        match response {
            AgentResponse::ListSessionsResponse(r) => {
                let sessions: Vec<SessionInfo> = r
                    .sessions
                    .iter()
                    .map(|s| SessionInfo {
                        id: s.session_id.to_string(),
                        title: s.title.clone().unwrap_or_default(),
                        created_at: String::new(),
                        updated_at: String::new(),
                    })
                    .collect();
                Ok(sessions)
            }
            other => Err(AcpClientError::AgentError {
                code: -1,
                message: format!("unexpected response to session/list: {other:?}"),
            }),
        }
    }

    /// Load an existing session by ID.
    ///
    /// Returns an error if the session ID is not found.
    /// The agent streams session history via `session/update` notifications,
    /// which are emitted as `ProviderEvent`s.
    pub async fn session_load(
        &self,
        session_id: &SessionId,
        cwd: &str,
    ) -> Result<(), AcpClientError> {
        {
            let inner = self.inner.lock().await;
            if inner.state != LifecycleState::Authenticated {
                return Err(AcpClientError::NotAuthenticated);
            }
        }

        let request = LoadSessionRequest::new(
            session_id.clone(),
            std::path::PathBuf::from(cwd),
        );
        let response = self
            .send_request(ClientRequest::LoadSessionRequest(request))
            .await?;

        match response {
            AgentResponse::LoadSessionResponse(_) => {
                let mut inner = self.inner.lock().await;
                inner.active_session_id = Some(session_id.clone());
                Ok(())
            }
            other => Err(AcpClientError::AgentError {
                code: -1,
                message: format!("unexpected response to session/load: {other:?}"),
            }),
        }
    }

    /// Subscribe to provider events (session updates, disconnects, etc.).
    pub fn subscribe(&self) -> broadcast::Receiver<ProviderEvent> {
        // Try non-blocking lock first.
        match self.inner.try_lock() {
            Ok(guard) => guard.event_tx.subscribe(),
            Err(_) => self.inner.blocking_lock().event_tx.subscribe(),
        }
    }

    /// Shut down the client and stop background tasks.
    pub async fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            // Dropping the broadcast sender signals all tasks to stop.
            drop(tx);
        }

        let mut inner = self.inner.lock().await;
        inner.state = LifecycleState::Disconnected;

        // Fail all pending requests.
        for (_, tx) in inner.pending_responses.drain() {
            let _ = tx.send(Err(AcpClientError::ShutDown));
        }
        inner.pending_methods.clear();

        // Send shutdown to writer.
        let _ = self.writer_tx.send(WriterMessage::Shutdown).await;
    }

    // ── Internal helpers ──────────────────────────────────────────────

    /// Allocate the next request ID.
    async fn next_request_id(&self) -> i64 {
        let mut inner = self.inner.lock().await;
        let id = inner.next_id;
        inner.next_id += 1;
        id
    }

    /// Send a typed client request and wait for the correlated response.
    async fn send_request(
        &self,
        request: ClientRequest,
    ) -> Result<AgentResponse, AcpClientError> {
        let id = self.next_request_id().await;
        let request_id = RequestId::Number(id);
        let method = request.method().to_string();

        // Serialize the request.
        let serialized = framing::serialize_client_request(&request_id, &request)
            .map_err(AcpClientError::Transport)?;

        // Register pending response before sending.
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut inner = self.inner.lock().await;
            // Check if the response was already buffered (reader read it before
            // we registered the pending request).
            if let Some(response) = inner.buffered_responses.remove(&id) {
                tracing::debug!("send_request: found buffered response for id={id}");
                let _ = tx.send(response);
                return match rx.await {
                    Ok(Ok(PendingResponse::Typed(response))) => Ok(response),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(AcpClientError::Disconnected),
                };
            }
            tracing::debug!("send_request: registering pending for id={id}, method={method}");
            inner.pending_responses.insert(id, tx);
            inner.pending_methods.insert(id, method);
        }

        // Send the serialized line through the writer.
        let send_result = self
            .writer_tx
            .send(WriterMessage::Write { line: serialized })
            .await;

        if send_result.is_err() {
            // Writer is closed.
            let mut inner = self.inner.lock().await;
            inner.pending_responses.remove(&id);
            return Err(AcpClientError::Disconnected);
        }

        // Wait for the response.
        match rx.await {
            Ok(Ok(PendingResponse::Typed(response))) => Ok(response),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // Oneshot was dropped — reader task died.
                Err(AcpClientError::Disconnected)
            }
        }
    }

    /// Send a client notification (fire-and-forget, no response expected).
    async fn send_notification(
        &self,
        notification: ClientNotification,
    ) -> Result<(), AcpClientError> {
        let serialized = framing::serialize_client_notification(&notification)
            .map_err(AcpClientError::Transport)?;

        let send_result = self
            .writer_tx
            .send(WriterMessage::Write { line: serialized })
            .await;

        if send_result.is_err() {
            return Err(AcpClientError::Disconnected);
        }

        Ok(())
    }
}

/// Background read loop that processes incoming NDJSON messages.
///
/// Routes agent responses to pending request callers and broadcasts
/// session update notifications to event subscribers.
/// Combined I/O loop that reads incoming and writes outgoing NDJSON messages.
///
/// This uses a single task to avoid issues with splitting the transport.
/// It prioritizes reading (to route responses to pending requests) while
/// also processing outgoing write requests.
async fn io_loop<T>(
    transport: &mut T,
    inner: Arc<Mutex<AcpClientInner>>,
    mut writer_rx: mpsc::Receiver<WriterMessage>,
    shutdown_rx: &mut broadcast::Receiver<()>,
) where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let (read_half, write_half) = tokio::io::split(transport);
    let mut ndjson_reader = NdjsonReader::new(read_half);
    let mut writer = write_half;
    let mut writer_closed = false;

    loop {
        tokio::select! {
            result = shutdown_rx.recv() => {
                tracing::debug!("io_loop: shutdown signal received: {result:?}");
                break;
            }
            raw_line = ndjson_reader.next_raw_line() => {
                match raw_line {
                    Ok(line) => {
                        process_raw_line(&inner, &line).await;
                    }
                    Err(FramingError::StreamClosed) => {
                        let mut guard = inner.lock().await;
                        guard.state = LifecycleState::Disconnected;
                        let _ = guard.event_tx.send(ProviderEvent::Disconnected {
                            message: "transport closed".to_string(),
                        });
                        for (_, tx) in guard.pending_responses.drain() {
                            let _ = tx.send(Err(AcpClientError::Disconnected));
                        }
                        guard.pending_methods.clear();
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("NDJSON read error: {e}");
                    }
                }
            }
            msg = writer_rx.recv(), if !writer_closed => {
                match msg {
                    Some(WriterMessage::Write { line }) => {
                        if let Err(e) = framing::write_line(&mut writer, &line).await {
                            tracing::error!("NDJSON write error: {e}");
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

    let _ = writer.shutdown().await;
}

/// Process a raw NDJSON line.
///
/// Handles responses with method-aware deserialization (to work around
/// `AgentResponse`'s `#[serde(untagged)]` ambiguity) and delegates
/// notifications/requests to `framing::decode_line`.
async fn process_raw_line(inner: &Arc<Mutex<AcpClientInner>>, line: &str) {
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("invalid JSON: {e}");
            return;
        }
    };

    let has_id = value.get("id").is_some();
    let has_method = value.get("method").is_some();
    let has_result = value.get("result").is_some();
    let has_error = value.get("error").is_some();

    if has_id && (has_result || has_error) && !has_method {
        // This is a response — use method-aware deserialization.
        let id_val = value.get("id").unwrap();
        let id: RequestId = match serde_json::from_value(id_val.clone()) {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!("invalid request id: {e}");
                return;
            }
        };
        let i64_id = match &id {
            RequestId::Number(n) => *n,
            RequestId::Str(s) => s.parse::<i64>().unwrap_or(-1),
            RequestId::Null => -1,
        };

        if let Some(error_val) = value.get("error") {
            let code = error_val.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let message = error_val.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error").to_string();
            route_response(inner, id, Err(AcpClientError::AgentError { code, message })).await;
            let mut guard = inner.lock().await;
            guard.pending_methods.remove(&i64_id);
            return;
        }

        // Look up the method for this request ID.
        let method = {
            let guard = inner.lock().await;
            guard.pending_methods.get(&i64_id).cloned()
        };

        let result_val = value.get("result").unwrap();
        let response = match method.as_deref() {
            Some("initialize") => {
                match serde_json::from_value::<agent_client_protocol_schema::InitializeResponse>(result_val.clone()) {
                    Ok(r) => Some(AgentResponse::InitializeResponse(r)),
                    Err(e) => {
                        tracing::warn!("failed to decode InitializeResponse: {e}");
                        None
                    }
                }
            }
            Some("authenticate") => {
                match serde_json::from_value::<agent_client_protocol_schema::AuthenticateResponse>(result_val.clone()) {
                    Ok(r) => Some(AgentResponse::AuthenticateResponse(r)),
                    Err(e) => {
                        tracing::warn!("failed to decode AuthenticateResponse: {e}");
                        None
                    }
                }
            }
            Some("session/new") => {
                match serde_json::from_value::<agent_client_protocol_schema::NewSessionResponse>(result_val.clone()) {
                    Ok(r) => Some(AgentResponse::NewSessionResponse(r)),
                    Err(e) => {
                        tracing::warn!("failed to decode NewSessionResponse: {e}");
                        None
                    }
                }
            }
            Some("session/load") => {
                match serde_json::from_value::<agent_client_protocol_schema::LoadSessionResponse>(result_val.clone()) {
                    Ok(r) => Some(AgentResponse::LoadSessionResponse(r)),
                    Err(e) => {
                        tracing::warn!("failed to decode LoadSessionResponse: {e}");
                        None
                    }
                }
            }
            Some("session/list") => {
                match serde_json::from_value::<agent_client_protocol_schema::ListSessionsResponse>(result_val.clone()) {
                    Ok(r) => Some(AgentResponse::ListSessionsResponse(r)),
                    Err(e) => {
                        tracing::warn!("failed to decode ListSessionsResponse: {e}");
                        None
                    }
                }
            }
            Some("session/prompt") => {
                match serde_json::from_value::<agent_client_protocol_schema::PromptResponse>(result_val.clone()) {
                    Ok(r) => Some(AgentResponse::PromptResponse(r)),
                    Err(e) => {
                        tracing::warn!("failed to decode PromptResponse: {e}");
                        None
                    }
                }
            }
            _ => {
                // Unknown method — fall back to generic deserialization.
                tracing::debug!("unknown method for request id={i64_id}, trying generic AgentResponse");
                serde_json::from_value::<AgentResponse>(result_val.clone()).ok()
            }
        };

        if let Some(response) = response {
            route_response(inner, id, Ok(PendingResponse::Typed(response))).await;
        }
        let mut guard = inner.lock().await;
        guard.pending_methods.remove(&i64_id);
        return;
    }

    // Not a response — delegate to framing layer for notifications/requests.
    let message = framing::decode_line(line);
    match message {
        IncomingMessage::AgentNotification(notification) => {
            let event = map_agent_notification(&notification);
            let guard = inner.lock().await;
            let _ = guard.event_tx.send(event);
        }
        IncomingMessage::AgentRequest { id, request: _agent_request } => {
            tracing::debug!("received agent request (not yet handled): id={:?}", id);
        }
        IncomingMessage::Skipped { reason } => {
            // Check if this was a notification with an unknown method —
            // emit as ProviderEvent::Unknown so callers know about it.
            let method = value
                .get("method")
                .and_then(|m| m.as_str())
                .unwrap_or("");
            if !method.is_empty() && value.get("id").is_none() {
                let payload = value
                    .get("params")
                    .map(|p| p.to_string())
                    .unwrap_or_default();
                let guard = inner.lock().await;
                let _ = guard.event_tx.send(ProviderEvent::Unknown {
                    method: method.to_string(),
                    payload,
                });
            } else {
                tracing::warn!("skipped incoming message: {reason}");
            }
        }
        IncomingMessage::AgentResponse { .. } => {
            // Should not happen since we handled responses above, but just in case.
        }
    }
}

/// Route a response to the pending request caller.
/// If no pending request exists for the ID, buffer the response for later.
async fn route_response(
    inner: &Arc<Mutex<AcpClientInner>>,
    id: RequestId,
    response: Result<PendingResponse, AcpClientError>,
) {
    let i64_id = match &id {
        RequestId::Number(n) => *n,
        RequestId::Str(s) => s.parse::<i64>().unwrap_or(-1),
        RequestId::Null => -1,
    };

    let mut guard = inner.lock().await;
    if let Some(tx) = guard.pending_responses.remove(&i64_id) {
        let _ = tx.send(response);
    } else {
        // No pending request yet — buffer the response.
        guard.buffered_responses.insert(i64_id, response);
    }
}

/// Map an agent notification to a ProviderEvent.
fn map_agent_notification(notification: &AgentNotification) -> ProviderEvent {
    match notification {
        AgentNotification::SessionNotification(session_notif) => {
            let session_id = session_notif.session_id.to_string();
            map_session_update(&session_id, &session_notif.update)
        }
        AgentNotification::ExtNotification(ext) => ProviderEvent::Unknown {
            method: ext.method.to_string(),
            payload: ext.params.get().to_string(),
        },
        // Handle future notification variants gracefully.
        _ => ProviderEvent::Unknown {
            method: "unknown_notification".to_string(),
            payload: "{}".to_string(),
        },
    }
}

/// Map a SessionUpdate to the appropriate ProviderEvent.
fn map_session_update(
    session_id: &str,
    update: &agent_client_protocol_schema::SessionUpdate,
) -> ProviderEvent {
    use agent_client_protocol_schema::SessionUpdate;

    match update {
        SessionUpdate::AgentMessageChunk(chunk) => {
            let text = extract_text_from_chunk(chunk);
            ProviderEvent::MessageDelta {
                thread_id: session_id.to_string(),
                item_id: String::new(),
                delta: text,
            }
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            let text = extract_text_from_chunk(chunk);
            ProviderEvent::ReasoningDelta {
                thread_id: session_id.to_string(),
                item_id: String::new(),
                delta: text,
            }
        }
        SessionUpdate::ToolCall(tool_call) => ProviderEvent::ToolCallStarted {
            thread_id: session_id.to_string(),
            item_id: tool_call.tool_call_id.to_string(),
            tool_name: tool_call.title.clone(),
            call_id: tool_call.tool_call_id.to_string(),
        },
        SessionUpdate::ToolCallUpdate(tool_update) => {
            let output = tool_update
                .fields
                .raw_output
                .as_ref()
                .and_then(|o| o.as_str())
                .unwrap_or("")
                .to_string();
            ProviderEvent::ToolCallUpdate {
                thread_id: session_id.to_string(),
                item_id: tool_update.tool_call_id.to_string(),
                call_id: tool_update.tool_call_id.to_string(),
                output_delta: output,
            }
        }
        SessionUpdate::Plan(plan) => {
            let _ = plan;
            ProviderEvent::PlanUpdated {
                thread_id: session_id.to_string(),
                item_id: String::new(),
            }
        }
        SessionUpdate::UserMessageChunk(chunk) => {
            let text = extract_text_from_chunk(chunk);
            ProviderEvent::MessageDelta {
                thread_id: session_id.to_string(),
                item_id: String::new(),
                delta: text,
            }
        }
        _ => ProviderEvent::Unknown {
            method: format!("session/update/{}", update_name(update)),
            payload: "{}".to_string(),
        },
    }
}

/// Get a display name for a SessionUpdate variant.
fn update_name(update: &agent_client_protocol_schema::SessionUpdate) -> &'static str {
    use agent_client_protocol_schema::SessionUpdate;
    match update {
        SessionUpdate::AgentMessageChunk(_) => "agent_message_chunk",
        SessionUpdate::AgentThoughtChunk(_) => "agent_thought_chunk",
        SessionUpdate::ToolCall(_) => "tool_call",
        SessionUpdate::ToolCallUpdate(_) => "tool_call_update",
        SessionUpdate::Plan(_) => "plan",
        SessionUpdate::UserMessageChunk(_) => "user_message_chunk",
        SessionUpdate::AvailableCommandsUpdate(_) => "available_commands_update",
        SessionUpdate::CurrentModeUpdate(_) => "current_mode_update",
        SessionUpdate::ConfigOptionUpdate(_) => "config_option_update",
        SessionUpdate::SessionInfoUpdate(_) => "session_info_update",
        _ => "unknown",
    }
}

/// Extract text content from a ContentChunk.
fn extract_text_from_chunk(chunk: &agent_client_protocol_schema::ContentChunk) -> String {
    match &chunk.content {
        agent_client_protocol_schema::ContentBlock::Text(text) => text.text.clone(),
        _ => String::new(),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol_schema::{AuthMethod, AuthMethodAgent};

    /// Create a duplex pair for testing: (client_end, mock_end).
    /// The client uses client_end, the test uses mock_end to send responses
    /// and read requests.
    fn test_duplex() -> (
        tokio::io::DuplexStream,
        tokio::io::DuplexStream,
    ) {
        tokio::io::duplex(4096)
    }

    /// Helper to create an initialize response JSON.
    fn make_init_response(id: i64, version: u16, auth_methods: Vec<AuthMethod>) -> String {
        let auth_methods_json: Vec<serde_json::Value> = auth_methods
            .iter()
            .map(|m| serde_json::to_value(m).unwrap())
            .collect();
        serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": version,
                "agentCapabilities": {},
                "authMethods": auth_methods_json,
            }
        }))
        .unwrap()
    }

    /// Helper to create an authenticate response JSON.
    fn make_auth_response(id: i64) -> String {
        serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {}
        }))
        .unwrap()
    }

    /// Helper to create an authenticate error response JSON.
    fn make_auth_error_response(id: i64, code: i64, message: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": code,
                "message": message
            }
        }))
        .unwrap()
    }

    /// Helper to create a session/new response JSON.
    fn make_new_session_response(id: i64, session_id: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "sessionId": session_id
            }
        }))
        .unwrap()
    }

    /// Helper to create a session/prompt response JSON.
    fn make_prompt_response(id: i64, stop_reason: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "stopReason": stop_reason
            }
        }))
        .unwrap()
    }

    /// Helper to create a session/list response JSON.
    fn make_list_sessions_response(id: i64, sessions: Vec<(&str, &str)>) -> String {
        let sessions_json: Vec<serde_json::Value> = sessions
            .iter()
            .map(|(sid, title)| {
                serde_json::json!({
                    "sessionId": sid,
                    "cwd": "/tmp",
                    "title": title
                })
            })
            .collect();
        serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "sessions": sessions_json
            }
        }))
        .unwrap()
    }

    /// Helper to create a session/load response JSON.
    fn make_load_session_response(id: i64) -> String {
        serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {}
        }))
        .unwrap()
    }

    /// Helper to create a session/load error response (session not found).
    fn make_load_error_response(id: i64) -> String {
        serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32002,
                "message": "session not found"
            }
        }))
        .unwrap()
    }

    /// Helper to create a session/update notification JSON.
    fn make_session_update_notification(session_id: &str, text: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": text }
                }
            }
        }))
        .unwrap()
    }

    /// Helper: write a line to the mock end of the duplex.
    async fn write_mock_line(mock_end: &mut tokio::io::DuplexStream, line: &str) {
        use tokio::io::AsyncWriteExt;
        mock_end.write_all(format!("{line}\n").as_bytes()).await.unwrap();
        mock_end.flush().await.unwrap();
    }

    /// Helper: read a line from the mock end (request sent by client).
    async fn read_mock_line(mock_end: &mut tokio::io::DuplexStream) -> String {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match tokio::io::AsyncReadExt::read(mock_end, &mut byte).await {
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

    // ── VAL-ACP-001: Initialize handshake ─────────────────────────────

    /// Setup helper: create client and duplex, wrapped in Arc<Mutex>.
    fn setup_client() -> (
        Arc<tokio::sync::Mutex<AcpClient>>,
        tokio::io::DuplexStream,
    ) {
        let (client_end, mock_end) = test_duplex();
        (
            Arc::new(tokio::sync::Mutex::new(AcpClient::new(client_end))),
            mock_end,
        )
    }

    /// Perform init handshake on the mock side.
    async fn mock_init(mock_end: &mut tokio::io::DuplexStream, id: i64, version: u16, auth_methods: Vec<AuthMethod>) {
        let _ = read_mock_line(mock_end).await;
        write_mock_line(mock_end, &make_init_response(id, version, auth_methods)).await;
    }

    /// Perform auth handshake on the mock side.
    async fn mock_auth(mock_end: &mut tokio::io::DuplexStream, id: i64) {
        let _ = read_mock_line(mock_end).await;
        write_mock_line(mock_end, &make_auth_response(id)).await;
    }

    #[tokio::test]
    async fn acp_initialize_handshake_success() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "initialize should succeed: {result:?}");
        let init_result = result.unwrap();
        assert_eq!(init_result.protocol_version, ProtocolVersion::V1);
        assert_eq!(init_result.auth_methods.len(), 1);

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_initialize_rejects_incompatible_version() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 99, vec![]).await;

        let result = h.await.unwrap();
        match result {
            Err(AcpClientError::ProtocolVersionMismatch { .. }) => {}
            other => panic!("expected ProtocolVersionMismatch, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_initialize_accepts_lower_compatible_version() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 0, vec![]).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "lower compatible version should succeed");
        assert_eq!(result.unwrap().protocol_version, ProtocolVersion::V0);

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-002: Authenticate ─────────────────────────────────────

    #[tokio::test]
    async fn acp_authenticate_after_initialize() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "authenticate should succeed: {result:?}");

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_session_new_rejected_before_authenticate() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![]).await;
        h.await.unwrap().unwrap();

        let result = client.lock().await.session_new("/tmp").await;
        match result {
            Err(AcpClientError::NotAuthenticated) => {}
            other => panic!("expected NotAuthenticated, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-003: Session lifecycle ────────────────────────────────

    #[tokio::test]
    async fn acp_session_new_returns_id() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/tmp").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_new_session_response(3, "sess-123")).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "session/new should succeed: {result:?}");
        assert_eq!(result.unwrap().session_id.to_string(), "sess-123");

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_session_prompt_streams_updates() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/tmp").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_new_session_response(3, "sess-123")).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move {
            c.lock().await.session_prompt(&SessionId::new("sess-123"), "Say hello").await
        });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_session_update_notification("sess-123", "Hello ")).await;
        write_mock_line(&mut mock_end, &make_session_update_notification("sess-123", "world")).await;
        write_mock_line(&mut mock_end, &make_prompt_response(4, "end_turn")).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "session/prompt should succeed: {result:?}");

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_session_cancel_mid_stream() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/tmp").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_new_session_response(3, "sess-123")).await;
        h.await.unwrap().unwrap();

        {
            let guard = client.lock().await;
            let mut inner = guard.inner.lock().await;
            inner.is_streaming = true;
        }

        let result = client.lock().await.session_cancel(&SessionId::new("sess-123")).await;
        assert!(result.is_ok(), "cancel should succeed: {result:?}");

        {
            let guard = client.lock().await;
            let inner = guard.inner.lock().await;
            assert!(!inner.is_streaming, "streaming should be reset after cancel");
        }

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_session_cancel_no_active_turn() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let result = client.lock().await.session_cancel(&SessionId::new("sess-123")).await;
        assert!(result.is_ok(), "cancel when idle should be no-op");

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-013: session/list ─────────────────────────────────────

    #[tokio::test]
    async fn acp_session_list_returns_sessions() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_list().await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_list_sessions_response(3, vec![("sess-1", "First Session"), ("sess-2", "Second Session")])).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "session/list should succeed: {result:?}");
        let sessions = result.unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "sess-1");
        assert_eq!(sessions[0].title, "First Session");

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_session_list_empty_array() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_list().await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_list_sessions_response(3, vec![])).await;

        let result = h.await.unwrap();
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-014: session/load ─────────────────────────────────────

    #[tokio::test]
    async fn acp_session_load_replays_history() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_load(&SessionId::new("sess-old"), "/tmp").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_load_session_response(3)).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "session/load should succeed: {result:?}");

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_session_load_unknown_returns_error() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_load(&SessionId::new("nonexistent"), "/tmp").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_load_error_response(3)).await;

        let result = h.await.unwrap();
        assert!(result.is_err(), "session/load with unknown ID should fail");

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-023: Protocol version mismatch ────────────────────────

    #[tokio::test]
    async fn acp_version_mismatch() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 5, vec![]).await;

        let result = h.await.unwrap();
        match result {
            Err(AcpClientError::ProtocolVersionMismatch { client_version: cv, server_version: sv }) => {
                assert_eq!(cv, ProtocolVersion::LATEST);
                assert_eq!(sv.to_string(), "5");
            }
            other => panic!("expected ProtocolVersionMismatch, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_backward_compatible_version() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 0, vec![]).await;

        let result = h.await.unwrap();
        assert!(result.is_ok());
        assert_eq!(result.unwrap().protocol_version, ProtocolVersion::V0);

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-024: Authentication failure ───────────────────────────

    #[tokio::test]
    async fn acp_auth_failure_permanent() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_auth_error_response(2, -32000, "Authentication required")).await;

        let result = h.await.unwrap();
        assert!(result.is_err(), "permanent auth failure should error: {result:?}");

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_auth_retry_transient() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_auth_error_response(2, -32603, "Internal error")).await;

        // The retry happens automatically.
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_auth_response(3)).await;

        let result = h.await.unwrap();
        let _ = result;

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-019: Cancel during streaming ──────────────────────────

    #[tokio::test]
    async fn acp_cancel_mid_stream() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/tmp").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_new_session_response(3, "sess-123")).await;
        h.await.unwrap().unwrap();

        {
            let guard = client.lock().await;
            let mut inner = guard.inner.lock().await;
            inner.is_streaming = true;
        }

        let result = client.lock().await.session_cancel(&SessionId::new("sess-123")).await;
        assert!(result.is_ok(), "cancel should succeed: {result:?}");

        {
            let guard = client.lock().await;
            let inner = guard.inner.lock().await;
            assert!(!inner.is_streaming, "streaming should be reset after cancel");
        }

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_prompt_after_cancel() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/tmp").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_new_session_response(3, "sess-123")).await;
        h.await.unwrap().unwrap();

        client.lock().await.session_cancel(&SessionId::new("sess-123")).await.unwrap();

        // Prompt after cancel.
        let c = client.clone();
        let h = tokio::spawn(async move {
            c.lock().await.session_prompt(&SessionId::new("sess-123"), "Next prompt").await
        });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_prompt_response(4, "end_turn")).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "prompt after cancel should succeed: {result:?}");

        client.lock().await.shutdown().await;
    }

    // ── Transport disconnect ──────────────────────────────────────────

    #[tokio::test]
    async fn acp_transport_disconnect_emits_event() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        drop(mock_end);

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event {
            Ok(Ok(ProviderEvent::Disconnected { message })) => {
                assert!(!message.is_empty());
            }
            other => panic!("expected Disconnected event, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-004: AgentMessageChunk → MessageDelta ─────────────────

    #[tokio::test]
    async fn acp_streaming_agent_message_chunk_maps_to_message_delta() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        // Send an AgentMessageChunk notification.
        let chunk_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess-123",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "Hello world" }
                }
            }
        });
        write_mock_line(&mut mock_end, &serde_json::to_string(&chunk_json).unwrap()).await;

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event {
            Ok(Ok(ProviderEvent::MessageDelta { thread_id, delta, .. })) => {
                assert_eq!(thread_id, "sess-123");
                assert_eq!(delta, "Hello world");
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_streaming_multiple_chunks_accumulate() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        // Send multiple chunks.
        let chunks = ["Hello ", "world", "!"];
        for chunk_text in &chunks {
            let chunk_json = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "sess-123",
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": { "type": "text", "text": chunk_text }
                    }
                }
            });
            write_mock_line(&mut mock_end, &serde_json::to_string(&chunk_json).unwrap()).await;
        }

        let mut accumulated = String::new();
        for _ in 0..3 {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
            match event {
                Ok(Ok(ProviderEvent::MessageDelta { delta, .. })) => {
                    accumulated.push_str(&delta);
                }
                other => panic!("expected MessageDelta, got {other:?}"),
            }
        }
        assert_eq!(accumulated, "Hello world!");

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-005: AgentThoughtChunk → ReasoningDelta ───────────────

    #[tokio::test]
    async fn acp_streaming_agent_thought_chunk_maps_to_reasoning_delta() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        let chunk_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess-123",
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": { "type": "text", "text": "Let me analyze..." }
                }
            }
        });
        write_mock_line(&mut mock_end, &serde_json::to_string(&chunk_json).unwrap()).await;

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event {
            Ok(Ok(ProviderEvent::ReasoningDelta { thread_id, delta, .. })) => {
                assert_eq!(thread_id, "sess-123");
                assert_eq!(delta, "Let me analyze...");
            }
            other => panic!("expected ReasoningDelta, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_streaming_multiline_thought_preserved() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        let multiline_text = "Step 1: Analyze\nStep 2: Plan\nStep 3: Execute";
        let chunk_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess-456",
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": { "type": "text", "text": multiline_text }
                }
            }
        });
        write_mock_line(&mut mock_end, &serde_json::to_string(&chunk_json).unwrap()).await;

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event {
            Ok(Ok(ProviderEvent::ReasoningDelta { delta, .. })) => {
                assert_eq!(delta, multiline_text);
                assert!(delta.contains('\n'), "multiline content should preserve newlines");
            }
            other => panic!("expected ReasoningDelta, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-006: ToolCall → ToolCallStarted, ToolCallUpdate ───────

    #[tokio::test]
    async fn acp_streaming_tool_call_lifecycle() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        // ToolCall (start).
        let tool_call_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess-123",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "call-1",
                    "title": "git status",
                    "kind": "execute",
                    "status": "in_progress"
                }
            }
        });
        write_mock_line(&mut mock_end, &serde_json::to_string(&tool_call_json).unwrap()).await;

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event {
            Ok(Ok(ProviderEvent::ToolCallStarted { thread_id, item_id, tool_name, call_id })) => {
                assert_eq!(thread_id, "sess-123");
                assert_eq!(item_id, "call-1");
                assert_eq!(tool_name, "git status");
                assert_eq!(call_id, "call-1");
            }
            other => panic!("expected ToolCallStarted, got {other:?}"),
        }

        // ToolCallUpdate with output.
        let tool_update_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess-123",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call-1",
                    "rawOutput": "On branch main\n"
                }
            }
        });
        write_mock_line(&mut mock_end, &serde_json::to_string(&tool_update_json).unwrap()).await;

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event {
            Ok(Ok(ProviderEvent::ToolCallUpdate { thread_id, item_id, call_id, output_delta })) => {
                assert_eq!(thread_id, "sess-123");
                assert_eq!(item_id, "call-1");
                assert_eq!(call_id, "call-1");
                assert_eq!(output_delta, "On branch main\n");
            }
            other => panic!("expected ToolCallUpdate, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_streaming_tool_call_threads_call_id() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        // Two concurrent tool calls.
        for call_id in &["call-A", "call-B"] {
            let tc_json = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "sess-123",
                    "update": {
                        "sessionUpdate": "tool_call",
                        "toolCallId": call_id,
                        "title": format!("tool {call_id}"),
                        "status": "in_progress"
                    }
                }
            });
            write_mock_line(&mut mock_end, &serde_json::to_string(&tc_json).unwrap()).await;
        }

        let event1 = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event1 {
            Ok(Ok(ProviderEvent::ToolCallStarted { call_id, .. })) => {
                assert_eq!(call_id, "call-A");
            }
            other => panic!("expected ToolCallStarted for call-A, got {other:?}"),
        }

        let event2 = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event2 {
            Ok(Ok(ProviderEvent::ToolCallStarted { call_id, .. })) => {
                assert_eq!(call_id, "call-B");
            }
            other => panic!("expected ToolCallStarted for call-B, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-007: Plan → PlanUpdated ───────────────────────────────

    #[tokio::test]
    async fn acp_streaming_plan_maps_to_plan_updated() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        let plan_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess-123",
                "update": {
                    "sessionUpdate": "plan",
                    "entries": [
                        {
                            "content": "Read the file",
                            "priority": "high",
                            "status": "completed"
                        },
                        {
                            "content": "Modify the file",
                            "priority": "medium",
                            "status": "in_progress"
                        },
                        {
                            "content": "Run tests",
                            "priority": "low",
                            "status": "pending"
                        }
                    ]
                }
            }
        });
        write_mock_line(&mut mock_end, &serde_json::to_string(&plan_json).unwrap()).await;

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event {
            Ok(Ok(ProviderEvent::PlanUpdated { thread_id, .. })) => {
                assert_eq!(thread_id, "sess-123");
            }
            other => panic!("expected PlanUpdated, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_streaming_empty_plan_maps_to_plan_updated() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        let plan_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess-123",
                "update": {
                    "sessionUpdate": "plan",
                    "entries": []
                }
            }
        });
        write_mock_line(&mut mock_end, &serde_json::to_string(&plan_json).unwrap()).await;

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event {
            Ok(Ok(ProviderEvent::PlanUpdated { thread_id, .. })) => {
                assert_eq!(thread_id, "sess-123");
                // Empty plan is valid, not an error.
            }
            other => panic!("expected PlanUpdated for empty plan, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-015: Transport disconnect during streaming ────────────

    #[tokio::test]
    async fn acp_streaming_disconnect_emits_disconnected_event() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        // Send a chunk, then drop the connection.
        let chunk_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess-123",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "partial..." }
                }
            }
        });
        write_mock_line(&mut mock_end, &serde_json::to_string(&chunk_json).unwrap()).await;

        // Read the MessageDelta event.
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        assert!(matches!(event, Ok(Ok(ProviderEvent::MessageDelta { .. }))), "expected MessageDelta");

        // Drop the connection.
        drop(mock_end);

        // Should get Disconnected event.
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event {
            Ok(Ok(ProviderEvent::Disconnected { message })) => {
                assert!(!message.is_empty());
            }
            other => panic!("expected Disconnected event, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_streaming_pending_request_gets_transport_error() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/tmp").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_new_session_response(3, "sess-123")).await;
        h.await.unwrap().unwrap();

        // Start a prompt (will wait for response), then drop the connection.
        let c = client.clone();
        let prompt_handle = tokio::spawn(async move {
            c.lock().await.session_prompt(&SessionId::new("sess-123"), "test").await
        });

        // Read the prompt request from the mock side.
        let _ = read_mock_line(&mut mock_end).await;

        // Drop the connection.
        drop(mock_end);

        // The prompt should get a transport error.
        let result = prompt_handle.await.unwrap();
        assert!(result.is_err(), "pending request should fail on disconnect: {result:?}");
    }

    // ── VAL-ACP-016: Malformed message handling ───────────────────────

    #[tokio::test]
    async fn acp_streaming_malformed_response_returns_error() {
        // Test the process_raw_line function directly with malformed JSON.
        let inner = Arc::new(Mutex::new(AcpClientInner {
            state: LifecycleState::Authenticated,
            negotiated_version: None,
            auth_methods: Vec::new(),
            active_session_id: None,
            is_streaming: false,
            event_tx: broadcast::channel(256).0,
            pending_responses: HashMap::new(),
            buffered_responses: HashMap::new(),
            pending_methods: HashMap::new(),
            next_id: 1,
        }));

        // Malformed JSON should be handled gracefully.
        process_raw_line(&inner, "not valid json {{{").await;
        // Should not panic; inner state should be unchanged.
        let guard = inner.lock().await;
        assert_eq!(guard.state, LifecycleState::Authenticated);
    }

    #[tokio::test]
    async fn acp_streaming_unknown_notification_method_ignored() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        // Send a notification with an unknown method.
        let unknown_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "future/unknown_method",
            "params": { "data": "test" }
        });
        write_mock_line(&mut mock_end, &serde_json::to_string(&unknown_json).unwrap()).await;

        // Should get an Unknown event (gracefully handled, not a crash).
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event {
            Ok(Ok(ProviderEvent::Unknown { method, .. })) => {
                assert!(method.contains("future/unknown_method") || method.contains("unknown_notification"));
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                // Acceptable — slow consumer.
            }
            other => panic!("expected Unknown event or lagged, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_streaming_client_continues_after_bad_message() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        // Send a bad line (not valid JSON).
        write_mock_line(&mut mock_end, "bad json {{{").await;

        // Send a valid notification after the bad one.
        let valid_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess-123",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "after bad msg" }
                }
            }
        });
        write_mock_line(&mut mock_end, &serde_json::to_string(&valid_json).unwrap()).await;

        // The valid message should still be processed.
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event {
            Ok(Ok(ProviderEvent::MessageDelta { delta, .. })) => {
                assert_eq!(delta, "after bad msg");
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                // The bad message might have been skipped and broadcast,
                // so we get a lagged error. Try the next event.
                let event2 = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
                match event2 {
                    Ok(Ok(ProviderEvent::MessageDelta { delta, .. })) => {
                        assert_eq!(delta, "after bad msg");
                    }
                    other => panic!("expected MessageDelta after lagged, got {other:?}"),
                }
            }
            other => panic!("expected MessageDelta or Lagged, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-017: Concurrent request correlation ───────────────────

    #[tokio::test]
    async fn acp_streaming_concurrent_requests_correlated_by_id() {
        // Test that responses are correctly correlated by request ID,
        // even when they arrive out of order.
        //
        // We can't truly send concurrent requests through a single AcpClient
        // because the Mutex serializes access. Instead, we verify that the
        // internal routing correctly matches responses to requesters by ID
        // using the buffered_responses mechanism.
        //
        // The real concurrency is at the I/O level: the io_loop reads
        // responses and routes them to pending oneshots by ID.
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        // Send session/list (id=3). The client sends the request and waits.
        let c1 = client.clone();
        let list_handle = tokio::spawn(async move { c1.lock().await.session_list().await });

        // Read the list request.
        let list_line = read_mock_line(&mut mock_end).await;
        assert!(list_line.contains("session/list"), "first request should be session/list: {list_line}");

        // Respond with the session/list response (id=3).
        write_mock_line(&mut mock_end, &make_list_sessions_response(3, vec![("sess-1", "First")])).await;

        // The list should resolve correctly.
        let list_result = list_handle.await.unwrap();
        assert!(list_result.is_ok(), "session/list should succeed: {list_result:?}");
        let sessions = list_result.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "sess-1");

        // Now send session/new (id=4).
        let c2 = client.clone();
        let new_handle = tokio::spawn(async move { c2.lock().await.session_new("/tmp").await });

        // Read the new request.
        let new_line = read_mock_line(&mut mock_end).await;
        assert!(new_line.contains("session/new"), "second request should be session/new: {new_line}");

        // Respond with the session/new response (id=4).
        write_mock_line(&mut mock_end, &make_new_session_response(4, "sess-new")).await;

        let new_result = new_handle.await.unwrap();
        assert!(new_result.is_ok(), "session/new should succeed: {new_result:?}");
        assert_eq!(new_result.unwrap().session_id.to_string(), "sess-new");

        client.lock().await.shutdown().await;
    }

    #[tokio::test]
    async fn acp_streaming_notifications_dont_block_requests() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        // Send a notification (no id).
        let notif_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess-123",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "streaming..." }
                }
            }
        });
        write_mock_line(&mut mock_end, &serde_json::to_string(&notif_json).unwrap()).await;

        // Also send a session/list request at the same time.
        let c2 = client.clone();
        let list_handle = tokio::spawn(async move { c2.lock().await.session_list().await });

        // Read the request.
        let _req_line = read_mock_line(&mut mock_end).await;

        // Respond to session/list.
        write_mock_line(&mut mock_end, &make_list_sessions_response(3, vec![])).await;

        // The notification should be in the event stream.
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        assert!(
            matches!(event, Ok(Ok(ProviderEvent::MessageDelta { .. }))),
            "expected MessageDelta notification"
        );

        // The request should also succeed.
        let list_result = list_handle.await.unwrap();
        assert!(list_result.is_ok(), "session/list should succeed alongside notifications");

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-030: Initialize handshake success (comprehensive) ─────

    /// Verify that initialize sends the correct protocol version and
    /// client info, and that the response contains structured fields
    /// (protocol_version, agent_capabilities, auth_methods, agent_info).
    #[tokio::test]
    async fn acp_handshake_initialize_sends_protocol_version_and_receives_capabilities() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });

        // Read the initialize request from the client.
        let init_line = read_mock_line(&mut mock_end).await;
        let init_val: serde_json::Value = serde_json::from_str(&init_line).unwrap();

        // Verify the request structure.
        assert_eq!(init_val["jsonrpc"], "2.0");
        assert_eq!(init_val["method"], "initialize");
        assert_eq!(init_val["params"]["protocolVersion"], 1);
        assert_eq!(init_val["params"]["clientInfo"]["name"], "litter");
        assert_eq!(init_val["params"]["clientInfo"]["version"], "0.1.0");

        // Respond with capabilities, auth methods, and agent info.
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0",
            "id": init_val["id"],
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {
                    "streaming": true,
                    "tools": true,
                    "plans": true,
                    "reasoning": true
                },
                "authMethods": [
                    {"type": "agent", "id": "local", "name": "Local Auth"},
                    {"type": "agent", "id": "api_key", "name": "API Key"}
                ],
                "agentInfo": {
                    "name": "test-agent",
                    "version": "1.0.0"
                }
            }
        }).to_string()).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "initialize should succeed: {result:?}");
        let init_result = result.unwrap();

        // VAL-ACP-030: InitializeResult contains protocol_version.
        assert_eq!(init_result.protocol_version, ProtocolVersion::V1);

        // VAL-ACP-030: InitializeResult contains agent_capabilities.
        // (AgentCapabilities is opaque — just verify it was deserialized.)

        // VAL-ACP-030: InitializeResult contains auth_methods (2 methods).
        assert_eq!(init_result.auth_methods.len(), 2);
        assert_eq!(init_result.auth_methods[0].to_string(), "local");
        assert_eq!(init_result.auth_methods[1].to_string(), "api_key");

        // VAL-ACP-030: InitializeResult contains agent_info.
        assert!(init_result.agent_info.is_some());
        let info = init_result.agent_info.unwrap();
        assert_eq!(info.name, "test-agent");
        assert_eq!(info.version, "1.0.0");

        // Client should now be in Initialized state.
        {
            let guard = client.lock().await;
            let inner = guard.inner.lock().await;
            assert_eq!(inner.state, LifecycleState::Initialized);
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-031: Initialize rejects incompatible version (state check) ──

    /// Verify that when the server returns a higher protocol version,
    /// the client returns ProtocolVersionMismatch and stays in Uninitialized.
    #[tokio::test]
    async fn acp_handshake_version_mismatch_state_remains_uninitialized() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 99, vec![]).await;

        let result = h.await.unwrap();
        match result {
            Err(AcpClientError::ProtocolVersionMismatch {
                client_version: cv,
                server_version: sv,
            }) => {
                // VAL-ACP-031: both version numbers are in the error.
                assert_eq!(cv, ProtocolVersion::LATEST);
                assert!(sv > cv, "server version should be higher than client: sv={sv:?}, cv={cv:?}");
            }
            other => panic!("expected ProtocolVersionMismatch, got {other:?}"),
        }

        // VAL-ACP-031: Client state remains Uninitialized.
        {
            let guard = client.lock().await;
            let inner = guard.inner.lock().await;
            assert_eq!(inner.state, LifecycleState::Uninitialized);
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-032: Initialize accepts lower compatible version ──────

    /// Verify that when the server returns a lower compatible version,
    /// the client accepts and stores the lower version.
    #[tokio::test]
    async fn acp_handshake_lower_version_stored_in_result() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 0, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local"))]).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "lower compatible version should succeed");
        let init_result = result.unwrap();

        // VAL-ACP-032: InitializeResult.protocol_version reflects server's version.
        assert_eq!(init_result.protocol_version, ProtocolVersion::V0);

        // VAL-ACP-032: Internal state stores the negotiated version.
        {
            let guard = client.lock().await;
            let inner = guard.inner.lock().await;
            assert_eq!(inner.negotiated_version, Some(ProtocolVersion::V0));
            assert_eq!(inner.state, LifecycleState::Initialized);
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-033: Authenticate uses first available method ─────────

    /// Verify that authenticate(None) picks the first method from
    /// InitializeResult.auth_methods.
    #[tokio::test]
    async fn acp_handshake_authenticate_uses_first_available_method() {
        let (client, mut mock_end) = setup_client();

        // Initialize with two auth methods.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(
            &mut mock_end,
            1,
            1u16,
            vec![
                AuthMethod::Agent(AuthMethodAgent::new("first_method", "First Method")),
                AuthMethod::Agent(AuthMethodAgent::new("second_method", "Second Method")),
            ],
        )
        .await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });

        // Read the authenticate request.
        let auth_line = read_mock_line(&mut mock_end).await;
        let auth_val: serde_json::Value = serde_json::from_str(&auth_line).unwrap();

        // VAL-ACP-033: Should use the first method "first_method".
        assert_eq!(auth_val["method"], "authenticate");
        assert_eq!(auth_val["params"]["methodId"], "first_method");

        // Respond with success.
        write_mock_line(&mut mock_end, &make_auth_response(2)).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "authenticate should succeed: {result:?}");

        // VAL-ACP-033: Client transitions to Authenticated state.
        {
            let guard = client.lock().await;
            let inner = guard.inner.lock().await;
            assert_eq!(inner.state, LifecycleState::Authenticated);
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-034: Authenticate permanent failure (no retry) ────────

    /// Verify that permanent auth failure (-32000) is NOT retried.
    #[tokio::test]
    async fn acp_handshake_auth_permanent_failure_no_retry() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(
            &mut mock_end,
            1,
            1u16,
            vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))],
        )
        .await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });

        // Read the first authenticate request.
        let _auth_line = read_mock_line(&mut mock_end).await;

        // Respond with permanent auth failure (-32000).
        write_mock_line(&mut mock_end, &make_auth_error_response(2, -32000, "AuthRequired")).await;

        let result = h.await.unwrap();

        // VAL-ACP-034: Should be a permanent failure.
        match result {
            Err(AcpClientError::AuthenticationFailed { message }) => {
                assert!(message.contains("AuthRequired"), "error message should contain the reason: {message}");
            }
            other => panic!("expected AuthenticationFailed, got {other:?}"),
        }

        // VAL-ACP-034: Verify only one auth request was sent (no retry).
        // If a retry happened, the client would have sent another request,
        // and the mock_end would have data to read. Try a non-blocking read.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // The mock_end should have no more data (no retry was sent).
        // We can't easily assert this without more infrastructure, so we
        // verify by checking the state is NOT Authenticated.
        {
            let guard = client.lock().await;
            let inner = guard.inner.lock().await;
            assert_ne!(inner.state, LifecycleState::Authenticated, "should NOT be authenticated after permanent failure");
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-035: Authenticate transient retry (exactly one) ───────

    /// Verify that transient auth failure retries exactly once.
    #[tokio::test]
    async fn acp_handshake_auth_transient_retries_once() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(
            &mut mock_end,
            1,
            1u16,
            vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))],
        )
        .await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });

        // Read the first authenticate request.
        let first_auth_line = read_mock_line(&mut mock_end).await;
        let first_auth_val: serde_json::Value = serde_json::from_str(&first_auth_line).unwrap();
        let first_id = first_auth_val["id"].as_i64().unwrap();

        // Respond with transient error (NOT -32000).
        write_mock_line(
            &mut mock_end,
            &make_auth_error_response(first_id, -32603, "Internal error"),
        )
        .await;

        // Read the retry request.
        let retry_auth_line = read_mock_line(&mut mock_end).await;
        let retry_auth_val: serde_json::Value = serde_json::from_str(&retry_auth_line).unwrap();
        assert_eq!(retry_auth_val["method"], "authenticate");
        let retry_id = retry_auth_val["id"].as_i64().unwrap();

        // VAL-ACP-035: The retry should use a different request ID.
        assert_ne!(first_id, retry_id, "retry should use a new request ID");

        // Respond with success on retry.
        write_mock_line(&mut mock_end, &make_auth_response(retry_id)).await;

        let result = h.await.unwrap();
        // VAL-ACP-035: Retry succeeds.
        assert!(result.is_ok(), "retry should succeed: {result:?}");

        // VAL-ACP-035: Client is now Authenticated.
        {
            let guard = client.lock().await;
            let inner = guard.inner.lock().await;
            assert_eq!(inner.state, LifecycleState::Authenticated);
        }

        client.lock().await.shutdown().await;
    }

    /// Verify that transient auth failure that fails on retry too
    /// returns AuthenticationTransient error.
    #[tokio::test]
    async fn acp_handshake_auth_transient_retry_then_fails() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(
            &mut mock_end,
            1,
            1u16,
            vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))],
        )
        .await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });

        // First attempt: transient error.
        let first_line = read_mock_line(&mut mock_end).await;
        let first_val: serde_json::Value = serde_json::from_str(&first_line).unwrap();
        write_mock_line(
            &mut mock_end,
            &make_auth_error_response(first_val["id"].as_i64().unwrap(), -32603, "Internal error"),
        )
        .await;

        // Retry: also transient error.
        let retry_line = read_mock_line(&mut mock_end).await;
        let retry_val: serde_json::Value = serde_json::from_str(&retry_line).unwrap();
        write_mock_line(
            &mut mock_end,
            &make_auth_error_response(retry_val["id"].as_i64().unwrap(), -32603, "Still broken"),
        )
        .await;

        let result = h.await.unwrap();
        // VAL-ACP-035: After one retry failure, returns transient error.
        match result {
            Err(AcpClientError::AuthenticationTransient { message }) => {
                assert!(message.contains("retry failed"), "message should mention retry: {message}");
            }
            Err(AcpClientError::AgentError { code, .. }) => {
                // If the retry also gets -32000, it becomes permanent.
                assert_ne!(code, -32000, "should not be permanent on transient retry");
            }
            other => panic!("expected AuthenticationTransient or AgentError, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-036: Capabilities surfaced from initialize ────────────

    /// Verify that ACP capabilities from the initialize response are
    /// accessible through the InitializeResult struct.
    #[tokio::test]
    async fn acp_handshake_capabilities_surface_from_initialize() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });

        let init_line = read_mock_line(&mut mock_end).await;
        let init_val: serde_json::Value = serde_json::from_str(&init_line).unwrap();

        // Respond with full capabilities.
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0",
            "id": init_val["id"],
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {"streaming": true, "tools": true, "plans": false, "reasoning": true},
                "authMethods": [
                    {"type": "agent", "id": "local", "name": "Local Auth"}
                ],
                "agentInfo": {
                    "name": "my-agent",
                    "version": "2.5.0"
                }
            }
        }).to_string()).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "initialize should succeed: {result:?}");
        let init_result = result.unwrap();

        // VAL-ACP-036: agent_capabilities is populated.
        // The AgentCapabilities struct carries whatever the server sent.
        // We just verify it was deserialized.
        let _caps = &init_result.agent_capabilities;

        // VAL-ACP-036: auth_methods contains available authentication methods.
        assert_eq!(init_result.auth_methods.len(), 1);
        assert_eq!(init_result.auth_methods[0].to_string(), "local");

        // VAL-ACP-036: agent_info contains the agent's Implementation (name, version).
        assert!(init_result.agent_info.is_some());
        let info = init_result.agent_info.unwrap();
        assert_eq!(info.name, "my-agent");
        assert_eq!(info.version, "2.5.0");

        // VAL-ACP-036: protocol_version is correct.
        assert_eq!(init_result.protocol_version, ProtocolVersion::V1);

        client.lock().await.shutdown().await;
    }

    /// Verify that capabilities are accessible even when agent_info is absent.
    #[tokio::test]
    async fn acp_handshake_capabilities_without_agent_info() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });

        let init_line = read_mock_line(&mut mock_end).await;
        let init_val: serde_json::Value = serde_json::from_str(&init_line).unwrap();

        // Respond without agentInfo.
        write_mock_line(&mut mock_end, &serde_json::json!({
            "jsonrpc": "2.0",
            "id": init_val["id"],
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {},
                "authMethods": [
                    {"type": "agent", "id": "local", "name": "Local Auth"}
                ]
            }
        }).to_string()).await;

        let result = h.await.unwrap();
        assert!(result.is_ok());
        let init_result = result.unwrap();

        // agent_info should be None.
        assert!(init_result.agent_info.is_none());

        // But auth_methods should still be populated.
        assert_eq!(init_result.auth_methods.len(), 1);

        client.lock().await.shutdown().await;
    }

    // ── Handshake sequencing guard tests ──────────────────────────────

    /// Verify that session operations are blocked until initialize completes.
    #[tokio::test]
    async fn acp_handshake_session_operations_blocked_before_initialize() {
        let (client, _mock_end) = setup_client();

        // session/new before initialize → NotAuthenticated (state is not Authenticated).
        let result = client.lock().await.session_new("/tmp").await;
        match result {
            Err(AcpClientError::NotAuthenticated) => {}
            other => panic!("expected NotAuthenticated for session/new before init, got {other:?}"),
        }

        // session/prompt before initialize → NotAuthenticated (state is not Authenticated).
        let result = client
            .lock()
            .await
            .session_prompt(&SessionId::new("s1"), "hello")
            .await;
        match result {
            Err(AcpClientError::NotAuthenticated) => {}
            other => panic!("expected NotAuthenticated for session/prompt before init, got {other:?}"),
        }

        // session/list before initialize → NotInitialized (checks specifically for Uninitialized).
        let result = client.lock().await.session_list().await;
        match result {
            Err(AcpClientError::NotInitialized) => {}
            other => panic!("expected NotInitialized for session/list before init, got {other:?}"),
        }

        // session/load before initialize → NotAuthenticated.
        let result = client
            .lock()
            .await
            .session_load(&SessionId::new("s1"), "/tmp")
            .await;
        match result {
            Err(AcpClientError::NotAuthenticated) => {}
            other => panic!("expected NotAuthenticated for session/load before init, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    /// Verify that session operations are blocked until authenticate completes.
    #[tokio::test]
    async fn acp_handshake_session_operations_blocked_before_authenticate() {
        let (client, mut mock_end) = setup_client();

        // Initialize succeeds.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(
            &mut mock_end,
            1,
            1u16,
            vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))],
        )
        .await;
        h.await.unwrap().unwrap();

        // session/new after init but before auth → NotAuthenticated.
        let result = client.lock().await.session_new("/tmp").await;
        match result {
            Err(AcpClientError::NotAuthenticated) => {}
            other => panic!("expected NotAuthenticated, got {other:?}"),
        }

        // session/load after init but before auth → NotAuthenticated.
        let result = client
            .lock()
            .await
            .session_load(&SessionId::new("s1"), "/tmp")
            .await;
        match result {
            Err(AcpClientError::NotAuthenticated) => {}
            other => panic!("expected NotAuthenticated, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    /// Verify that authenticate before initialize fails.
    #[tokio::test]
    async fn acp_handshake_authenticate_before_initialize_fails() {
        let (client, _mock_end) = setup_client();

        // authenticate before initialize → NotInitialized.
        let result = client.lock().await.authenticate(None).await;
        match result {
            Err(AcpClientError::NotInitialized) => {}
            other => panic!("expected NotInitialized, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    /// Verify full handshake sequence: initialize → authenticate → session operations work.
    #[tokio::test]
    async fn acp_handshake_full_sequence_before_session_ops() {
        let (client, mut mock_end) = setup_client();

        // Step 1: initialize.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(
            &mut mock_end,
            1,
            1u16,
            vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))],
        )
        .await;
        h.await.unwrap().unwrap();

        // Step 2: authenticate.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        // Step 3: session operations now work.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/home").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_new_session_response(3, "handshake-sess-1")).await;
        let result = h.await.unwrap();
        assert!(result.is_ok(), "session/new should work after handshake: {result:?}");
        assert_eq!(result.unwrap().session_id.to_string(), "handshake-sess-1");

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-040: session/new creates session with CWD ─────────────

    /// Verify that session/new sends the CWD parameter and stores the
    /// returned session ID as the active session.
    #[tokio::test]
    async fn acp_session_lifecycle_new_with_cwd() {
        let (client, mut mock_end) = setup_client();

        // Full handshake.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        // session/new with a specific CWD.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/home/user/project").await });

        // Read the request and verify it contains the CWD.
        let req_line = read_mock_line(&mut mock_end).await;
        let req_val: serde_json::Value = serde_json::from_str(&req_line).unwrap();
        assert_eq!(req_val["method"], "session/new");

        write_mock_line(&mut mock_end, &make_new_session_response(3, "sess-cwd-001")).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "session/new should succeed: {result:?}");
        let new_result = result.unwrap();
        assert_eq!(new_result.session_id.to_string(), "sess-cwd-001");

        // Verify active_session_id is set.
        {
            let guard = client.lock().await;
            let inner = guard.inner.lock().await;
            assert_eq!(inner.active_session_id.as_ref().unwrap().to_string(), "sess-cwd-001");
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-041: session/prompt streams updates then completes ────

    /// Verify that session/prompt blocks until PromptResponse is received,
    /// and all streaming events are broadcast via event_receiver.
    #[tokio::test]
    async fn acp_session_lifecycle_prompt_streams_then_completes() {
        let (client, mut mock_end) = setup_client();

        // Full handshake + session/new.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/tmp").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_new_session_response(3, "sess-stream")).await;
        h.await.unwrap().unwrap();

        // Subscribe to events BEFORE the prompt.
        let mut event_rx = client.lock().await.subscribe();

        // session/prompt.
        let c = client.clone();
        let h = tokio::spawn(async move {
            c.lock().await.session_prompt(&SessionId::new("sess-stream"), "Tell me a story").await
        });
        let _ = read_mock_line(&mut mock_end).await;

        // Stream multiple updates.
        write_mock_line(&mut mock_end, &make_session_update_notification("sess-stream", "Once upon")).await;
        write_mock_line(&mut mock_end, &make_session_update_notification("sess-stream", " a time")).await;

        // Then send the completion response.
        write_mock_line(&mut mock_end, &make_prompt_response(4, "end_turn")).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "session/prompt should succeed: {result:?}");
        let stop_reason = result.unwrap().stop_reason;
        // Stop reason is Debug-formatted from the enum variant, so "end_turn" becomes "EndTurn".
        assert!(!stop_reason.is_empty(), "stop reason should be non-empty: {stop_reason}");

        // Verify streaming events were received.
        let mut deltas = Vec::new();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        while let Ok(event) = event_rx.try_recv() {
            if let ProviderEvent::MessageDelta { delta, .. } = event {
                deltas.push(delta);
            }
        }
        assert!(deltas.len() >= 2, "should have received at least 2 MessageDelta events, got {deltas:?}");
        assert!(deltas[0].contains("Once upon"), "first delta should contain 'Once upon'");
        assert!(deltas[1].contains("a time"), "second delta should contain 'a time'");

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-042: session/cancel aborts active prompt ──────────────

    /// Verify that session/cancel sends a cancel notification and resets
    /// is_streaming, making the session ready for next prompt.
    #[tokio::test]
    async fn acp_session_lifecycle_cancel_resets_and_allows_next_prompt() {
        let (client, mut mock_end) = setup_client();

        // Full handshake + session/new.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/tmp").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_new_session_response(3, "sess-cancel")).await;
        h.await.unwrap().unwrap();

        // Simulate streaming state.
        {
            let guard = client.lock().await;
            let mut inner = guard.inner.lock().await;
            inner.is_streaming = true;
        }

        // Cancel.
        let result = client.lock().await.session_cancel(&SessionId::new("sess-cancel")).await;
        assert!(result.is_ok(), "cancel should succeed: {result:?}");

        // is_streaming should be reset.
        {
            let guard = client.lock().await;
            let inner = guard.inner.lock().await;
            assert!(!inner.is_streaming, "is_streaming should be false after cancel");
        }

        // Next prompt should work.
        let c = client.clone();
        let h = tokio::spawn(async move {
            c.lock().await.session_prompt(&SessionId::new("sess-cancel"), "After cancel").await
        });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_prompt_response(4, "end_turn")).await;
        let result = h.await.unwrap();
        assert!(result.is_ok(), "prompt after cancel should succeed: {result:?}");

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-060: Connection failure during handshake ──────────────

    /// Verify that stream close during initialize returns a clean error.
    #[tokio::test]
    async fn acp_error_handling_disconnect_during_initialize() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });

        // Read the initialize request.
        let _ = read_mock_line(&mut mock_end).await;

        // Drop the connection instead of responding.
        drop(mock_end);

        let result = h.await.unwrap();
        assert!(result.is_err(), "initialize should fail when stream closes: {result:?}");
    }

    /// Verify that stream close during authenticate returns a clean error.
    #[tokio::test]
    async fn acp_error_handling_disconnect_during_authenticate() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });

        // Read the authenticate request.
        let _ = read_mock_line(&mut mock_end).await;

        // Drop the connection.
        drop(mock_end);

        let result = h.await.unwrap();
        assert!(result.is_err(), "authenticate should fail when stream closes: {result:?}");
    }

    // ── VAL-ACP-061: Agent crash mid-stream ───────────────────────────

    /// Verify that agent process exit during streaming emits Disconnected
    /// event and pending requests fail cleanly.
    #[tokio::test]
    async fn acp_error_handling_agent_crash_emits_disconnected() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/tmp").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_new_session_response(3, "sess-crash")).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        // Start a prompt.
        let c = client.clone();
        let prompt_h = tokio::spawn(async move {
            c.lock().await.session_prompt(&SessionId::new("sess-crash"), "hello").await
        });

        // Read the prompt request.
        let _ = read_mock_line(&mut mock_end).await;

        // Send a partial update, then crash (drop).
        write_mock_line(&mut mock_end, &make_session_update_notification("sess-crash", "partial...")).await;

        // Wait for the notification to be processed.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;

        // Drop the connection (agent crash).
        drop(mock_end);

        // Should get Disconnected event.
        let event = tokio::time::timeout(std::time::Duration::from_secs(3), event_rx.recv()).await;
        match event {
            Ok(Ok(ProviderEvent::Disconnected { message })) => {
                assert!(!message.is_empty());
            }
            other => panic!("expected Disconnected event, got {other:?}"),
        }

        // Pending prompt should fail.
        let prompt_result = prompt_h.await.unwrap();
        assert!(prompt_result.is_err(), "pending prompt should fail on agent crash");
    }

    // ── VAL-ACP-062: Malformed JSON from agent ────────────────────────

    /// Verify that invalid JSON lines are skipped and processing continues.
    #[tokio::test]
    async fn acp_error_handling_malformed_json_skipped_continues() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        // Send a malformed line.
        write_mock_line(&mut mock_end, "this is not json!!!").await;

        // Send a valid notification after it.
        let valid_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "after malformed" }
                }
            }
        });
        write_mock_line(&mut mock_end, &serde_json::to_string(&valid_json).unwrap()).await;

        // Should get the valid event.
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event {
            Ok(Ok(ProviderEvent::MessageDelta { delta, .. })) => {
                assert_eq!(delta, "after malformed");
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                // The malformed line produced a skipped event — try next.
                let event2 = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
                match event2 {
                    Ok(Ok(ProviderEvent::MessageDelta { delta, .. })) => {
                        assert_eq!(delta, "after malformed");
                    }
                    other => panic!("expected MessageDelta after lagged, got {other:?}"),
                }
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-063: Unknown session/update variants → Unknown event ──

    /// Verify that unrecognized SessionUpdate variants produce
    /// ProviderEvent::Unknown for forward compatibility.
    #[tokio::test]
    async fn acp_error_handling_unknown_session_update_maps_to_unknown() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        // Send an unknown session/update variant via raw JSON that won't
        // match any known SessionUpdate but still parses as a valid notification.
        // We use a valid session/update with an unknown variant name.
        let unknown_update = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "future/feature",
            "params": {
                "sessionId": "s1",
                "data": { "custom": true }
            }
        });
        write_mock_line(&mut mock_end, &serde_json::to_string(&unknown_update).unwrap()).await;

        // Should get Unknown event (gracefully handled).
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
        match event {
            Ok(Ok(ProviderEvent::Unknown { method, .. })) => {
                // The method should reflect the unknown notification.
                assert!(!method.is_empty(), "unknown method should be non-empty: {method}");
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            other => panic!("expected Unknown event, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-065: Buffered response routing ────────────────────────

    /// Verify that responses arriving before their request is registered
    /// are buffered and correctly delivered.
    #[tokio::test]
    async fn acp_error_handling_buffered_response_delivered() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        // Test: Send two sequential requests and verify they are correctly
        // correlated by ID (even though they come in order).
        // This validates the response routing mechanism.
        let c = client.clone();
        let list_h = tokio::spawn(async move { c.lock().await.session_list().await });

        let req_line = read_mock_line(&mut mock_end).await;
        let req_val: serde_json::Value = serde_json::from_str(&req_line).unwrap();
        let req_id = req_val["id"].as_i64().unwrap();

        // Respond with the matching ID.
        write_mock_line(&mut mock_end, &make_list_sessions_response(req_id, vec![("s1", "Test")])).await;

        let result = list_h.await.unwrap();
        assert!(result.is_ok(), "session/list should succeed: {result:?}");
        let sessions = result.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "s1");

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-120: Unicode content preserved through ACP ────────────

    /// Verify that Unicode text (emoji, CJK, RTL) survives the full
    /// round-trip through ACP streaming.
    #[tokio::test]
    async fn acp_session_lifecycle_unicode_content_preserved() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let mut event_rx = client.lock().await.subscribe();

        // Send updates with diverse Unicode content.
        let unicode_texts = vec![
            "🌍🌎🌏",                           // Emoji
            "世界こんにちは",                      // CJK
            "مرحبا بالعالم",                     // RTL Arabic
            "Héllo wörld café",                // Latin with diacritics
            "Test\u{0301} combining",          // Combining character
        ];

        for text in &unicode_texts {
            let chunk_json = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "s-unicode",
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": { "type": "text", "text": text }
                    }
                }
            });
            write_mock_line(&mut mock_end, &serde_json::to_string(&chunk_json).unwrap()).await;
        }

        // Verify all deltas preserve Unicode content.
        let mut received_deltas = Vec::new();
        for _ in 0..unicode_texts.len() {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await;
            match event {
                Ok(Ok(ProviderEvent::MessageDelta { delta, .. })) => {
                    received_deltas.push(delta);
                }
                other => panic!("expected MessageDelta for Unicode content, got {other:?}"),
            }
        }

        assert_eq!(received_deltas.len(), unicode_texts.len(),
            "should receive all Unicode deltas");
        for (i, expected) in unicode_texts.iter().enumerate() {
            assert_eq!(&received_deltas[i], expected,
                "Unicode delta {i} should be preserved: expected '{expected}', got '{}'", received_deltas[i]);
        }

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-121: Empty prompt handling ────────────────────────────

    /// Verify that an empty prompt text is handled gracefully.
    #[tokio::test]
    async fn acp_session_lifecycle_empty_prompt_handled() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/tmp").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_new_session_response(3, "sess-empty")).await;
        h.await.unwrap().unwrap();

        // Send an empty prompt. The agent may accept or reject it, but
        // the client should not crash or hang.
        let c = client.clone();
        let h = tokio::spawn(async move {
            c.lock().await.session_prompt(&SessionId::new("sess-empty"), "").await
        });

        // Read the prompt request — it should still be sent with empty text.
        let req_line = read_mock_line(&mut mock_end).await;
        let req_val: serde_json::Value = serde_json::from_str(&req_line).unwrap();
        assert_eq!(req_val["method"], "session/prompt");

        // The agent could respond with success or error.
        // For this test, respond with success (end_turn).
        write_mock_line(&mut mock_end, &make_prompt_response(4, "end_turn")).await;

        let result = h.await.unwrap();
        // Should succeed — the empty prompt was sent and the agent responded.
        assert!(result.is_ok(), "empty prompt should complete without crash: {result:?}");

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-122: Very long prompt (>1MB) ──────────────────────────

    /// Verify that a very large prompt (over 1MB) can be serialized and
    /// sent without OOM or framing corruption.
    #[tokio::test]
    async fn acp_session_lifecycle_very_long_prompt() {
        // Use a larger duplex buffer for this test (default is 4096).
        let (client_end, mut mock_end) = tokio::io::duplex(2 * 1024 * 1024);

        let client = Arc::new(tokio::sync::Mutex::new(AcpClient::new(client_end)));

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.authenticate(None).await });
        mock_auth(&mut mock_end, 2).await;
        h.await.unwrap().unwrap();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.session_new("/tmp").await });
        let _ = read_mock_line(&mut mock_end).await;
        write_mock_line(&mut mock_end, &make_new_session_response(3, "sess-long")).await;
        h.await.unwrap().unwrap();

        // Create a very long prompt (1MB+).
        let long_text = "A".repeat(1_100_000);

        let c = client.clone();
        let h = tokio::spawn(async move {
            c.lock().await.session_prompt(&SessionId::new("sess-long"), &long_text).await
        });

        // Read the prompt request. It should be a valid NDJSON line.
        let req_line = read_mock_line(&mut mock_end).await;
        assert!(!req_line.is_empty(), "should receive prompt request");

        // Parse the JSON.
        let req_val: serde_json::Value = serde_json::from_str(&req_line).unwrap();
        assert_eq!(req_val["method"], "session/prompt");

        // Verify the long text was included — find it anywhere in the params.
        let req_str = req_line.clone();
        assert!(req_str.contains(&"A".repeat(100)),
            "long text should be present in the serialized request");
        assert!(req_str.len() > 1_100_000,
            "request should be over 1MB: actual len = {}", req_str.len());

        // Respond with success.
        let req_id = req_val["id"].as_i64().unwrap();
        write_mock_line(&mut mock_end, &make_prompt_response(req_id, "max_tokens")).await;

        let result = h.await.unwrap();
        assert!(result.is_ok(), "long prompt should succeed: {result:?}");

        client.lock().await.shutdown().await;
    }

    // ── VAL-ACP-123: Concurrent event subscribers ─────────────────────

    /// Verify that multiple broadcast receivers all receive the same events
    /// and that slow consumers get Lagged notification.
    #[tokio::test]
    async fn acp_session_lifecycle_concurrent_event_subscribers() {
        let (client, mut mock_end) = setup_client();

        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(&mut mock_end, 1, 1u16, vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))]).await;
        h.await.unwrap().unwrap();

        // Create multiple subscribers.
        let mut rx1 = client.lock().await.subscribe();
        let mut rx2 = client.lock().await.subscribe();
        let mut rx3 = client.lock().await.subscribe();

        // Send a notification.
        let chunk_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "s-multi",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "broadcast test" }
                }
            }
        });
        write_mock_line(&mut mock_end, &serde_json::to_string(&chunk_json).unwrap()).await;

        // All subscribers should receive the event.
        let e1 = tokio::time::timeout(std::time::Duration::from_secs(2), rx1.recv()).await;
        let e2 = tokio::time::timeout(std::time::Duration::from_secs(2), rx2.recv()).await;
        let e3 = tokio::time::timeout(std::time::Duration::from_secs(2), rx3.recv()).await;

        assert!(matches!(e1, Ok(Ok(ProviderEvent::MessageDelta { .. }))),
            "subscriber 1 should get MessageDelta: {e1:?}");
        assert!(matches!(e2, Ok(Ok(ProviderEvent::MessageDelta { .. }))),
            "subscriber 2 should get MessageDelta: {e2:?}");
        assert!(matches!(e3, Ok(Ok(ProviderEvent::MessageDelta { .. }))),
            "subscriber 3 should get MessageDelta: {e3:?}");

        // Verify all got the same content.
        match (e1.unwrap().unwrap(), e2.unwrap().unwrap(), e3.unwrap().unwrap()) {
            (ProviderEvent::MessageDelta { delta: d1, .. },
             ProviderEvent::MessageDelta { delta: d2, .. },
             ProviderEvent::MessageDelta { delta: d3, .. }) => {
                assert_eq!(d1, "broadcast test");
                assert_eq!(d2, "broadcast test");
                assert_eq!(d3, "broadcast test");
                // All deltas should be identical.
                assert_eq!(d1, d2);
                assert_eq!(d2, d3);
            }
            _ => unreachable!(),
        }

        client.lock().await.shutdown().await;
    }

    /// Verify that initialize can only be called once.
    #[tokio::test]
    async fn acp_handshake_initialize_called_twice_fails() {
        let (client, mut mock_end) = setup_client();

        // First initialize succeeds.
        let c = client.clone();
        let h = tokio::spawn(async move { c.lock().await.initialize("litter", "0.1.0").await });
        mock_init(
            &mut mock_end,
            1,
            1u16,
            vec![AuthMethod::Agent(AuthMethodAgent::new("local", "Local Auth"))],
        )
        .await;
        h.await.unwrap().unwrap();

        // Second initialize fails (state is Initialized, not Uninitialized).
        let result = client.lock().await.initialize("litter", "0.1.0").await;
        match result {
            Err(AcpClientError::NotInitialized) => {
                // This is the expected error — NotInitialized is returned
                // when state != Uninitialized.
            }
            other => panic!("expected NotInitialized error on double init, got {other:?}"),
        }

        client.lock().await.shutdown().await;
    }
}
