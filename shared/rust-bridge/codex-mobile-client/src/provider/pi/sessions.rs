//! Pi session persistence.
//!
//! Manages Pi's session files stored in `~/.pi/agent/sessions/*.jsonl` on
//! the remote host. Supports:
//! - Listing available sessions (reads directory listing via SSH)
//! - Loading session history (reads `.jsonl` file contents via SSH)
//! - Parsing session metadata (session ID, timestamps, message count)
//!
//! Session files are newline-delimited JSON where each line is an event
//! from Pi's event stream. The module parses these events to extract
//! session metadata and reconstruct conversation history.

use crate::provider::SessionInfo;
use crate::ssh::{SshClient, SshError};

/// Default Pi sessions directory on the remote host.
const PI_SESSIONS_DIR: &str = "~/.pi/agent/sessions";

/// Maximum number of lines to read from a session file for metadata.
const MAX_METADATA_LINES: usize = 100;

/// Parsed Pi session file entry from directory listing.
#[derive(Debug, Clone)]
pub struct PiSessionEntry {
    /// Filename (e.g. "session-abc123.jsonl").
    pub filename: String,
    /// Full remote path.
    pub path: String,
}

/// Detailed session metadata parsed from a Pi session file.
#[derive(Debug, Clone)]
pub struct PiSessionMetadata {
    /// Session ID extracted from the first `agent_start` event.
    pub session_id: String,
    /// First user prompt in the session (used as title).
    pub first_prompt: Option<String>,
    /// Number of messages in the session.
    pub message_count: usize,
    /// Number of JSONL lines in the file.
    pub line_count: usize,
}

/// Result of parsing a Pi session JSONL line.
#[derive(Debug, Clone)]
enum PiSessionLine {
    /// An agent_start event with session ID.
    AgentStart { session_id: Option<String> },
    /// A prompt command with text.
    Prompt { text: String },
    /// A message event.
    Message,
    /// Other event (ignored for metadata).
    Other,
}

/// List Pi sessions from `~/.pi/agent/sessions/*.jsonl` on the remote host.
///
/// Uses `ls` via SSH to enumerate session files. Returns session metadata
/// for each file found. If the directory doesn't exist or is empty, returns
/// an empty list (not an error).
pub async fn list_pi_sessions(ssh_client: &SshClient) -> Result<Vec<SessionInfo>, SshError> {
    let entries = list_session_files(ssh_client).await?;

    let mut sessions = Vec::new();
    for entry in entries {
        // Read the first few lines to extract metadata.
        match read_session_metadata(ssh_client, &entry.path).await {
            Ok(metadata) => {
                sessions.push(SessionInfo {
                    id: metadata.session_id.clone(),
                    title: metadata
                        .first_prompt
                        .unwrap_or_else(|| metadata.session_id.clone()),
                    created_at: String::new(), // Pi doesn't store timestamps in a standard format.
                    updated_at: String::new(),
                });
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to read Pi session metadata for {}: {e}",
                    entry.filename
                );
                // Still include the session with basic info.
                sessions.push(SessionInfo {
                    id: entry.filename.clone(),
                    title: entry.filename.clone(),
                    created_at: String::new(),
                    updated_at: String::new(),
                });
            }
        }
    }

    Ok(sessions)
}

/// List session files in the Pi sessions directory.
///
/// Returns parsed entries with filename and full path.
async fn list_session_files(ssh_client: &SshClient) -> Result<Vec<PiSessionEntry>, SshError> {
    let cmd = format!("ls -1 {PI_SESSIONS_DIR}/*.jsonl 2>/dev/null");
    let result = ssh_client.exec(&cmd).await?;

    if result.exit_code != 0 {
        // Directory doesn't exist or no sessions.
        return Ok(Vec::new());
    }

    let entries: Vec<PiSessionEntry> = result
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let path = line.trim().to_string();
            let filename = path
                .rsplit('/')
                .next()
                .unwrap_or(&path)
                .to_string();
            PiSessionEntry { filename, path }
        })
        .collect();

    Ok(entries)
}

/// Read metadata from a Pi session file.
///
/// Reads the first `MAX_METADATA_LINES` lines and extracts:
/// - Session ID from `agent_start` event
/// - First prompt text
/// - Message count
async fn read_session_metadata(
    ssh_client: &SshClient,
    path: &str,
) -> Result<PiSessionMetadata, SshError> {
    let cmd = format!("head -{MAX_METADATA_LINES} {path}");
    let result = ssh_client.exec(&cmd).await?;

    if result.exit_code != 0 {
        return Err(SshError::ExecFailed {
            exit_code: result.exit_code,
            stderr: "failed to read session file".to_string(),
        });
    }

    parse_session_metadata(&result.stdout)
}

/// Parse session metadata from raw JSONL content.
fn parse_session_metadata(content: &str) -> Result<PiSessionMetadata, SshError> {
    let mut session_id = String::new();
    let mut first_prompt = None;
    let mut message_count = 0;
    let mut line_count = 0;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        line_count += 1;

        let parsed = parse_session_line(trimmed);
        match parsed {
            PiSessionLine::AgentStart { session_id: sid } => {
                if let Some(sid) = sid {
                    session_id = sid;
                }
            }
            PiSessionLine::Prompt { text } => {
                if first_prompt.is_none() {
                    first_prompt = Some(text);
                }
            }
            PiSessionLine::Message => {
                message_count += 1;
            }
            PiSessionLine::Other => {}
        }
    }

    // Use the filename stem as fallback session ID.
    if session_id.is_empty() {
        session_id = format!("pi-session-{line_count}");
    }

    Ok(PiSessionMetadata {
        session_id,
        first_prompt,
        message_count,
        line_count,
    })
}

/// Parse a single Pi session JSONL line.
fn parse_session_line(line: &str) -> PiSessionLine {
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return PiSessionLine::Other,
    };

    let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match event_type {
        "agent_start" => {
            let session_id = value
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            PiSessionLine::AgentStart { session_id }
        }
        "prompt" => {
            let text = value
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            PiSessionLine::Prompt { text }
        }
        "message_start" | "message_update" | "message_end" => PiSessionLine::Message,
        "agent_end" | "turn_start" | "turn_end" => PiSessionLine::Other,
        "response" => PiSessionLine::Other,
        "error" => PiSessionLine::Other,
        _ => PiSessionLine::Other,
    }
}

/// Read a Pi session file's full content via SSH.
///
/// Used for loading session history for replay/resume.
pub async fn read_session_file(
    ssh_client: &SshClient,
    path: &str,
) -> Result<String, SshError> {
    let cmd = format!("cat {path}");
    let result = ssh_client.exec(&cmd).await?;

    if result.exit_code != 0 {
        return Err(SshError::ExecFailed {
            exit_code: result.exit_code,
            stderr: if result.stderr.trim().is_empty() {
                "failed to read session file".to_string()
            } else {
                result.stderr
            },
        });
    }

    Ok(result.stdout)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Session line parsing tests ─────────────────────────────────────

    #[test]
    fn parse_agent_start_event() {
        let line = r#"{"type":"agent_start","session_id":"sess-123"}"#;
        let result = parse_session_line(line);
        match result {
            PiSessionLine::AgentStart { session_id } => {
                assert_eq!(session_id, Some("sess-123".to_string()));
            }
            _ => panic!("expected AgentStart, got {result:?}"),
        }
    }

    #[test]
    fn parse_agent_start_no_session_id() {
        let line = r#"{"type":"agent_start"}"#;
        let result = parse_session_line(line);
        match result {
            PiSessionLine::AgentStart { session_id } => {
                assert_eq!(session_id, None);
            }
            _ => panic!("expected AgentStart, got {result:?}"),
        }
    }

    #[test]
    fn parse_prompt_event() {
        let line = r#"{"type":"prompt","text":"Hello Pi"}"#;
        let result = parse_session_line(line);
        match result {
            PiSessionLine::Prompt { text } => {
                assert_eq!(text, "Hello Pi");
            }
            _ => panic!("expected Prompt, got {result:?}"),
        }
    }

    #[test]
    fn parse_message_start_event() {
        let line = r#"{"type":"message_start","message_id":"msg-1","role":"assistant"}"#;
        let result = parse_session_line(line);
        assert!(matches!(result, PiSessionLine::Message));
    }

    #[test]
    fn parse_message_update_event() {
        let line = r#"{"type":"message_update","message_id":"msg-1","text_delta":"Hello"}"#;
        let result = parse_session_line(line);
        assert!(matches!(result, PiSessionLine::Message));
    }

    #[test]
    fn parse_message_end_event() {
        let line = r#"{"type":"message_end","message_id":"msg-1"}"#;
        let result = parse_session_line(line);
        assert!(matches!(result, PiSessionLine::Message));
    }

    #[test]
    fn parse_unknown_event() {
        let line = r#"{"type":"some_future_event","data":{}}"#;
        let result = parse_session_line(line);
        assert!(matches!(result, PiSessionLine::Other));
    }

    #[test]
    fn parse_invalid_json() {
        let line = r#"{broken json"#;
        let result = parse_session_line(line);
        assert!(matches!(result, PiSessionLine::Other));
    }

    #[test]
    fn parse_no_type_field() {
        let line = r#"{"foo":"bar"}"#;
        let result = parse_session_line(line);
        assert!(matches!(result, PiSessionLine::Other));
    }

    // ── Session metadata parsing tests ─────────────────────────────────

    #[test]
    fn parse_metadata_complete_session() {
        let content = r#"{"type":"agent_start","session_id":"sess-abc"}
{"type":"prompt","text":"What is 2+2?"}
{"type":"message_start","message_id":"msg-1","role":"assistant"}
{"type":"message_update","message_id":"msg-1","text_delta":"4"}
{"type":"message_end","message_id":"msg-1"}
{"type":"agent_end","reason":"complete"}"#;

        let metadata = parse_session_metadata(content).unwrap();
        assert_eq!(metadata.session_id, "sess-abc");
        assert_eq!(metadata.first_prompt, Some("What is 2+2?".to_string()));
        assert_eq!(metadata.message_count, 3); // message_start + message_update + message_end
        assert_eq!(metadata.line_count, 6);
    }

    #[test]
    fn parse_metadata_empty_session() {
        let content = "";
        let metadata = parse_session_metadata(content).unwrap();
        assert!(metadata.session_id.starts_with("pi-session-"));
        assert_eq!(metadata.first_prompt, None);
        assert_eq!(metadata.message_count, 0);
        assert_eq!(metadata.line_count, 0);
    }

    #[test]
    fn parse_metadata_multiple_prompts() {
        let content = r#"{"type":"agent_start","session_id":"sess-xyz"}
{"type":"prompt","text":"First question"}
{"type":"message_start","message_id":"msg-1","role":"assistant"}
{"type":"message_end","message_id":"msg-1"}
{"type":"agent_end","reason":"complete"}
{"type":"agent_start"}
{"type":"prompt","text":"Second question"}
{"type":"message_start","message_id":"msg-2","role":"assistant"}
{"type":"message_end","message_id":"msg-2"}
{"type":"agent_end","reason":"complete"}"#;

        let metadata = parse_session_metadata(content).unwrap();
        assert_eq!(metadata.session_id, "sess-xyz");
        // First prompt should be captured.
        assert_eq!(metadata.first_prompt, Some("First question".to_string()));
        assert_eq!(metadata.message_count, 4); // 2 message_start + 2 message_end
    }

    #[test]
    fn parse_metadata_malformed_lines_skipped() {
        let content = r#"{"type":"agent_start","session_id":"sess-1"}
{broken json
{"type":"prompt","text":"Hello"}
{"type":"message_start","message_id":"msg-1"}
unknown garbage"#;

        let metadata = parse_session_metadata(content).unwrap();
        assert_eq!(metadata.session_id, "sess-1");
        assert_eq!(metadata.first_prompt, Some("Hello".to_string()));
        assert_eq!(metadata.message_count, 1);
    }

    // ── VAL-PI-007: Session persistence listing and hydration ──────────

    #[test]
    fn session_entry_parsing_from_ls_output() {
        let output = "/home/ubuntu/.pi/agent/sessions/sess-abc.jsonl\n/home/ubuntu/.pi/agent/sessions/sess-def.jsonl\n";
        let entries: Vec<PiSessionEntry> = output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let path = line.trim().to_string();
                let filename = path.rsplit('/').next().unwrap_or(&path).to_string();
                PiSessionEntry { filename, path }
            })
            .collect();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].filename, "sess-abc.jsonl");
        assert_eq!(entries[0].path, "/home/ubuntu/.pi/agent/sessions/sess-abc.jsonl");
        assert_eq!(entries[1].filename, "sess-def.jsonl");
    }

    #[test]
    fn session_entry_empty_ls_output() {
        let output = "";
        let entries: Vec<PiSessionEntry> = output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let path = line.trim().to_string();
                let filename = path.rsplit('/').next().unwrap_or(&path).to_string();
                PiSessionEntry { filename, path }
            })
            .collect();

        assert!(entries.is_empty());
    }

    // ── SessionInfo conversion ─────────────────────────────────────────

    #[test]
    fn session_info_from_metadata() {
        let metadata = PiSessionMetadata {
            session_id: "sess-123".to_string(),
            first_prompt: Some("What is Rust?".to_string()),
            message_count: 5,
            line_count: 12,
        };

        let info = SessionInfo {
            id: metadata.session_id.clone(),
            title: metadata
                .first_prompt
                .unwrap_or_else(|| metadata.session_id.clone()),
            created_at: String::new(),
            updated_at: String::new(),
        };

        assert_eq!(info.id, "sess-123");
        assert_eq!(info.title, "What is Rust?");
    }

    #[test]
    fn session_info_from_metadata_no_prompt() {
        let metadata = PiSessionMetadata {
            session_id: "sess-456".to_string(),
            first_prompt: None,
            message_count: 0,
            line_count: 1,
        };

        let info = SessionInfo {
            id: metadata.session_id.clone(),
            title: metadata
                .first_prompt
                .unwrap_or_else(|| metadata.session_id.clone()),
            created_at: String::new(),
            updated_at: String::new(),
        };

        assert_eq!(info.id, "sess-456");
        assert_eq!(info.title, "sess-456"); // Falls back to session ID.
    }
}
