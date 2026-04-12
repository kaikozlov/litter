//! Droid `--output-format stream-json` protocol types.
//!
//! Defines the JSONL message types emitted by `droid exec --output-format stream-json`.
//! This is NOT ACP JSON-RPC — it's Droid's own streaming JSON protocol.
//!
//! # Message Types
//!
//! - `system/init` — Session initialization with session ID, tools, model
//! - `message` — User or assistant messages with text content
//! - `tool_call` — Agent invokes a tool with parameters
//! - `tool_result` — Tool execution result (output/error)
//! - `completion` — Session completed with final text and usage stats
//!
//! # Usage
//!
//! ```text
//! droid exec --output-format stream-json --input-format stream-json --cwd <dir>
//!   → stdin: JSONL user messages ({"type":"user_message","text":"..."}\n)
//!   → stdout: JSONL system/message/tool_call/tool_result/completion lines
//! ```

use serde::{Deserialize, Serialize};

/// Top-level message type from Droid's stream-json output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StreamMessage {
    /// Session initialization event emitted first.
    #[serde(rename = "system")]
    System {
        /// Event subtype (always "init" for the first message).
        #[serde(default)]
        subtype: Option<String>,
        /// Working directory.
        #[serde(default)]
        cwd: Option<String>,
        /// Session ID for this execution.
        #[serde(default)]
        session_id: Option<String>,
        /// Available tool names.
        #[serde(default)]
        tools: Option<Vec<String>>,
        /// Model being used.
        #[serde(default)]
        model: Option<String>,
        /// Reasoning effort level.
        #[serde(default)]
        reasoning_effort: Option<String>,
    },

    /// A user or assistant message.
    #[serde(rename = "message")]
    Message {
        /// Message role: "user" or "assistant".
        #[serde(default)]
        role: Option<String>,
        /// Message ID.
        #[serde(default)]
        id: Option<String>,
        /// Message text content.
        #[serde(default)]
        text: Option<String>,
        /// Timestamp (ms since epoch).
        #[serde(default)]
        timestamp: Option<i64>,
        /// Session ID this message belongs to.
        #[serde(default)]
        session_id: Option<String>,
    },

    /// A tool call invocation by the agent.
    #[serde(rename = "tool_call")]
    ToolCall {
        /// Tool call ID.
        #[serde(default)]
        id: Option<String>,
        /// Associated message ID.
        #[serde(default, rename = "messageId")]
        message_id: Option<String>,
        /// Tool identifier (e.g. "LS", "Read", "Execute").
        #[serde(default, rename = "toolId")]
        tool_id: Option<String>,
        /// Human-readable tool name.
        #[serde(default, rename = "toolName")]
        tool_name: Option<String>,
        /// Tool parameters.
        #[serde(default)]
        parameters: Option<serde_json::Value>,
        /// Timestamp.
        #[serde(default)]
        timestamp: Option<i64>,
        /// Session ID.
        #[serde(default)]
        session_id: Option<String>,
    },

    /// A tool execution result.
    #[serde(rename = "tool_result")]
    ToolResult {
        /// Tool call ID this result is for.
        #[serde(default)]
        id: Option<String>,
        /// Associated message ID.
        #[serde(default, rename = "messageId")]
        message_id: Option<String>,
        /// Tool identifier.
        #[serde(default, rename = "toolId")]
        tool_id: Option<String>,
        /// Whether the result is an error.
        #[serde(default, rename = "isError")]
        is_error: Option<bool>,
        /// Tool output value.
        #[serde(default)]
        value: Option<String>,
        /// Timestamp.
        #[serde(default)]
        timestamp: Option<i64>,
        /// Session ID.
        #[serde(default)]
        session_id: Option<String>,
    },

    /// Session completion event.
    #[serde(rename = "completion")]
    Completion {
        /// Final assembled text of the response.
        #[serde(default, rename = "finalText")]
        final_text: Option<String>,
        /// Number of turns in the session.
        #[serde(default, rename = "numTurns")]
        num_turns: Option<u32>,
        /// Total session duration in ms.
        #[serde(default, rename = "durationMs")]
        duration_ms: Option<u64>,
        /// Session ID.
        #[serde(default)]
        session_id: Option<String>,
        /// Timestamp.
        #[serde(default)]
        timestamp: Option<i64>,
        /// Token usage.
        #[serde(default)]
        usage: Option<UsageInfo>,
    },
}

/// Token usage information from completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageInfo {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
}

/// User message input for `--input-format stream-json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessageInput {
    /// Message type identifier.
    pub r#type: String,
    /// The user's prompt text.
    pub text: String,
}

impl UserMessageInput {
    /// Create a new user message input.
    pub fn new(text: &str) -> Self {
        Self {
            r#type: "user_message".to_string(),
            text: text.to_string(),
        }
    }

    /// Serialize to a JSONL line (with trailing newline).
    pub fn to_jsonl(&self) -> Result<String, serde_json::Error> {
        let mut line = serde_json::to_string(self)?;
        line.push('\n');
        Ok(line)
    }
}

/// Parse a line from Droid's stream-json output.
///
/// Returns `Ok(Some(StreamMessage))` for valid messages,
/// `Ok(None)` for empty lines, and `Err` for invalid JSON.
pub fn parse_stream_message(line: &str) -> Result<Option<StreamMessage>, serde_json::Error> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(trimmed).map(Some)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_system_init() {
        let json = r#"{"type":"system","subtype":"init","cwd":"/home/ubuntu","session_id":"sess-1","tools":["Read","LS"],"model":"claude-sonnet-4-20250514","reasoning_effort":"high"}"#;
        let msg = parse_stream_message(json).unwrap().unwrap();
        match msg {
            StreamMessage::System { session_id, model, tools, .. } => {
                assert_eq!(session_id, Some("sess-1".to_string()));
                assert_eq!(model, Some("claude-sonnet-4-20250514".to_string()));
                assert_eq!(tools.unwrap().len(), 2);
            }
            _ => panic!("expected System message"),
        }
    }

    #[test]
    fn parse_user_message() {
        let json = r#"{"type":"message","role":"user","id":"msg-1","text":"hello","timestamp":1234,"session_id":"sess-1"}"#;
        let msg = parse_stream_message(json).unwrap().unwrap();
        match msg {
            StreamMessage::Message { role, text, .. } => {
                assert_eq!(role, Some("user".to_string()));
                assert_eq!(text, Some("hello".to_string()));
            }
            _ => panic!("expected Message"),
        }
    }

    #[test]
    fn parse_assistant_message() {
        let json = r#"{"type":"message","role":"assistant","id":"msg-2","text":"Hello!","session_id":"sess-1"}"#;
        let msg = parse_stream_message(json).unwrap().unwrap();
        match msg {
            StreamMessage::Message { role, text, .. } => {
                assert_eq!(role, Some("assistant".to_string()));
                assert_eq!(text, Some("Hello!".to_string()));
            }
            _ => panic!("expected Message"),
        }
    }

    #[test]
    fn parse_tool_call() {
        let json = r#"{"type":"tool_call","id":"call-1","messageId":"msg-3","toolId":"LS","toolName":"LS","parameters":{"directory_path":"/tmp"},"session_id":"sess-1"}"#;
        let msg = parse_stream_message(json).unwrap().unwrap();
        match msg {
            StreamMessage::ToolCall { tool_name, parameters, .. } => {
                assert_eq!(tool_name, Some("LS".to_string()));
                assert_eq!(parameters.unwrap()["directory_path"], "/tmp");
            }
            _ => panic!("expected ToolCall"),
        }
    }

    #[test]
    fn parse_tool_result() {
        let json = r#"{"type":"tool_result","id":"call-1","messageId":"msg-4","toolId":"LS","isError":false,"value":"file1\nfile2","session_id":"sess-1"}"#;
        let msg = parse_stream_message(json).unwrap().unwrap();
        match msg {
            StreamMessage::ToolResult { value, is_error, .. } => {
                assert_eq!(is_error, Some(false));
                assert!(value.unwrap().contains("file1"));
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn parse_completion() {
        let json = r#"{"type":"completion","finalText":"Hello!","numTurns":1,"durationMs":3000,"session_id":"sess-1","timestamp":1234,"usage":{"inputTokens":100,"outputTokens":5}}"#;
        let msg = parse_stream_message(json).unwrap().unwrap();
        match msg {
            StreamMessage::Completion { final_text, num_turns, usage, .. } => {
                assert_eq!(final_text, Some("Hello!".to_string()));
                assert_eq!(num_turns, Some(1));
                let u = usage.unwrap();
                assert_eq!(u.input_tokens, Some(100));
                assert_eq!(u.output_tokens, Some(5));
            }
            _ => panic!("expected Completion"),
        }
    }

    #[test]
    fn parse_empty_line() {
        assert!(parse_stream_message("").unwrap().is_none());
        assert!(parse_stream_message("  ").unwrap().is_none());
    }

    #[test]
    fn parse_invalid_json() {
        assert!(parse_stream_message("{broken").is_err());
    }

    #[test]
    fn user_message_input_serialization() {
        let input = UserMessageInput::new("list files");
        let jsonl = input.to_jsonl().unwrap();
        assert!(jsonl.contains("\"type\":\"user_message\""));
        assert!(jsonl.contains("\"text\":\"list files\""));
        assert!(jsonl.ends_with('\n'));
    }

    #[test]
    fn user_message_roundtrip() {
        let input = UserMessageInput::new("hello world");
        let jsonl = input.to_jsonl().unwrap();
        let parsed: UserMessageInput = serde_json::from_str(jsonl.trim()).unwrap();
        assert_eq!(parsed.text, "hello world");
        assert_eq!(parsed.r#type, "user_message");
    }
}
