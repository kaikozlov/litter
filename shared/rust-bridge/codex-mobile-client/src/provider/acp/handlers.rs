//! Client-side request handlers for ACP agent requests.
//!
//! When the agent sends a request to the client (e.g., `fs/read_text_file`,
//! `fs/write_text_file`, `request_permission`, `terminal/*`), these handlers
//! process the request and return an appropriate response.
//!
//! # Handler Categories
//!
//! - **fs/read_text_file**: Reads file content from the agent's working directory.
//!   In default mobile mode, returns "unsupported" error (-32601).
//!   With a filesystem delegate configured (testing), returns actual file content.
//!
//! - **fs/write_text_file**: Writes file via the filesystem delegate.
//!   In default mobile mode, returns "unsupported" error (-32601).
//!
//! - **request_permission**: Applies the per-agent permission policy
//!   (AutoApproveAll, AutoRejectHighRisk, PromptAlways).
//!   When PromptAlways, emits a ProviderEvent::ApprovalRequested.
//!
//! - **terminal/\***: Returns MethodNotFound (-32601) for all terminal operations,
//!   since mobile clients don't support terminal sessions.

use agent_client_protocol_schema::{
    AgentRequest, ClientResponse, PermissionOption, PermissionOptionId, PermissionOptionKind,
    ReadTextFileResponse, RequestId, RequestPermissionOutcome, RequestPermissionResponse,
    SelectedPermissionOutcome, WriteTextFileResponse,
};

use crate::provider::AgentPermissionPolicy;
use crate::provider::acp::framing;

/// Result of handling an agent request.
#[derive(Debug)]
pub enum HandleResult {
    /// The handler produced a response that should be sent back to the agent.
    Respond {
        /// Serialized JSON-RPC response line.
        response_line: String,
    },
    /// The handler determined that the user should be prompted (permission request).
    /// The caller should emit a `ProviderEvent::ApprovalRequested` and wait for
    /// the user's response, then call `handle_permission_user_response`.
    PromptUser {
        /// The request ID to correlate the response.
        request_id: RequestId,
        /// Tool call ID for tracking.
        tool_call_id: String,
        /// Tool name/title.
        tool_title: String,
        /// Available options for the user.
        options: Vec<PermissionOption>,
    },
    /// The handler encountered an error and cannot produce a response.
    Error {
        /// Human-readable error description.
        message: String,
    },
}

/// Trait for filesystem operations on the agent's working directory.
///
/// In production mobile mode, no delegate is configured and all fs operations
/// return "unsupported". In testing, a delegate can be injected to simulate
/// file operations.
pub trait FsDelegate: Send + Sync {
    /// Read a text file at the given path.
    fn read_text_file(&self, path: &str) -> Result<String, String>;

    /// Write content to a text file at the given path.
    fn write_text_file(&self, path: &str, content: &str) -> Result<(), String>;
}

/// A no-op filesystem delegate that always returns unsupported.
pub struct NoFsDelegate;

impl FsDelegate for NoFsDelegate {
    fn read_text_file(&self, _path: &str) -> Result<String, String> {
        Err("fs/read_text_file not supported on mobile".to_string())
    }

    fn write_text_file(&self, _path: &str, _content: &str) -> Result<(), String> {
        Err("fs/write_text_file not supported on mobile".to_string())
    }
}

/// Handle an incoming agent request.
///
/// Returns a `HandleResult` indicating what action the caller should take:
/// - `Respond`: Send the serialized response back to the agent.
/// - `PromptUser`: The permission policy is `PromptAlways`; emit an event and wait.
/// - `Error`: The request could not be handled.
pub fn handle_agent_request(
    request_id: &RequestId,
    request: &AgentRequest,
    permission_policy: AgentPermissionPolicy,
    fs_delegate: &dyn FsDelegate,
) -> HandleResult {
    match request {
        AgentRequest::ReadTextFileRequest(req) => {
            handle_fs_read(request_id, &req.path.to_string_lossy(), fs_delegate)
        }
        AgentRequest::WriteTextFileRequest(req) => {
            handle_fs_write(request_id, &req.path.to_string_lossy(), &req.content, fs_delegate)
        }
        AgentRequest::RequestPermissionRequest(req) => {
            handle_request_permission(request_id, req, permission_policy)
        }
        // All terminal/* methods return MethodNotFound for mobile.
        AgentRequest::CreateTerminalRequest(_)
        | AgentRequest::TerminalOutputRequest(_)
        | AgentRequest::ReleaseTerminalRequest(_)
        | AgentRequest::WaitForTerminalExitRequest(_)
        | AgentRequest::KillTerminalRequest(_) => {
            handle_terminal_unsupported(request_id, request.method())
        }
        // Extension methods and future variants are not supported.
        _ => {
            handle_terminal_unsupported(request_id, request.method())
        }
    }
}

/// Handle `fs/read_text_file` request.
fn handle_fs_read(
    request_id: &RequestId,
    path: &str,
    fs_delegate: &dyn FsDelegate,
) -> HandleResult {
    match fs_delegate.read_text_file(path) {
        Ok(content) => {
            let response = ClientResponse::ReadTextFileResponse(
                ReadTextFileResponse::new(&content),
            );
            let response_line = framing::serialize_client_response(request_id, &response)
                .unwrap_or_else(|e| {
                    framing::serialize_client_error_response(
                        request_id,
                        -32603,
                        &format!("serialization error: {e}"),
                    )
                    .unwrap_or_default()
                });
            HandleResult::Respond { response_line }
        }
        Err(message) => {
            // Return unsupported error for mobile.
            let response_line = framing::serialize_client_error_response(
                request_id,
                -32601,
                &message,
            )
            .unwrap_or_default();
            HandleResult::Respond { response_line }
        }
    }
}

/// Handle `fs/write_text_file` request.
fn handle_fs_write(
    request_id: &RequestId,
    path: &str,
    content: &str,
    fs_delegate: &dyn FsDelegate,
) -> HandleResult {
    match fs_delegate.write_text_file(path, content) {
        Ok(()) => {
            let response = ClientResponse::WriteTextFileResponse(
                WriteTextFileResponse::new(),
            );
            let response_line = framing::serialize_client_response(request_id, &response)
                .unwrap_or_else(|e| {
                    framing::serialize_client_error_response(
                        request_id,
                        -32603,
                        &format!("serialization error: {e}"),
                    )
                    .unwrap_or_default()
                });
            HandleResult::Respond { response_line }
        }
        Err(message) => {
            let response_line = framing::serialize_client_error_response(
                request_id,
                -32601,
                &message,
            )
            .unwrap_or_default();
            HandleResult::Respond { response_line }
        }
    }
}

/// Handle `request_permission` request.
///
/// Applies the per-agent permission policy:
/// - `AutoApproveAll`: Immediately responds with the first "allow" option.
/// - `AutoRejectHighRisk`: Responds with the first "reject" option for high-risk,
///   first "allow" option for others.
/// - `PromptAlways`: Returns `HandleResult::PromptUser` so the caller can
///   surface the request to the user via `ProviderEvent::ApprovalRequested`.
fn handle_request_permission(
    request_id: &RequestId,
    req: &agent_client_protocol_schema::RequestPermissionRequest,
    policy: AgentPermissionPolicy,
) -> HandleResult {
    let tool_call_id = req.tool_call.tool_call_id.to_string();
    let tool_title = req
        .tool_call
        .fields
        .title
        .clone()
        .unwrap_or_default();

    match policy {
        AgentPermissionPolicy::AutoApproveAll => {
            // Find the first allow option.
            let allow_option = req.options.iter().find(|o| {
                matches!(
                    o.kind,
                    PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
                )
            });

            let outcome = match allow_option {
                Some(option) => RequestPermissionOutcome::Selected(
                    SelectedPermissionOutcome::new(option.option_id.clone()),
                ),
                None => {
                    // No allow option available — reject.
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                        PermissionOptionId::new("auto_reject"),
                    ))
                }
            };

            let response = ClientResponse::RequestPermissionResponse(
                RequestPermissionResponse::new(outcome),
            );
            let response_line = framing::serialize_client_response(request_id, &response)
                .unwrap_or_default();
            HandleResult::Respond { response_line }
        }

        AgentPermissionPolicy::AutoRejectHighRisk => {
            // Determine if this is a high-risk operation based on tool kind.
            let is_high_risk = is_high_risk_tool_call(&req.tool_call.fields);

            let selected_option = if is_high_risk {
                // Find the first reject option.
                req.options.iter().find(|o| {
                    matches!(
                        o.kind,
                        PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways
                    )
                })
            } else {
                // Find the first allow option.
                req.options.iter().find(|o| {
                    matches!(
                        o.kind,
                        PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
                    )
                })
            };

            let outcome = match selected_option {
                Some(option) => RequestPermissionOutcome::Selected(
                    SelectedPermissionOutcome::new(option.option_id.clone()),
                ),
                None => {
                    // Fallback: reject if high-risk, approve otherwise.
                    let fallback_id = if is_high_risk {
                        "auto_reject"
                    } else {
                        "auto_approve"
                    };
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                        PermissionOptionId::new(fallback_id),
                    ))
                }
            };

            let response = ClientResponse::RequestPermissionResponse(
                RequestPermissionResponse::new(outcome),
            );
            let response_line = framing::serialize_client_response(request_id, &response)
                .unwrap_or_default();
            HandleResult::Respond { response_line }
        }

        AgentPermissionPolicy::PromptAlways => {
            // Return PromptUser so the caller can surface the request to the user.
            HandleResult::PromptUser {
                request_id: request_id.clone(),
                tool_call_id,
                tool_title,
                options: req.options.clone(),
            }
        }
    }
}

/// Determine if a tool call is high-risk based on its kind and properties.
fn is_high_risk_tool_call(fields: &agent_client_protocol_schema::ToolCallUpdateFields) -> bool {
    use agent_client_protocol_schema::ToolKind;

    matches!(fields.kind, Some(ToolKind::Delete) | Some(ToolKind::Execute))
}

/// Handle a user's response to a permission prompt.
///
/// Call this when the user has responded to a `ProviderEvent::ApprovalRequested`
/// event. It serializes the response for sending back to the agent.
pub fn handle_permission_user_response(
    request_id: &RequestId,
    selected_option_id: &PermissionOptionId,
) -> Result<String, String> {
    let outcome = RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
        selected_option_id.clone(),
    ));
    let response = ClientResponse::RequestPermissionResponse(RequestPermissionResponse::new(outcome));
    framing::serialize_client_response(request_id, &response)
        .map_err(|e| format!("serialization error: {e}"))
}

/// Handle unsupported terminal/* requests by returning MethodNotFound.
fn handle_terminal_unsupported(request_id: &RequestId, method: &str) -> HandleResult {
    let response_line = framing::serialize_client_error_response(
        request_id,
        -32601,
        &format!("method not found: {method} (mobile does not support terminal operations)"),
    )
    .unwrap_or_default();
    HandleResult::Respond { response_line }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol_schema::{
        SessionId, ToolCallUpdate, ToolCallUpdateFields, ToolKind, ToolCallId,
    };
    use serde_json::Value;

    /// Helper to parse a JSON-RPC response.
    fn parse_response(line: &str) -> Value {
        serde_json::from_str(line).expect("response should be valid JSON")
    }

    // ── fs/read_text_file ──────────────────────────────────────────────

    #[test]
    fn acp_fs_read_text_file_unsupported_without_delegate() {
        let delegate = NoFsDelegate;
        let request_id = RequestId::Number(1);
        let request = AgentRequest::ReadTextFileRequest(
            agent_client_protocol_schema::ReadTextFileRequest::new(
                SessionId::new("sess-1"),
                "/tmp/test.txt",
            ),
        );

        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::PromptAlways,
            &delegate,
        );

        match result {
            HandleResult::Respond { response_line } => {
                let resp = parse_response(&response_line);
                assert_eq!(resp["id"], 1);
                assert!(resp.get("error").is_some(), "should have error field");
                assert_eq!(resp["error"]["code"], -32601);
            }
            other => panic!("expected Respond, got {other:?}"),
        }
    }

    #[test]
    fn acp_fs_read_text_file_returns_content_with_delegate() {
        struct TestDelegate;
        impl FsDelegate for TestDelegate {
            fn read_text_file(&self, path: &str) -> Result<String, String> {
                if path == "/tmp/hello.txt" {
                    Ok("hello world".to_string())
                } else {
                    Err(format!("file not found: {path}"))
                }
            }
            fn write_text_file(&self, _path: &str, _content: &str) -> Result<(), String> {
                Ok(())
            }
        }

        let delegate = TestDelegate;
        let request_id = RequestId::Number(2);
        let request = AgentRequest::ReadTextFileRequest(
            agent_client_protocol_schema::ReadTextFileRequest::new(
                SessionId::new("sess-1"),
                "/tmp/hello.txt",
            ),
        );

        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::PromptAlways,
            &delegate,
        );

        match result {
            HandleResult::Respond { response_line } => {
                let resp = parse_response(&response_line);
                assert_eq!(resp["id"], 2);
                let content = resp["result"]["content"].as_str().unwrap();
                assert_eq!(content, "hello world");
            }
            other => panic!("expected Respond, got {other:?}"),
        }
    }

    // ── fs/write_text_file ─────────────────────────────────────────────

    #[test]
    fn acp_fs_write_text_file_responds_unsupported() {
        let delegate = NoFsDelegate;
        let request_id = RequestId::Number(3);
        let request = AgentRequest::WriteTextFileRequest(
            agent_client_protocol_schema::WriteTextFileRequest::new(
                SessionId::new("sess-1"),
                "/tmp/out.txt",
                "test content",
            ),
        );

        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::PromptAlways,
            &delegate,
        );

        match result {
            HandleResult::Respond { response_line } => {
                let resp = parse_response(&response_line);
                assert_eq!(resp["id"], 3);
                assert!(resp.get("error").is_some(), "should have error field");
                assert_eq!(resp["error"]["code"], -32601);
            }
            other => panic!("expected Respond, got {other:?}"),
        }
    }

    #[test]
    fn acp_fs_write_text_file_succeeds_with_delegate() {
        struct TestDelegate;
        impl FsDelegate for TestDelegate {
            fn read_text_file(&self, _path: &str) -> Result<String, String> {
                Ok(String::new())
            }
            fn write_text_file(&self, path: &str, content: &str) -> Result<(), String> {
                if path == "/tmp/out.txt" && content == "hello" {
                    Ok(())
                } else {
                    Err("write failed".to_string())
                }
            }
        }

        let delegate = TestDelegate;
        let request_id = RequestId::Number(4);
        let request = AgentRequest::WriteTextFileRequest(
            agent_client_protocol_schema::WriteTextFileRequest::new(
                SessionId::new("sess-1"),
                "/tmp/out.txt",
                "hello",
            ),
        );

        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::PromptAlways,
            &delegate,
        );

        match result {
            HandleResult::Respond { response_line } => {
                let resp = parse_response(&response_line);
                assert_eq!(resp["id"], 4);
                assert!(resp.get("result").is_some(), "should have result field");
            }
            other => panic!("expected Respond, got {other:?}"),
        }
    }

    // ── request_permission ─────────────────────────────────────────────

    fn make_permission_request(
        tool_title: &str,
        tool_kind: Option<ToolKind>,
    ) -> agent_client_protocol_schema::RequestPermissionRequest {
        let mut fields = ToolCallUpdateFields::new().title(tool_title);
        if let Some(kind) = tool_kind {
            fields = fields.kind(kind);
        }
        agent_client_protocol_schema::RequestPermissionRequest::new(
            SessionId::new("sess-1"),
            ToolCallUpdate::new(ToolCallId::new("call-1"), fields),
            vec![
                PermissionOption::new(
                    PermissionOptionId::new("allow"),
                    "Allow",
                    PermissionOptionKind::AllowOnce,
                ),
                PermissionOption::new(
                    PermissionOptionId::new("reject"),
                    "Reject",
                    PermissionOptionKind::RejectOnce,
                ),
            ],
        )
    }

    #[test]
    fn acp_permission_auto_approve() {
        let delegate = NoFsDelegate;
        let request_id = RequestId::Number(5);
        let perm_req = make_permission_request("Read file", None);
        let request = AgentRequest::RequestPermissionRequest(perm_req);

        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::AutoApproveAll,
            &delegate,
        );

        match result {
            HandleResult::Respond { response_line } => {
                let resp = parse_response(&response_line);
                assert_eq!(resp["id"], 5);
                // RequestPermissionOutcome::Selected serializes as { "outcome": "selected", "optionId": "..." }
                // wrapped in RequestPermissionResponse { outcome: ... }
                let outcome = &resp["result"]["outcome"];
                assert_eq!(outcome["outcome"], "selected");
                assert_eq!(outcome["optionId"], "allow");
            }
            other => panic!("expected Respond, got {other:?}"),
        }
    }

    #[test]
    fn acp_permission_auto_reject() {
        let delegate = NoFsDelegate;
        let request_id = RequestId::Number(6);
        let perm_req = make_permission_request("Delete files", Some(ToolKind::Delete));
        let request = AgentRequest::RequestPermissionRequest(perm_req);

        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::AutoRejectHighRisk,
            &delegate,
        );

        match result {
            HandleResult::Respond { response_line } => {
                let resp = parse_response(&response_line);
                assert_eq!(resp["id"], 6);
                let outcome = &resp["result"]["outcome"];
                assert_eq!(outcome["outcome"], "selected");
                assert_eq!(outcome["optionId"], "reject", "high-risk should be auto-rejected");
            }
            other => panic!("expected Respond, got {other:?}"),
        }
    }

    #[test]
    fn acp_permission_auto_reject_approves_low_risk() {
        let delegate = NoFsDelegate;
        let request_id = RequestId::Number(7);
        let perm_req = make_permission_request("Read file", Some(ToolKind::Read));
        let request = AgentRequest::RequestPermissionRequest(perm_req);

        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::AutoRejectHighRisk,
            &delegate,
        );

        match result {
            HandleResult::Respond { response_line } => {
                let resp = parse_response(&response_line);
                assert_eq!(resp["id"], 7);
                let outcome = &resp["result"]["outcome"];
                assert_eq!(outcome["outcome"], "selected");
                assert_eq!(outcome["optionId"], "allow", "low-risk should be auto-approved");
            }
            other => panic!("expected Respond, got {other:?}"),
        }
    }

    #[test]
    fn acp_permission_prompt_emits_event() {
        let delegate = NoFsDelegate;
        let request_id = RequestId::Number(8);
        let perm_req = make_permission_request("Execute command", Some(ToolKind::Execute));
        let request = AgentRequest::RequestPermissionRequest(perm_req);

        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::PromptAlways,
            &delegate,
        );

        match result {
            HandleResult::PromptUser {
                request_id: rid,
                tool_call_id,
                tool_title,
                options,
            } => {
                assert_eq!(rid, RequestId::Number(8));
                assert_eq!(tool_call_id, "call-1");
                assert_eq!(tool_title, "Execute command");
                assert_eq!(options.len(), 2);
            }
            other => panic!("expected PromptUser, got {other:?}"),
        }
    }

    #[test]
    fn acp_per_agent_policy_independence() {
        let delegate = NoFsDelegate;

        // Agent A: AutoApproveAll → approves.
        let request_id = RequestId::Number(10);
        let perm_req = make_permission_request("Delete files", Some(ToolKind::Delete));
        let request = AgentRequest::RequestPermissionRequest(perm_req);
        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::AutoApproveAll,
            &delegate,
        );
        match result {
            HandleResult::Respond { response_line } => {
                let resp = parse_response(&response_line);
                let outcome = &resp["result"]["outcome"];
                assert_eq!(outcome["optionId"], "allow", "Agent A should auto-approve");
            }
            other => panic!("expected Respond, got {other:?}"),
        }

        // Agent B: AutoRejectHighRisk → rejects the same high-risk request.
        let request_id = RequestId::Number(11);
        let perm_req = make_permission_request("Delete files", Some(ToolKind::Delete));
        let request = AgentRequest::RequestPermissionRequest(perm_req);
        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::AutoRejectHighRisk,
            &delegate,
        );
        match result {
            HandleResult::Respond { response_line } => {
                let resp = parse_response(&response_line);
                let outcome = &resp["result"]["outcome"];
                assert_eq!(outcome["optionId"], "reject", "Agent B should auto-reject high-risk");
            }
            other => panic!("expected Respond, got {other:?}"),
        }
    }

    // ── terminal/* ─────────────────────────────────────────────────────

    #[test]
    fn acp_terminal_create_returns_method_not_found() {
        let delegate = NoFsDelegate;
        let request_id = RequestId::Number(20);
        let request = AgentRequest::CreateTerminalRequest(
            agent_client_protocol_schema::CreateTerminalRequest::new(
                SessionId::new("sess-1"),
                "rm -rf /",
            ),
        );

        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::PromptAlways,
            &delegate,
        );

        match result {
            HandleResult::Respond { response_line } => {
                let resp = parse_response(&response_line);
                assert_eq!(resp["id"], 20);
                assert_eq!(resp["error"]["code"], -32601);
                let msg = resp["error"]["message"].as_str().unwrap();
                assert!(
                    msg.contains("terminal/create"),
                    "error should mention the method: {msg}"
                );
            }
            other => panic!("expected Respond, got {other:?}"),
        }
    }

    #[test]
    fn acp_terminal_output_returns_method_not_found() {
        let delegate = NoFsDelegate;
        let request_id = RequestId::Number(21);
        let request = AgentRequest::TerminalOutputRequest(
            agent_client_protocol_schema::TerminalOutputRequest::new(
                SessionId::new("sess-1"),
                agent_client_protocol_schema::TerminalId::new("term-1"),
            ),
        );

        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::PromptAlways,
            &delegate,
        );

        match result {
            HandleResult::Respond { response_line } => {
                let resp = parse_response(&response_line);
                assert_eq!(resp["error"]["code"], -32601);
            }
            other => panic!("expected Respond, got {other:?}"),
        }
    }

    #[test]
    fn acp_terminal_release_returns_method_not_found() {
        let delegate = NoFsDelegate;
        let request_id = RequestId::Number(22);
        let request = AgentRequest::ReleaseTerminalRequest(
            agent_client_protocol_schema::ReleaseTerminalRequest::new(
                SessionId::new("sess-1"),
                agent_client_protocol_schema::TerminalId::new("term-1"),
            ),
        );

        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::PromptAlways,
            &delegate,
        );

        match result {
            HandleResult::Respond { response_line } => {
                let resp = parse_response(&response_line);
                assert_eq!(resp["error"]["code"], -32601);
            }
            other => panic!("expected Respond, got {other:?}"),
        }
    }

    #[test]
    fn acp_terminal_kill_returns_method_not_found() {
        let delegate = NoFsDelegate;
        let request_id = RequestId::Number(23);
        let request = AgentRequest::KillTerminalRequest(
            agent_client_protocol_schema::KillTerminalRequest::new(
                SessionId::new("sess-1"),
                agent_client_protocol_schema::TerminalId::new("term-1"),
            ),
        );

        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::PromptAlways,
            &delegate,
        );

        match result {
            HandleResult::Respond { response_line } => {
                let resp = parse_response(&response_line);
                assert_eq!(resp["error"]["code"], -32601);
            }
            other => panic!("expected Respond, got {other:?}"),
        }
    }

    #[test]
    fn acp_terminal_wait_for_exit_returns_method_not_found() {
        let delegate = NoFsDelegate;
        let request_id = RequestId::Number(24);
        let request = AgentRequest::WaitForTerminalExitRequest(
            agent_client_protocol_schema::WaitForTerminalExitRequest::new(
                SessionId::new("sess-1"),
                agent_client_protocol_schema::TerminalId::new("term-1"),
            ),
        );

        let result = handle_agent_request(
            &request_id,
            &request,
            AgentPermissionPolicy::PromptAlways,
            &delegate,
        );

        match result {
            HandleResult::Respond { response_line } => {
                let resp = parse_response(&response_line);
                assert_eq!(resp["error"]["code"], -32601);
            }
            other => panic!("expected Respond, got {other:?}"),
        }
    }

    // ── user response to permission prompt ─────────────────────────────

    #[test]
    fn acp_permission_user_response_serializes() {
        let request_id = RequestId::Number(30);
        let option_id = PermissionOptionId::new("allow");
        let result = handle_permission_user_response(&request_id, &option_id);
        assert!(result.is_ok(), "should serialize: {result:?}");
        let line = result.unwrap();
        let resp = parse_response(&line);
        assert_eq!(resp["id"], 30);
        // The outcome field contains { outcome: "selected", optionId: "allow" }
        assert_eq!(resp["result"]["outcome"]["outcome"], "selected");
        assert_eq!(resp["result"]["outcome"]["optionId"], "allow");
    }
}
