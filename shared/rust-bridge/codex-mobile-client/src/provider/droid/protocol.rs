//! Droid Factory API protocol types and JSON-RPC 2.0 framing.
//!
//! Defines the JSON-RPC message types for Droid's native Factory API
//! (`droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc`).
//!
//! # Protocol Overview
//!
//! The Droid native transport uses JSON-RPC 2.0 over stdin/stdout.
//!
//! ## Client → Droid (requests)
//! - `droid.initialize_session` — Start a session, returns session ID + model info
//! - `droid.add_user_message` — Send a user prompt, Droid streams notifications back
//! - `droid.cancel` — Cancel the active request
//! - `droid.set_model` — Change the model
//! - `droid.set_autonomy_level` — Set autonomy (suggest/normal/full)
//! - `droid.set_reasoning_effort` — Set reasoning effort (low/medium/high)
//! - `droid.fork_session` — Fork the current session
//! - `droid.resume_session` — Resume a previous session
//! - `droid.list_sessions` — List available sessions
//!
//! ## Droid → Client (notifications)
//! - `droid_working_state_changed` — Agent state (idle/streaming)
//! - `create_message` — Streaming assistant text deltas
//! - `tool_result` — Tool call result with name, input, output
//! - `error` — Error notification
//! - `complete` — Prompt processing completed

use std::fmt;

use serde::{Deserialize, Serialize};

// ── Autonomy Level ─────────────────────────────────────────────────────────

/// Droid autonomy levels.
///
/// Maps to approval policy:
/// - `Suggest` → high-approval (prompt always)
/// - `Normal` → default approval
/// - `Full` → auto-approve all
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DroidAutonomyLevel {
    Suggest,
    Normal,
    Full,
}

impl DroidAutonomyLevel {
    /// All autonomy level variants.
    pub fn all() -> &'static [DroidAutonomyLevel] {
        &[DroidAutonomyLevel::Suggest, DroidAutonomyLevel::Normal, DroidAutonomyLevel::Full]
    }
}

impl fmt::Display for DroidAutonomyLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Suggest => write!(f, "suggest"),
            Self::Normal => write!(f, "normal"),
            Self::Full => write!(f, "full"),
        }
    }
}

// ── Reasoning Effort ───────────────────────────────────────────────────────

/// Droid reasoning effort levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DroidReasoningEffort {
    Low,
    Medium,
    High,
}

impl DroidReasoningEffort {
    /// All reasoning effort variants.
    pub fn all() -> &'static [DroidReasoningEffort] {
        &[DroidReasoningEffort::Low, DroidReasoningEffort::Medium, DroidReasoningEffort::High]
    }
}

impl fmt::Display for DroidReasoningEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
        }
    }
}

// ── Working State ──────────────────────────────────────────────────────────

/// Droid working states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DroidWorkingState {
    Idle,
    Streaming,
}

impl fmt::Display for DroidWorkingState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Streaming => write!(f, "streaming"),
        }
    }
}

// ── JSON-RPC 2.0 Types ─────────────────────────────────────────────────────

/// A JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: i64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcRequest {
    /// Create a new JSON-RPC request.
    pub fn new(id: i64, method: &str, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 response (success or error).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// Check if this is a success response.
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    /// Extract the result value, or return an error.
    pub fn into_result(self) -> Result<serde_json::Value, JsonRpcError> {
        if let Some(error) = self.error {
            Err(error)
        } else {
            Ok(self.result.unwrap_or(serde_json::Value::Null))
        }
    }
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// A JSON-RPC 2.0 notification (no ID, no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Parsed incoming JSON-RPC message from Droid.
#[derive(Debug, Clone)]
pub enum DroidIncoming {
    /// A response to a previously sent request (has an ID).
    Response(JsonRpcResponse),
    /// A notification from Droid (no ID).
    Notification(JsonRpcNotification),
}

// ── Droid Notification Payloads ─────────────────────────────────────────────

/// Payload for `droid_working_state_changed` notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingStateChangedPayload {
    pub state: DroidWorkingState,
}

/// Payload for `create_message` notification (streaming text delta).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMessagePayload {
    /// The message ID for this assistant message.
    #[serde(default)]
    pub message_id: Option<String>,
    /// Incremental text content.
    #[serde(default)]
    pub text_delta: Option<String>,
    /// Whether this is the final delta for this message.
    #[serde(default)]
    pub complete: Option<bool>,
}

/// Payload for `tool_result` notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultPayload {
    /// Tool call identifier.
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// Tool name.
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Tool input/command.
    #[serde(default)]
    pub input: Option<String>,
    /// Tool output.
    #[serde(default)]
    pub output: Option<String>,
    /// Tool execution status (running, completed, failed).
    #[serde(default)]
    pub status: Option<String>,
}

/// Payload for `error` notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    /// Error message.
    #[serde(default)]
    pub message: Option<String>,
    /// Error code.
    #[serde(default)]
    pub code: Option<i64>,
    /// Whether the error is fatal (session terminated).
    #[serde(default)]
    pub fatal: Option<bool>,
}

/// Payload for `complete` notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletePayload {
    /// Optional stop reason.
    #[serde(default)]
    pub reason: Option<String>,
}

// ── Droid Request Params ────────────────────────────────────────────────────

/// Parameters for `droid.initialize_session`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeSessionParams {
    /// Model to use (e.g. "claude-sonnet-4-20250514").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Autonomy level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autonomy_level: Option<DroidAutonomyLevel>,
    /// Reasoning effort.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<DroidReasoningEffort>,
    /// Session ID to resume (for resume).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Working directory for the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
}

/// Response from `droid.initialize_session`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeSessionResult {
    /// The session ID.
    pub session_id: String,
    /// Model info.
    #[serde(default)]
    pub model: Option<ModelInfo>,
    /// Available models.
    #[serde(default)]
    pub available_models: Option<Vec<ModelInfo>>,
}

/// Model information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Model identifier (e.g. "claude-sonnet-4-20250514").
    #[serde(default)]
    pub id: Option<String>,
    /// Human-readable model name.
    #[serde(default)]
    pub name: Option<String>,
}

/// Parameters for `droid.add_user_message`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddUserMessageParams {
    /// The user prompt text.
    pub content: String,
}

/// Parameters for `droid.fork_session`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkSessionParams {
    /// Session ID to fork.
    pub session_id: String,
}

/// Result from `droid.fork_session`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkSessionResult {
    /// The new forked session ID.
    pub session_id: String,
}

/// Parameters for `droid.resume_session`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeSessionParams {
    /// Session ID to resume.
    pub session_id: String,
}

/// Parameters for `droid.set_model`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetModelParams {
    /// Model to switch to.
    pub model: String,
}

/// Parameters for `droid.set_autonomy_level`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetAutonomyLevelParams {
    /// New autonomy level.
    pub level: DroidAutonomyLevel,
}

/// Parameters for `droid.set_reasoning_effort`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetReasoningEffortParams {
    /// New reasoning effort.
    pub effort: DroidReasoningEffort,
}

// ── JSONL Framing ───────────────────────────────────────────────────────────

/// Maximum line length for JSON-RPC messages (256 KiB).
pub const MAX_LINE_LENGTH: usize = 256 * 1024;

/// Serialize a JSON-RPC request to a JSONL line (with trailing newline).
pub fn serialize_request(req: &JsonRpcRequest) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    Ok(line)
}

/// Parse an incoming JSON-RPC message from a line.
///
/// Returns `Ok(Some(DroidIncoming))` for valid messages,
/// `Ok(None)` for empty lines, and `Err` for invalid JSON.
pub fn parse_incoming(line: &str) -> Result<Option<DroidIncoming>, serde_json::Error> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    // First try to parse as a generic JSON value to determine the type.
    let value: serde_json::Value = serde_json::from_str(trimmed)?;

    // If it has an "id" field, it's a response. If it has a "method" field
    // but no "id", it's a notification.
    if value.get("id").is_some() {
        // Response (or error response).
        let response: JsonRpcResponse = serde_json::from_value(value)?;
        Ok(Some(DroidIncoming::Response(response)))
    } else if value.get("method").is_some() {
        // Notification.
        let notification: JsonRpcNotification = serde_json::from_value(value)?;
        Ok(Some(DroidIncoming::Notification(notification)))
    } else {
        // Unknown shape — treat as error.
        Err(serde::de::Error::custom(
            "JSON-RPC message must have 'id' (response) or 'method' (notification)",
        ))
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Autonomy Level ─────────────────────────────────────────────────

    #[test]
    fn droid_autonomy_level_all_variants() {
        assert_eq!(DroidAutonomyLevel::all().len(), 3);
    }

    #[test]
    fn droid_autonomy_level_display() {
        assert_eq!(DroidAutonomyLevel::Suggest.to_string(), "suggest");
        assert_eq!(DroidAutonomyLevel::Normal.to_string(), "normal");
        assert_eq!(DroidAutonomyLevel::Full.to_string(), "full");
    }

    #[test]
    fn droid_autonomy_level_serde_roundtrip() {
        for level in DroidAutonomyLevel::all() {
            let json = serde_json::to_string(level).unwrap();
            let back: DroidAutonomyLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(*level, back);
        }
    }

    // ── Reasoning Effort ──────────────────────────────────────────────

    #[test]
    fn droid_reasoning_effort_all_variants() {
        assert_eq!(DroidReasoningEffort::all().len(), 3);
    }

    #[test]
    fn droid_reasoning_effort_display() {
        assert_eq!(DroidReasoningEffort::Low.to_string(), "low");
        assert_eq!(DroidReasoningEffort::Medium.to_string(), "medium");
        assert_eq!(DroidReasoningEffort::High.to_string(), "high");
    }

    // ── Working State ──────────────────────────────────────────────────

    #[test]
    fn droid_working_state_serde() {
        assert_eq!(
            serde_json::to_string(&DroidWorkingState::Idle).unwrap(),
            "\"idle\""
        );
        assert_eq!(
            serde_json::to_string(&DroidWorkingState::Streaming).unwrap(),
            "\"streaming\""
        );
    }

    // ── JSON-RPC Request Serialization ─────────────────────────────────

    #[test]
    fn jsonrpc_request_initialize_session() {
        let req = JsonRpcRequest::new(
            1,
            "droid.initialize_session",
            Some(serde_json::json!({
                "model": "claude-sonnet-4-20250514",
                "autonomyLevel": "normal",
            })),
        );
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"method\":\"droid.initialize_session\""));
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"id\":1"));
    }

    #[test]
    fn jsonrpc_request_add_user_message() {
        let req = JsonRpcRequest::new(
            2,
            "droid.add_user_message",
            Some(serde_json::json!({"content": "What is 2+2?"})),
        );
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"method\":\"droid.add_user_message\""));
        assert!(json.contains("What is 2+2?"));
    }

    // ── JSON-RPC Response Parsing ──────────────────────────────────────

    #[test]
    fn jsonrpc_response_success() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"sess-123","model":{"id":"claude-sonnet-4-20250514"}}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.is_success());
        assert_eq!(resp.id, Some(1));
        let result = resp.into_result().unwrap();
        assert_eq!(result["sessionId"], "sess-123");
    }

    #[test]
    fn jsonrpc_response_error() {
        let json = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid request"}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.is_success());
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32600);
        assert_eq!(err.message, "Invalid request");
    }

    // ── JSON-RPC Notification Parsing ──────────────────────────────────

    #[test]
    fn jsonrpc_notification_working_state() {
        let json = r#"{"jsonrpc":"2.0","method":"droid_working_state_changed","params":{"state":"streaming"}}"#;
        let notif: JsonRpcNotification = serde_json::from_str(json).unwrap();
        assert_eq!(notif.method, "droid_working_state_changed");
        assert_eq!(notif.params["state"], "streaming");
    }

    #[test]
    fn jsonrpc_notification_create_message() {
        let json = r#"{"jsonrpc":"2.0","method":"create_message","params":{"message_id":"msg-1","text_delta":"Hello"}}"#;
        let notif: JsonRpcNotification = serde_json::from_str(json).unwrap();
        assert_eq!(notif.method, "create_message");
        assert_eq!(notif.params["text_delta"], "Hello");
    }

    #[test]
    fn jsonrpc_notification_tool_result() {
        let json = r#"{"jsonrpc":"2.0","method":"tool_result","params":{"tool_name":"read_file","output":"file contents"}}"#;
        let notif: JsonRpcNotification = serde_json::from_str(json).unwrap();
        assert_eq!(notif.method, "tool_result");
        assert_eq!(notif.params["tool_name"], "read_file");
    }

    #[test]
    fn jsonrpc_notification_error() {
        let json = r#"{"jsonrpc":"2.0","method":"error","params":{"message":"Rate limited","code":429}}"#;
        let notif: JsonRpcNotification = serde_json::from_str(json).unwrap();
        assert_eq!(notif.method, "error");
        assert_eq!(notif.params["message"], "Rate limited");
    }

    #[test]
    fn jsonrpc_notification_complete() {
        let json = r#"{"jsonrpc":"2.0","method":"complete","params":{"reason":"done"}}"#;
        let notif: JsonRpcNotification = serde_json::from_str(json).unwrap();
        assert_eq!(notif.method, "complete");
    }

    // ── Parse Incoming (determine response vs notification) ────────────

    #[test]
    fn parse_incoming_response() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"sess-1"}}"#;
        let incoming = parse_incoming(json).unwrap().unwrap();
        match incoming {
            DroidIncoming::Response(resp) => {
                assert_eq!(resp.id, Some(1));
                assert!(resp.is_success());
            }
            DroidIncoming::Notification(_) => panic!("expected response"),
        }
    }

    #[test]
    fn parse_incoming_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"create_message","params":{"text_delta":"hi"}}"#;
        let incoming = parse_incoming(json).unwrap().unwrap();
        match incoming {
            DroidIncoming::Notification(notif) => {
                assert_eq!(notif.method, "create_message");
            }
            DroidIncoming::Response(_) => panic!("expected notification"),
        }
    }

    #[test]
    fn parse_incoming_empty_line() {
        assert!(parse_incoming("").unwrap().is_none());
        assert!(parse_incoming("  ").unwrap().is_none());
    }

    #[test]
    fn parse_incoming_invalid_json() {
        assert!(parse_incoming("{broken").is_err());
    }

    // ── Serialize Request to JSONL ─────────────────────────────────────

    #[test]
    fn serialize_request_adds_newline() {
        let req = JsonRpcRequest::new(1, "test", None);
        let line = serialize_request(&req).unwrap();
        assert!(line.ends_with('\n'));
    }

    // ── Notification Payload Deserialization ───────────────────────────

    #[test]
    fn working_state_changed_payload() {
        let payload: WorkingStateChangedPayload =
            serde_json::from_str(r#"{"state":"streaming"}"#).unwrap();
        assert_eq!(payload.state, DroidWorkingState::Streaming);
    }

    #[test]
    fn create_message_payload() {
        let payload: CreateMessagePayload =
            serde_json::from_str(r#"{"message_id":"msg-1","text_delta":"Hello","complete":false}"#)
                .unwrap();
        assert_eq!(payload.message_id, Some("msg-1".to_string()));
        assert_eq!(payload.text_delta, Some("Hello".to_string()));
        assert_eq!(payload.complete, Some(false));
    }

    #[test]
    fn create_message_payload_minimal() {
        let payload: CreateMessagePayload = serde_json::from_str(r#"{}"#).unwrap();
        assert!(payload.message_id.is_none());
        assert!(payload.text_delta.is_none());
    }

    #[test]
    fn tool_result_payload() {
        let payload: ToolResultPayload = serde_json::from_str(
            r#"{"tool_call_id":"tc-1","tool_name":"read_file","input":"/tmp/test","output":"contents","status":"completed"}"#,
        )
        .unwrap();
        assert_eq!(payload.tool_call_id, Some("tc-1".to_string()));
        assert_eq!(payload.tool_name, Some("read_file".to_string()));
        assert_eq!(payload.status, Some("completed".to_string()));
    }

    #[test]
    fn error_payload() {
        let payload: ErrorPayload =
            serde_json::from_str(r#"{"message":"Rate limited","code":429,"fatal":false}"#).unwrap();
        assert_eq!(payload.message, Some("Rate limited".to_string()));
        assert_eq!(payload.code, Some(429));
        assert_eq!(payload.fatal, Some(false));
    }

    #[test]
    fn error_payload_fatal() {
        let payload: ErrorPayload =
            serde_json::from_str(r#"{"message":"Auth failed","code":401,"fatal":true}"#).unwrap();
        assert_eq!(payload.fatal, Some(true));
    }

    #[test]
    fn complete_payload() {
        let payload: CompletePayload =
            serde_json::from_str(r#"{"reason":"done"}"#).unwrap();
        assert_eq!(payload.reason, Some("done".to_string()));
    }

    // ── Initialize Session Types ────────────────────────────────────────

    #[test]
    fn initialize_session_params_serialization() {
        let params = InitializeSessionParams {
            model: Some("claude-sonnet-4-20250514".to_string()),
            autonomy_level: Some(DroidAutonomyLevel::Normal),
            reasoning_effort: Some(DroidReasoningEffort::High),
            session_id: None,
            working_directory: Some("/home/user/project".to_string()),
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"autonomyLevel\":\"normal\""));
        assert!(json.contains("\"reasoningEffort\":\"high\""));
        assert!(!json.contains("sessionId")); // skip_serializing_if
    }

    #[test]
    fn initialize_session_result_deserialization() {
        let json = r#"{"sessionId":"sess-abc","model":{"id":"claude-sonnet-4-20250514"},"availableModels":[{"id":"claude-sonnet-4-20250514"},{"id":"gpt-4.1"}]}"#;
        let result: InitializeSessionResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.session_id, "sess-abc");
        assert!(result.model.is_some());
        assert_eq!(result.available_models.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn initialize_session_result_minimal() {
        let json = r#"{"sessionId":"sess-min"}"#;
        let result: InitializeSessionResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.session_id, "sess-min");
        assert!(result.model.is_none());
        assert!(result.available_models.is_none());
    }

    // ── Fork / Resume Params ────────────────────────────────────────────

    #[test]
    fn fork_session_params() {
        let params = ForkSessionParams {
            session_id: "sess-1".to_string(),
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"session_id\":\"sess-1\""));
    }

    #[test]
    fn fork_session_result() {
        let json = r#"{"session_id":"sess-forked"}"#;
        let result: ForkSessionResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.session_id, "sess-forked");
    }
}
