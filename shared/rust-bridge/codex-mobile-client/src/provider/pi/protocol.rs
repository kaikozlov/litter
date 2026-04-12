//! Pi native RPC protocol types and JSONL framing.
//!
//! Defines the JSON message types for Pi's `--mode rpc` protocol.
//! Pi communicates over stdin/stdout using newline-delimited JSON (JSONL).
//!
//! # Protocol Overview
//!
//! Commands are sent as JSON objects with a `type` field:
//! - `prompt` — Send a user prompt (async; events stream back)
//! - `abort` — Cancel the active prompt
//! - `get_state` — Query current session state
//! - `set_model` — Change model for next prompt
//! - `get_available_models` — List available models
//! - `set_thinking_level` — Set thinking/reasoning level
//! - `set_steering_mode` — Set steering mode
//! - `compact` — Compact session context
//!
//! Events are received as JSON objects with a `type` field:
//! - `agent_start` — Agent begins processing
//! - `agent_end` — Agent finishes processing
//! - `turn_start` / `turn_end` — Turn boundaries
//! - `message_start` / `message_update` / `message_end` — Message streaming
//! - `thinking_delta` — Reasoning content (embedded in message_update)
//! - `text_delta` — Text content (embedded in message_update)
//! - `toolcall_start` / `toolcall_delta` / `toolcall_end` — Tool call lifecycle
//! - `tool_execution_start` / `tool_execution_update` / `tool_execution_end` — Tool execution
//!
//! # JSONL Framing
//!
//! One JSON object per line, LF (`\n`) delimiter. Empty lines are skipped.
//! Malformed lines are logged and skipped without crashing.

use std::fmt;

use serde::{Deserialize, Serialize};

// ── Thinking Level ─────────────────────────────────────────────────────────

/// Pi's thinking/reasoning levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PiThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
}

impl PiThinkingLevel {
    /// All thinking level variants in order.
    pub fn all() -> &'static [PiThinkingLevel] {
        &[
            PiThinkingLevel::Off,
            PiThinkingLevel::Minimal,
            PiThinkingLevel::Low,
            PiThinkingLevel::Medium,
            PiThinkingLevel::High,
            PiThinkingLevel::XHigh,
        ]
    }
}

impl fmt::Display for PiThinkingLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => write!(f, "off"),
            Self::Minimal => write!(f, "minimal"),
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
            Self::XHigh => write!(f, "xhigh"),
        }
    }
}

// ── Commands (client → Pi) ─────────────────────────────────────────────────

/// A Pi RPC command sent from client to the Pi process.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PiCommand {
    /// Send a user prompt. Pi will stream events back until `agent_end`.
    #[serde(rename = "prompt")]
    Prompt {
        /// The prompt text.
        text: String,
        /// Optional session ID for resuming a session.
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },

    /// Cancel the active prompt.
    #[serde(rename = "abort")]
    Abort,

    /// Query the current session state.
    #[serde(rename = "get_state")]
    GetState,

    /// Change the model for subsequent prompts.
    #[serde(rename = "set_model")]
    SetModel {
        /// Model identifier (e.g. "claude-sonnet-4-20250514").
        model: String,
    },

    /// List available models.
    #[serde(rename = "get_available_models")]
    GetAvailableModels,

    /// Set the thinking/reasoning level.
    #[serde(rename = "set_thinking_level")]
    SetThinkingLevel {
        /// The thinking level to set.
        level: PiThinkingLevel,
    },

    /// Set the steering mode.
    #[serde(rename = "set_steering_mode")]
    SetSteeringMode {
        /// The steering mode (e.g. "auto", "guided").
        mode: String,
    },

    /// Compact the session context.
    #[serde(rename = "compact")]
    Compact,
}

impl PiCommand {
    /// Returns the command type string.
    pub fn command_type(&self) -> &'static str {
        match self {
            PiCommand::Prompt { .. } => "prompt",
            PiCommand::Abort => "abort",
            PiCommand::GetState => "get_state",
            PiCommand::SetModel { .. } => "set_model",
            PiCommand::GetAvailableModels => "get_available_models",
            PiCommand::SetThinkingLevel { .. } => "set_thinking_level",
            PiCommand::SetSteeringMode { .. } => "set_steering_mode",
            PiCommand::Compact => "compact",
        }
    }
}

// ── Events (Pi → client) ───────────────────────────────────────────────────

/// A Pi event received from the Pi process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PiEvent {
    /// Agent started processing a prompt.
    #[serde(rename = "agent_start")]
    AgentStart {
        /// Optional session identifier.
        #[serde(default)]
        session_id: Option<String>,
    },

    /// Agent finished processing.
    #[serde(rename = "agent_end")]
    AgentEnd {
        /// Optional stop reason.
        #[serde(default)]
        reason: Option<String>,
    },

    /// A new turn started.
    #[serde(rename = "turn_start")]
    TurnStart {
        /// Turn identifier.
        #[serde(default)]
        turn_id: Option<String>,
    },

    /// Turn ended.
    #[serde(rename = "turn_end")]
    TurnEnd {
        /// Turn identifier.
        #[serde(default)]
        turn_id: Option<String>,
    },

    /// A new message started (assistant or tool response).
    #[serde(rename = "message_start")]
    MessageStart {
        /// Message identifier.
        #[serde(default)]
        message_id: Option<String>,
        /// Role of the message sender (e.g. "assistant").
        #[serde(default)]
        role: Option<String>,
    },

    /// Message content update with streaming deltas.
    #[serde(rename = "message_update")]
    MessageUpdate {
        /// Message identifier.
        #[serde(default)]
        message_id: Option<String>,
        /// Text content delta.
        #[serde(default)]
        text_delta: Option<String>,
        /// Thinking/reasoning content delta.
        #[serde(default)]
        thinking_delta: Option<String>,
    },

    /// Message completed.
    #[serde(rename = "message_end")]
    MessageEnd {
        /// Message identifier.
        #[serde(default)]
        message_id: Option<String>,
    },

    /// Tool call started.
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        /// Tool call identifier.
        #[serde(default)]
        call_id: Option<String>,
        /// Tool name.
        #[serde(default)]
        tool_name: Option<String>,
        /// Tool call input/arguments.
        #[serde(default)]
        input: Option<String>,
    },

    /// Tool call content delta.
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        /// Tool call identifier.
        #[serde(default)]
        call_id: Option<String>,
        /// Content delta.
        #[serde(default)]
        content_delta: Option<String>,
    },

    /// Tool call completed.
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        /// Tool call identifier.
        #[serde(default)]
        call_id: Option<String>,
        /// Tool call output.
        #[serde(default)]
        output: Option<String>,
    },

    /// Tool execution started (shell command running).
    #[serde(rename = "tool_execution_start")]
    ToolExecutionStart {
        /// Tool execution identifier.
        #[serde(default)]
        execution_id: Option<String>,
        /// Command being executed.
        #[serde(default)]
        command: Option<String>,
        /// Working directory.
        #[serde(default)]
        cwd: Option<String>,
    },

    /// Tool execution output update.
    #[serde(rename = "tool_execution_update")]
    ToolExecutionUpdate {
        /// Tool execution identifier.
        #[serde(default)]
        execution_id: Option<String>,
        /// Standard output delta.
        #[serde(default)]
        stdout: Option<String>,
        /// Standard error delta.
        #[serde(default)]
        stderr: Option<String>,
    },

    /// Tool execution completed.
    #[serde(rename = "tool_execution_end")]
    ToolExecutionEnd {
        /// Tool execution identifier.
        #[serde(default)]
        execution_id: Option<String>,
        /// Exit code.
        #[serde(default)]
        exit_code: Option<i64>,
    },

    /// A response to a command (e.g. get_state, get_available_models).
    #[serde(rename = "response")]
    Response {
        /// The command type this responds to.
        #[serde(default)]
        command: Option<String>,
        /// Response data (varies by command).
        #[serde(default)]
        data: Option<serde_json::Value>,
        /// Error message if the command failed.
        #[serde(default)]
        error: Option<String>,
    },

    /// An error from the Pi process.
    #[serde(rename = "error")]
    Error {
        /// Error message.
        #[serde(default)]
        message: Option<String>,
        /// Error code.
        #[serde(default)]
        code: Option<i64>,
    },
}

// ── JSONL Framing ───────────────────────────────────────────────────────────

/// Maximum line length for Pi JSONL messages (256 KiB).
pub const MAX_LINE_LENGTH: usize = 256 * 1024;

/// Serialize a Pi command to a JSONL line (with trailing newline).
pub fn serialize_command(cmd: &PiCommand) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(cmd)?;
    line.push('\n');
    Ok(line)
}

/// Deserialize a Pi event from a JSONL line.
///
/// Returns `Ok(Some(event))` for valid events, `Ok(None)` for empty lines,
/// and `Err` for malformed JSON.
pub fn deserialize_event(line: &str) -> Result<Option<PiEvent>, serde_json::Error> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let event: PiEvent = serde_json::from_str(trimmed)?;
    Ok(Some(event))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Thinking Level ─────────────────────────────────────────────────

    #[test]
    fn pi_thinking_level_all_variants() {
        assert_eq!(PiThinkingLevel::all().len(), 6);
    }

    #[test]
    fn pi_thinking_level_display() {
        assert_eq!(PiThinkingLevel::Off.to_string(), "off");
        assert_eq!(PiThinkingLevel::Minimal.to_string(), "minimal");
        assert_eq!(PiThinkingLevel::Low.to_string(), "low");
        assert_eq!(PiThinkingLevel::Medium.to_string(), "medium");
        assert_eq!(PiThinkingLevel::High.to_string(), "high");
        assert_eq!(PiThinkingLevel::XHigh.to_string(), "xhigh");
    }

    #[test]
    fn pi_thinking_level_serde_roundtrip() {
        for level in PiThinkingLevel::all() {
            let json = serde_json::to_string(level).unwrap();
            let back: PiThinkingLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(*level, back);
        }
    }

    // ── Command Serialization ──────────────────────────────────────────

    #[test]
    fn pi_command_prompt_serialization() {
        let cmd = PiCommand::Prompt {
            text: "Hello, world!".to_string(),
            session_id: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"type\":\"prompt\""));
        assert!(json.contains("\"text\":\"Hello, world!\""));
        assert!(!json.contains("session_id")); // skip_serializing_if
    }

    #[test]
    fn pi_command_prompt_with_session_id() {
        let cmd = PiCommand::Prompt {
            text: "Continue".to_string(),
            session_id: Some("sess-123".to_string()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"session_id\":\"sess-123\""));
    }

    #[test]
    fn pi_command_abort_serialization() {
        let cmd = PiCommand::Abort;
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, "{\"type\":\"abort\"}");
    }

    #[test]
    fn pi_command_set_thinking_level_serialization() {
        let cmd = PiCommand::SetThinkingLevel {
            level: PiThinkingLevel::High,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"type\":\"set_thinking_level\""));
        assert!(json.contains("\"level\":\"high\""));
    }

    #[test]
    fn pi_command_set_model_serialization() {
        let cmd = PiCommand::SetModel {
            model: "claude-sonnet-4-20250514".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"model\":\"claude-sonnet-4-20250514\""));
    }

    #[test]
    fn pi_command_compact_serialization() {
        let cmd = PiCommand::Compact;
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, "{\"type\":\"compact\"}");
    }

    #[test]
    fn pi_command_type_strings() {
        assert_eq!(PiCommand::Abort.command_type(), "abort");
        assert_eq!(PiCommand::GetState.command_type(), "get_state");
        assert_eq!(
            PiCommand::GetAvailableModels.command_type(),
            "get_available_models"
        );
    }

    // ── Event Deserialization ──────────────────────────────────────────

    #[test]
    fn pi_event_agent_start() {
        let json = r#"{"type":"agent_start","session_id":"sess-1"}"#;
        let event: PiEvent = serde_json::from_str(json).unwrap();
        match event {
            PiEvent::AgentStart { session_id } => {
                assert_eq!(session_id, Some("sess-1".to_string()));
            }
            _ => panic!("expected AgentStart"),
        }
    }

    #[test]
    fn pi_event_agent_end() {
        let json = r#"{"type":"agent_end","reason":"complete"}"#;
        let event: PiEvent = serde_json::from_str(json).unwrap();
        match event {
            PiEvent::AgentEnd { reason } => {
                assert_eq!(reason, Some("complete".to_string()));
            }
            _ => panic!("expected AgentEnd"),
        }
    }

    #[test]
    fn pi_event_message_update_text_delta() {
        let json = r#"{"type":"message_update","message_id":"msg-1","text_delta":"Hello"}"#;
        let event: PiEvent = serde_json::from_str(json).unwrap();
        match event {
            PiEvent::MessageUpdate {
                message_id,
                text_delta,
                thinking_delta,
            } => {
                assert_eq!(message_id, Some("msg-1".to_string()));
                assert_eq!(text_delta, Some("Hello".to_string()));
                assert_eq!(thinking_delta, None);
            }
            _ => panic!("expected MessageUpdate"),
        }
    }

    #[test]
    fn pi_event_message_update_thinking_delta() {
        let json =
            r#"{"type":"message_update","message_id":"msg-1","thinking_delta":"Let me think..."}"#;
        let event: PiEvent = serde_json::from_str(json).unwrap();
        match event {
            PiEvent::MessageUpdate { thinking_delta, .. } => {
                assert_eq!(thinking_delta, Some("Let me think...".to_string()));
            }
            _ => panic!("expected MessageUpdate"),
        }
    }

    #[test]
    fn pi_event_tool_execution_start() {
        let json = r#"{"type":"tool_execution_start","execution_id":"exec-1","command":"git status"}"#;
        let event: PiEvent = serde_json::from_str(json).unwrap();
        match event {
            PiEvent::ToolExecutionStart {
                execution_id,
                command,
                ..
            } => {
                assert_eq!(execution_id, Some("exec-1".to_string()));
                assert_eq!(command, Some("git status".to_string()));
            }
            _ => panic!("expected ToolExecutionStart"),
        }
    }

    #[test]
    fn pi_event_tool_execution_end() {
        let json = r#"{"type":"tool_execution_end","execution_id":"exec-1","exit_code":0}"#;
        let event: PiEvent = serde_json::from_str(json).unwrap();
        match event {
            PiEvent::ToolExecutionEnd {
                execution_id,
                exit_code,
                ..
            } => {
                assert_eq!(execution_id, Some("exec-1".to_string()));
                assert_eq!(exit_code, Some(0));
            }
            _ => panic!("expected ToolExecutionEnd"),
        }
    }

    #[test]
    fn pi_event_response() {
        let json = r#"{"type":"response","command":"get_state","data":{"status":"idle"}}"#;
        let event: PiEvent = serde_json::from_str(json).unwrap();
        match event {
            PiEvent::Response { command, data, .. } => {
                assert_eq!(command, Some("get_state".to_string()));
                assert_eq!(
                    data,
                    Some(serde_json::json!({"status": "idle"}))
                );
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn pi_event_error() {
        let json = r#"{"type":"error","message":"something went wrong","code":42}"#;
        let event: PiEvent = serde_json::from_str(json).unwrap();
        match event {
            PiEvent::Error { message, code } => {
                assert_eq!(message, Some("something went wrong".to_string()));
                assert_eq!(code, Some(42));
            }
            _ => panic!("expected Error"),
        }
    }

    // ── JSONL Framing ──────────────────────────────────────────────────

    #[test]
    fn serialize_command_adds_newline() {
        let cmd = PiCommand::Prompt {
            text: "test".to_string(),
            session_id: None,
        };
        let line = serialize_command(&cmd).unwrap();
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn deserialize_event_empty_line() {
        assert_eq!(deserialize_event("").unwrap(), None);
        assert_eq!(deserialize_event("  ").unwrap(), None);
        assert_eq!(deserialize_event("\n").unwrap(), None);
    }

    #[test]
    fn deserialize_event_valid() {
        let json = r#"{"type":"agent_start"}"#;
        let event = deserialize_event(json).unwrap().unwrap();
        match event {
            PiEvent::AgentStart { .. } => {}
            _ => panic!("expected AgentStart"),
        }
    }

    #[test]
    fn deserialize_event_invalid_json() {
        let result = deserialize_event("{broken json");
        assert!(result.is_err());
    }

    #[test]
    fn pi_event_minimal_fields() {
        // All fields are optional — verify minimal events parse.
        let cases = vec![
            (r#"{"type":"agent_start"}"#, "agent_start"),
            (r#"{"type":"agent_end"}"#, "agent_end"),
            (r#"{"type":"turn_start"}"#, "turn_start"),
            (r#"{"type":"turn_end"}"#, "turn_end"),
            (r#"{"type":"message_start"}"#, "message_start"),
            (r#"{"type":"message_update"}"#, "message_update"),
            (r#"{"type":"message_end"}"#, "message_end"),
            (r#"{"type":"toolcall_start"}"#, "toolcall_start"),
            (r#"{"type":"toolcall_delta"}"#, "toolcall_delta"),
            (r#"{"type":"toolcall_end"}"#, "toolcall_end"),
            (r#"{"type":"tool_execution_start"}"#, "tool_execution_start"),
            (r#"{"type":"tool_execution_update"}"#, "tool_execution_update"),
            (r#"{"type":"tool_execution_end"}"#, "tool_execution_end"),
            (r#"{"type":"response"}"#, "response"),
            (r#"{"type":"error"}"#, "error"),
        ];
        for (json, expected_type) in cases {
            let event: PiEvent = serde_json::from_str(json).unwrap_or_else(|e| {
                panic!("failed to parse {json}: {e}");
            });
            let actual_type = match &event {
                PiEvent::AgentStart { .. } => "agent_start",
                PiEvent::AgentEnd { .. } => "agent_end",
                PiEvent::TurnStart { .. } => "turn_start",
                PiEvent::TurnEnd { .. } => "turn_end",
                PiEvent::MessageStart { .. } => "message_start",
                PiEvent::MessageUpdate { .. } => "message_update",
                PiEvent::MessageEnd { .. } => "message_end",
                PiEvent::ToolCallStart { .. } => "toolcall_start",
                PiEvent::ToolCallDelta { .. } => "toolcall_delta",
                PiEvent::ToolCallEnd { .. } => "toolcall_end",
                PiEvent::ToolExecutionStart { .. } => "tool_execution_start",
                PiEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
                PiEvent::ToolExecutionEnd { .. } => "tool_execution_end",
                PiEvent::Response { .. } => "response",
                PiEvent::Error { .. } => "error",
            };
            assert_eq!(actual_type, expected_type, "type mismatch for {json}");
        }
    }

    #[test]
    fn pi_command_all_serializable() {
        let commands = vec![
            PiCommand::Prompt {
                text: "hi".to_string(),
                session_id: None,
            },
            PiCommand::Abort,
            PiCommand::GetState,
            PiCommand::SetModel {
                model: "m".to_string(),
            },
            PiCommand::GetAvailableModels,
            PiCommand::SetThinkingLevel {
                level: PiThinkingLevel::High,
            },
            PiCommand::SetSteeringMode {
                mode: "auto".to_string(),
            },
            PiCommand::Compact,
        ];
        for cmd in &commands {
            let json = serde_json::to_string(cmd).unwrap();
            let back: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert!(back.is_object() || back.is_string(), "command must serialize to object or string: {json}");
        }
    }
}
