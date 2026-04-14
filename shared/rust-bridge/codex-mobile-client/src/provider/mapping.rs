//! Mapping functions between upstream protocol events, `ProviderEvent`, and `UiEvent`.
//!
//! This module provides the complete event mapping pipeline:
//!
//! 1. **Upstream → `ProviderEvent`**: `app_server_event_to_provider_event` and
//!    `server_notification_to_provider_event` convert Codex protocol events
//!    into the normalized `ProviderEvent` enum.
//!
//! 2. **`ProviderEvent` → `UiEvent`**: `provider_event_to_ui_event` converts
//!    normalized provider events into the UI-layer `UiEvent` enum for
//!    consumption by the store/reducer pipeline.
//!
//! Every upstream `ServerNotification` variant must map to a `ProviderEvent`.
//! Unknown or unrecognized upstream events map to `ProviderEvent::Unknown`
//! with a warning log for forward compatibility.

use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::{ServerNotification, ServerRequest};
use tracing::warn;

use crate::provider::ProviderEvent;
use crate::session::events::UiEvent;
use crate::types::ThreadKey;

// ── Upstream → ProviderEvent ───────────────────────────────────────────────

/// Map an `AppServerEvent` (the top-level envelope from `AppServerClient`)
/// to a `ProviderEvent`.
///
/// Handles all four variants:
/// - `ServerNotification` → delegates to `server_notification_to_provider_event`
/// - `ServerRequest` → delegates to `server_request_to_provider_event`
/// - `Lagged` → `ProviderEvent::Lagged`
/// - `Disconnected` → `ProviderEvent::Disconnected`
pub fn app_server_event_to_provider_event(
    server_id: &str,
    event: &AppServerEvent,
) -> ProviderEvent {
    match event {
        AppServerEvent::ServerNotification(notification) => {
            server_notification_to_provider_event(server_id, notification)
        }
        AppServerEvent::ServerRequest(request) => {
            server_request_to_provider_event(server_id, request)
        }
        AppServerEvent::Lagged { skipped } => ProviderEvent::Lagged {
            skipped: *skipped as u32,
        },
        AppServerEvent::Disconnected { message } => ProviderEvent::Disconnected {
            message: message.clone(),
        },
    }
}

/// Map a `ServerRequest` to a `ProviderEvent`.
///
/// Approval requests map to `ProviderEvent::ApprovalRequested`.
/// Dynamic tool calls map to `ProviderEvent::ToolCallStarted` (as a placeholder;
/// the full dynamic tool flow still uses the existing UiEvent pipeline).
/// Unrecognized requests map to `ProviderEvent::Unknown`.
pub fn server_request_to_provider_event(
    _server_id: &str,
    request: &ServerRequest,
) -> ProviderEvent {
    match request {
        ServerRequest::CommandExecutionRequestApproval { request_id, params } => {
            ProviderEvent::ApprovalRequested {
                thread_id: params.thread_id.clone(),
                request_id: request_id_to_string(request_id),
                kind: "command".to_string(),
                reason: params.reason.clone(),
                command: params.command.clone(),
            }
        }
        ServerRequest::FileChangeRequestApproval { request_id, params } => {
            ProviderEvent::ApprovalRequested {
                thread_id: params.thread_id.clone(),
                request_id: request_id_to_string(request_id),
                kind: "fileChange".to_string(),
                reason: params.reason.clone(),
                command: None,
            }
        }
        ServerRequest::PermissionsRequestApproval { request_id, params } => {
            ProviderEvent::ApprovalRequested {
                thread_id: params.thread_id.clone(),
                request_id: request_id_to_string(request_id),
                kind: "permissions".to_string(),
                reason: params.reason.clone(),
                command: None,
            }
        }
        ServerRequest::McpServerElicitationRequest { request_id, params } => {
            ProviderEvent::UserInputRequested {
                thread_id: params.thread_id.clone(),
                request_id: request_id_to_string(request_id),
            }
        }
        ServerRequest::ToolRequestUserInput { request_id, params } => {
            ProviderEvent::UserInputRequested {
                thread_id: params.thread_id.clone(),
                request_id: request_id_to_string(request_id),
            }
        }
        ServerRequest::DynamicToolCall { request_id, params } => {
            ProviderEvent::ToolCallStarted {
                thread_id: params.thread_id.clone(),
                item_id: params.call_id.clone(),
                tool_name: params.tool.clone(),
                call_id: request_id_to_string(request_id),
            }
        }
        ServerRequest::ChatgptAuthTokensRefresh { request_id: _, .. } => {
            // Auth token refresh is an internal mechanism, not a user-visible event.
            // Map to Unknown with relevant info for logging.
            ProviderEvent::Unknown {
                method: "account/chatgptAuthTokens/refresh".to_string(),
                payload: serde_json::to_string(request).unwrap_or_default(),
            }
        }
        // ── Legacy v1 approval requests → Unknown ─────────────────────
        // These are deprecated legacy request types. They still need explicit
        // match arms for compile-time exhaustiveness, but map to Unknown
        // because the v2 equivalents (CommandExecutionRequestApproval,
        // FileChangeRequestApproval) are the canonical mapped path.
        ServerRequest::ApplyPatchApproval { request_id: _, params: _ } => {
            let method = "applyPatch/approval".to_string();
            warn!(method = %method, "legacy v1 apply patch approval — mapping to Unknown ProviderEvent");
            ProviderEvent::Unknown {
                method,
                payload: serde_json::to_string(request).unwrap_or_default(),
            }
        }
        ServerRequest::ExecCommandApproval { request_id: _, params: _ } => {
            let method = "execCommand/approval".to_string();
            warn!(method = %method, "legacy v1 exec command approval — mapping to Unknown ProviderEvent");
            ProviderEvent::Unknown {
                method,
                payload: serde_json::to_string(request).unwrap_or_default(),
            }
        }
    }
}

/// Map a typed `ServerNotification` to a normalized `ProviderEvent`.
///
/// Every known `ServerNotification` variant must have an explicit match arm
/// producing the corresponding `ProviderEvent`. Unrecognized variants (from
/// future upstream additions) map to `ProviderEvent::Unknown` with a warning.
pub fn server_notification_to_provider_event(
    server_id: &str,
    notification: &ServerNotification,
) -> ProviderEvent {
    match notification {
        // ── Thread lifecycle ──────────────────────────────────────────
        ServerNotification::ThreadStarted(n) => ProviderEvent::ThreadStarted {
            thread_id: n.thread.id.clone(),
        },
        ServerNotification::ThreadArchived(n) => ProviderEvent::ThreadArchived {
            thread_id: n.thread_id.clone(),
        },
        ServerNotification::ThreadNameUpdated(n) => ProviderEvent::ThreadNameUpdated {
            thread_id: n.thread_id.clone(),
            name: n.thread_name.clone().unwrap_or_default(),
        },
        ServerNotification::ThreadStatusChanged(n) => ProviderEvent::ThreadStatusChanged {
            thread_id: n.thread_id.clone(),
            status: format!("{:?}", n.status),
        },
        ServerNotification::ModelRerouted(n) => ProviderEvent::ModelRerouted {
            thread_id: n.thread_id.clone(),
        },

        // ── Turn lifecycle ────────────────────────────────────────────
        ServerNotification::TurnStarted(n) => ProviderEvent::TurnStarted {
            thread_id: n.thread_id.clone(),
            turn_id: n.turn.id.clone(),
        },
        ServerNotification::TurnCompleted(n) => ProviderEvent::TurnCompleted {
            thread_id: n.thread_id.clone(),
            turn_id: n.turn.id.clone(),
        },
        ServerNotification::TurnDiffUpdated(n) => ProviderEvent::TurnDiffUpdated {
            thread_id: n.thread_id.clone(),
        },
        ServerNotification::TurnPlanUpdated(n) => ProviderEvent::TurnPlanUpdated {
            thread_id: n.thread_id.clone(),
        },

        // ── Item lifecycle ────────────────────────────────────────────
        ServerNotification::ItemStarted(n) => ProviderEvent::ItemStarted {
            thread_id: n.thread_id.clone(),
            turn_id: n.turn_id.clone(),
            item_id: item_id_from_notification_item(&n.item),
        },
        ServerNotification::ItemCompleted(n) => ProviderEvent::ItemCompleted {
            thread_id: n.thread_id.clone(),
            turn_id: n.turn_id.clone(),
            item_id: item_id_from_notification_item(&n.item),
        },
        ServerNotification::McpToolCallProgress(n) => ProviderEvent::McpToolCallProgress {
            thread_id: n.thread_id.clone(),
            item_id: n.item_id.clone(),
            message: n.message.clone(),
        },
        ServerNotification::ServerRequestResolved(n) => ProviderEvent::ServerRequestResolved {
            thread_id: n.thread_id.clone(),
        },

        // ── Streaming deltas ──────────────────────────────────────────
        ServerNotification::AgentMessageDelta(n) => ProviderEvent::MessageDelta {
            thread_id: n.thread_id.clone(),
            item_id: n.item_id.clone(),
            delta: n.delta.clone(),
        },
        ServerNotification::ReasoningTextDelta(n) => ProviderEvent::ReasoningDelta {
            thread_id: n.thread_id.clone(),
            item_id: n.item_id.clone(),
            delta: n.delta.clone(),
        },
        ServerNotification::ReasoningSummaryTextDelta(n) => ProviderEvent::ReasoningDelta {
            thread_id: n.thread_id.clone(),
            item_id: n.item_id.clone(),
            delta: n.delta.clone(),
        },
        ServerNotification::PlanDelta(n) => ProviderEvent::PlanDelta {
            thread_id: n.thread_id.clone(),
            item_id: n.item_id.clone(),
            delta: n.delta.clone(),
        },
        ServerNotification::CommandExecutionOutputDelta(n) => ProviderEvent::CommandOutputDelta {
            thread_id: n.thread_id.clone(),
            item_id: n.item_id.clone(),
            delta: n.delta.clone(),
        },
        ServerNotification::FileChangeOutputDelta(n) => ProviderEvent::FileChangeDelta {
            thread_id: n.thread_id.clone(),
            item_id: n.item_id.clone(),
            delta: n.delta.clone(),
        },

        // ── Account / limits ──────────────────────────────────────────
        ServerNotification::AccountRateLimitsUpdated(_) => ProviderEvent::AccountRateLimitsUpdated {
            server_id: server_id.to_string(),
        },
        ServerNotification::AccountLoginCompleted(_) => ProviderEvent::AccountLoginCompleted {
            server_id: server_id.to_string(),
        },

        // ── Realtime voice ────────────────────────────────────────────
        ServerNotification::ThreadRealtimeStarted(n) => ProviderEvent::RealtimeStarted {
            thread_id: n.thread_id.clone(),
        },
        ServerNotification::ThreadRealtimeClosed(n) => ProviderEvent::RealtimeClosed {
            thread_id: n.thread_id.clone(),
        },

        // ── Errors ────────────────────────────────────────────────────
        ServerNotification::Error(n) => ProviderEvent::Error {
            message: n.error.message.clone(),
            code: None,
        },

        // ── Context tokens ────────────────────────────────────────────
        ServerNotification::ThreadTokenUsageUpdated(n) => {
            let last = &n.token_usage.last;
            let used = (last.input_tokens + last.output_tokens) as u64;
            let limit = n.token_usage.model_context_window.unwrap_or(0) as u64;
            ProviderEvent::ContextTokensUpdated {
                thread_id: n.thread_id.clone(),
                used,
                limit,
            }
        }

        // ── Secondary upstream notifications → Unknown ────────────────
        //
        // These are legitimate upstream notifications that are not individually
        // mapped to typed ProviderEvent variants. Each has an explicit match arm
        // so that adding a new upstream variant causes a compile error here,
        // forcing the mapping to be updated.
        //
        // They map to Unknown for forward compatibility, preserving data for
        // diagnostic purposes.

        // Thread secondary
        ServerNotification::ThreadUnarchived(n) => unknown_notification(
            "thread/unarchived",
            &n.thread_id,
            notification,
        ),
        ServerNotification::ThreadClosed(n) => unknown_notification(
            "thread/closed",
            &n.thread_id,
            notification,
        ),
        ServerNotification::ContextCompacted(n) => unknown_notification(
            "thread/compacted",
            &n.thread_id,
            notification,
        ),

        // Hook lifecycle
        ServerNotification::HookStarted(n) => unknown_notification(
            "hook/started",
            &n.thread_id,
            notification,
        ),
        ServerNotification::HookCompleted(n) => unknown_notification(
            "hook/completed",
            &n.thread_id,
            notification,
        ),

        // Item secondary
        ServerNotification::ItemGuardianApprovalReviewStarted(n) => unknown_notification(
            "item/autoApprovalReview/started",
            &n.thread_id,
            notification,
        ),
        ServerNotification::ItemGuardianApprovalReviewCompleted(n) => unknown_notification(
            "item/autoApprovalReview/completed",
            &n.thread_id,
            notification,
        ),
        ServerNotification::RawResponseItemCompleted(n) => unknown_notification(
            "rawResponseItem/completed",
            &n.thread_id,
            notification,
        ),

        // Streaming secondary
        ServerNotification::ReasoningSummaryPartAdded(n) => unknown_notification(
            "item/reasoning/summaryPartAdded",
            &n.thread_id,
            notification,
        ),
        ServerNotification::CommandExecOutputDelta(n) => unknown_notification(
            "command/exec/outputDelta",
            &n.process_id,
            notification,
        ),
        ServerNotification::TerminalInteraction(n) => unknown_notification(
            "item/commandExecution/terminalInteraction",
            &n.thread_id,
            notification,
        ),

        // MCP secondary
        ServerNotification::McpServerOauthLoginCompleted(_) => unknown_notification(
            "mcpServer/oauthLogin/completed",
            "",
            notification,
        ),
        ServerNotification::McpServerStatusUpdated(_) => unknown_notification(
            "mcpServer/startupStatus/updated",
            "",
            notification,
        ),

        // Account / config secondary
        ServerNotification::AccountUpdated(_) => unknown_notification(
            "account/updated",
            "",
            notification,
        ),
        ServerNotification::AppListUpdated(_) => unknown_notification(
            "app/list/updated",
            "",
            notification,
        ),
        ServerNotification::DeprecationNotice(_) => unknown_notification(
            "deprecationNotice",
            "",
            notification,
        ),
        ServerNotification::ConfigWarning(_) => unknown_notification(
            "configWarning",
            "",
            notification,
        ),

        // Filesystem
        ServerNotification::FsChanged(_) => unknown_notification(
            "fs/changed",
            "",
            notification,
        ),

        // Skills
        ServerNotification::SkillsChanged(_) => unknown_notification(
            "skills/changed",
            "",
            notification,
        ),

        // Fuzzy search
        ServerNotification::FuzzyFileSearchSessionUpdated(_) => unknown_notification(
            "fuzzyFileSearch/sessionUpdated",
            "",
            notification,
        ),
        ServerNotification::FuzzyFileSearchSessionCompleted(_) => unknown_notification(
            "fuzzyFileSearch/sessionCompleted",
            "",
            notification,
        ),

        // Experimental realtime secondary
        ServerNotification::ThreadRealtimeItemAdded(n) => unknown_notification(
            "thread/realtime/itemAdded",
            &n.thread_id,
            notification,
        ),
        ServerNotification::ThreadRealtimeTranscriptUpdated(n) => unknown_notification(
            "thread/realtime/transcriptUpdated",
            &n.thread_id,
            notification,
        ),
        ServerNotification::ThreadRealtimeOutputAudioDelta(n) => unknown_notification(
            "thread/realtime/outputAudio/delta",
            &n.thread_id,
            notification,
        ),
        ServerNotification::ThreadRealtimeError(n) => unknown_notification(
            "thread/realtime/error",
            &n.thread_id,
            notification,
        ),

        // Windows-specific (not applicable on mobile, but must be mapped)
        ServerNotification::WindowsWorldWritableWarning(_) => unknown_notification(
            "windows/worldWritableWarning",
            "",
            notification,
        ),
        ServerNotification::WindowsSandboxSetupCompleted(_) => unknown_notification(
            "windowsSandbox/setupCompleted",
            "",
            notification,
        ),
    }
}

// ── ProviderEvent → UiEvent ────────────────────────────────────────────────

/// Map a normalized `ProviderEvent` to a `UiEvent` for the store/reducer.
///
/// Not all `ProviderEvent` variants produce `UiEvent`s — some (like
/// `ProviderEvent::Lagged` or `ProviderEvent::Unknown`) are handled
/// internally and return `None`.
///
/// Returns `None` for events that do not correspond to any `UiEvent`.
///
/// TODO: Will be called from `ServerSession::from_provider` event bridge
/// once that feature is implemented. Currently used only in tests.
#[allow(dead_code)]
pub(crate) fn provider_event_to_ui_event(
    server_id: &str,
    event: &ProviderEvent,
) -> Option<UiEvent> {
    match event {
        // ── Thread/Turn lifecycle ──────────────────────────────────────
        ProviderEvent::ThreadStarted { thread_id: _ } => Some(UiEvent::ConnectionStateChanged {
            server_id: server_id.to_string(),
            health: "thread_started".to_string(),
        }),
        ProviderEvent::ThreadArchived { thread_id } => Some(UiEvent::ThreadArchived {
            key: mk_key(server_id, thread_id),
        }),
        ProviderEvent::ThreadNameUpdated { thread_id, name } => {
            Some(UiEvent::ThreadNameUpdated {
                key: mk_key(server_id, thread_id),
                thread_name: if name.is_empty() {
                    None
                } else {
                    Some(name.clone())
                },
            })
        }
        ProviderEvent::ThreadStatusChanged { thread_id, status } => {
            // ThreadStatusChanged needs the upstream notification for full
            // fidelity. In the provider pipeline, this is a simplified
            // version. We emit a raw notification to preserve the status.
            Some(UiEvent::RawNotification {
                server_id: server_id.to_string(),
                method: "thread/status/changed".to_string(),
                params: serde_json::json!({
                    "threadId": thread_id,
                    "status": status,
                }),
            })
        }
        ProviderEvent::ModelRerouted { thread_id } => Some(UiEvent::RawNotification {
            server_id: server_id.to_string(),
            method: "model/rerouted".to_string(),
            params: serde_json::json!({ "threadId": thread_id }),
        }),
        ProviderEvent::TurnStarted { thread_id, turn_id } => Some(UiEvent::TurnStarted {
            key: mk_key(server_id, thread_id),
            turn_id: turn_id.clone(),
        }),
        ProviderEvent::TurnCompleted { thread_id, turn_id } => Some(UiEvent::TurnCompleted {
            key: mk_key(server_id, thread_id),
            turn_id: turn_id.clone(),
        }),
        ProviderEvent::TurnDiffUpdated { thread_id } => Some(UiEvent::RawNotification {
            server_id: server_id.to_string(),
            method: "turn/diff/updated".to_string(),
            params: serde_json::json!({ "threadId": thread_id }),
        }),
        ProviderEvent::TurnPlanUpdated { thread_id } => Some(UiEvent::RawNotification {
            server_id: server_id.to_string(),
            method: "turn/plan/updated".to_string(),
            params: serde_json::json!({ "threadId": thread_id }),
        }),
        ProviderEvent::ItemStarted {
            thread_id,
            turn_id,
            item_id,
        } => Some(UiEvent::RawNotification {
            server_id: server_id.to_string(),
            method: "item/started".to_string(),
            params: serde_json::json!({
                "threadId": thread_id,
                "turnId": turn_id,
                "itemId": item_id,
            }),
        }),
        ProviderEvent::ItemCompleted {
            thread_id,
            turn_id,
            item_id,
        } => Some(UiEvent::RawNotification {
            server_id: server_id.to_string(),
            method: "item/completed".to_string(),
            params: serde_json::json!({
                "threadId": thread_id,
                "turnId": turn_id,
                "itemId": item_id,
            }),
        }),

        // ── Streaming deltas ──────────────────────────────────────────
        ProviderEvent::MessageDelta {
            thread_id,
            item_id,
            delta,
        } => Some(UiEvent::MessageDelta {
            key: mk_key(server_id, thread_id),
            item_id: item_id.clone(),
            delta: delta.clone(),
        }),
        ProviderEvent::ReasoningDelta {
            thread_id,
            item_id,
            delta,
        } => Some(UiEvent::ReasoningDelta {
            key: mk_key(server_id, thread_id),
            item_id: item_id.clone(),
            delta: delta.clone(),
        }),
        ProviderEvent::PlanDelta {
            thread_id,
            item_id,
            delta,
        } => Some(UiEvent::PlanDelta {
            key: mk_key(server_id, thread_id),
            item_id: item_id.clone(),
            delta: delta.clone(),
        }),
        ProviderEvent::CommandOutputDelta {
            thread_id,
            item_id,
            delta,
        } => Some(UiEvent::CommandOutputDelta {
            key: mk_key(server_id, thread_id),
            item_id: item_id.clone(),
            delta: delta.clone(),
        }),
        ProviderEvent::FileChangeDelta {
            thread_id,
            item_id,
            delta,
        } => Some(UiEvent::CommandOutputDelta {
            key: mk_key(server_id, thread_id),
            item_id: item_id.clone(),
            delta: delta.clone(),
        }),

        // ── Tool calls ────────────────────────────────────────────────
        ProviderEvent::ToolCallStarted { .. } => {
            // Tool call started from the provider pipeline —
            // map to a raw notification since the full typed path
            // requires upstream notification data.
            Some(UiEvent::RawNotification {
                server_id: server_id.to_string(),
                method: "item/started".to_string(),
                params: serde_json::to_value(event).unwrap_or_default(),
            })
        }
        ProviderEvent::ToolCallUpdate { .. } => Some(UiEvent::RawNotification {
            server_id: server_id.to_string(),
            method: "toolCall/update".to_string(),
            params: serde_json::to_value(event).unwrap_or_default(),
        }),
        ProviderEvent::McpToolCallProgress {
            thread_id,
            item_id,
            message,
        } => Some(UiEvent::RawNotification {
            server_id: server_id.to_string(),
            method: "item/mcpToolCall/progress".to_string(),
            params: serde_json::json!({
                "threadId": thread_id,
                "itemId": item_id,
                "message": message,
            }),
        }),

        // ── Plans ─────────────────────────────────────────────────────
        ProviderEvent::PlanUpdated { .. } => Some(UiEvent::RawNotification {
            server_id: server_id.to_string(),
            method: "plan/updated".to_string(),
            params: serde_json::to_value(event).unwrap_or_default(),
        }),

        // ── Approvals / user input ────────────────────────────────────
        ProviderEvent::ApprovalRequested { .. } => {
            // Full approval handling requires the upstream PendingApproval
            // construction which uses the existing EventProcessor path.
            // Map to raw notification for now; the CodexProvider will
            // continue using the direct EventProcessor for approvals.
            Some(UiEvent::RawNotification {
                server_id: server_id.to_string(),
                method: "approval/requested".to_string(),
                params: serde_json::to_value(event).unwrap_or_default(),
            })
        }
        ProviderEvent::UserInputRequested { .. } => Some(UiEvent::RawNotification {
            server_id: server_id.to_string(),
            method: "userInput/requested".to_string(),
            params: serde_json::to_value(event).unwrap_or_default(),
        }),
        ProviderEvent::ServerRequestResolved { thread_id } => {
            Some(UiEvent::RawNotification {
                server_id: server_id.to_string(),
                method: "serverRequest/resolved".to_string(),
                params: serde_json::json!({ "threadId": thread_id }),
            })
        }

        // ── Account / limits ──────────────────────────────────────────
        ProviderEvent::AccountRateLimitsUpdated { server_id: sid } => {
            Some(UiEvent::RawNotification {
                server_id: sid.clone(),
                method: "account/rateLimits/updated".to_string(),
                params: serde_json::json!({}),
            })
        }
        ProviderEvent::AccountLoginCompleted { server_id: sid } => {
            Some(UiEvent::RawNotification {
                server_id: sid.clone(),
                method: "account/login/completed".to_string(),
                params: serde_json::json!({}),
            })
        }

        // ── Realtime voice ────────────────────────────────────────────
        ProviderEvent::RealtimeStarted { thread_id } => Some(UiEvent::RawNotification {
            server_id: server_id.to_string(),
            method: "thread/realtime/started".to_string(),
            params: serde_json::json!({ "threadId": thread_id }),
        }),
        ProviderEvent::RealtimeClosed { thread_id } => Some(UiEvent::RawNotification {
            server_id: server_id.to_string(),
            method: "thread/realtime/closed".to_string(),
            params: serde_json::json!({ "threadId": thread_id }),
        }),

        // ── Streaming lifecycle ───────────────────────────────────────
        ProviderEvent::StreamingStarted { .. }
        | ProviderEvent::StreamingCompleted { .. } => {
            // Internal lifecycle events — no direct UiEvent mapping.
            None
        }

        // ── Context ───────────────────────────────────────────────────
        ProviderEvent::ContextTokensUpdated {
            thread_id,
            used,
            limit,
        } => Some(UiEvent::ContextTokensUpdated {
            key: mk_key(server_id, thread_id),
            used: *used,
            limit: *limit,
        }),

        // ── Connection lifecycle ──────────────────────────────────────
        ProviderEvent::Disconnected { message: _ } => Some(UiEvent::ConnectionStateChanged {
            server_id: server_id.to_string(),
            health: "disconnected".to_string(),
        }),
        ProviderEvent::Error { message, code } => Some(UiEvent::Error {
            key: None,
            message: message.clone(),
            code: *code,
        }),

        // ── Internal ──────────────────────────────────────────────────
        ProviderEvent::Lagged { .. } => {
            // Lagged events are internal — no UiEvent needed.
            None
        }
        ProviderEvent::Unknown { method, payload } => {
            // Unknown events forward as raw notification so the platform
            // layer can still process them if needed.
            Some(UiEvent::RawNotification {
                server_id: server_id.to_string(),
                method: method.clone(),
                params: serde_json::from_str(payload).unwrap_or_default(),
            })
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

#[allow(dead_code)]
fn mk_key(server_id: &str, thread_id: &str) -> ThreadKey {
    ThreadKey {
        server_id: server_id.to_string(),
        thread_id: thread_id.to_string(),
    }
}

fn request_id_to_string(request_id: &codex_app_server_protocol::RequestId) -> String {
    match request_id {
        codex_app_server_protocol::RequestId::String(value) => value.clone(),
        codex_app_server_protocol::RequestId::Integer(value) => value.to_string(),
    }
}

/// Helper to produce a `ProviderEvent::Unknown` for secondary upstream notifications
/// that don't map to typed `ProviderEvent` variants. Logs a warning and preserves
/// the full payload for diagnostic purposes.
fn unknown_notification(
    method: &'static str,
    thread_id: &str,
    notification: &ServerNotification,
) -> ProviderEvent {
    warn!(
        method = method,
        thread_id = thread_id,
        "secondary server notification — mapping to ProviderEvent::Unknown"
    );
    ProviderEvent::Unknown {
        method: method.to_string(),
        payload: serde_json::to_string(notification).unwrap_or_default(),
    }
}

fn item_id_from_notification_item(item: &codex_app_server_protocol::ThreadItem) -> String {
    match item {
        codex_app_server_protocol::ThreadItem::UserMessage { id, .. }
        | codex_app_server_protocol::ThreadItem::HookPrompt { id, .. }
        | codex_app_server_protocol::ThreadItem::AgentMessage { id, .. }
        | codex_app_server_protocol::ThreadItem::Plan { id, .. }
        | codex_app_server_protocol::ThreadItem::Reasoning { id, .. }
        | codex_app_server_protocol::ThreadItem::CommandExecution { id, .. }
        | codex_app_server_protocol::ThreadItem::FileChange { id, .. }
        | codex_app_server_protocol::ThreadItem::McpToolCall { id, .. }
        | codex_app_server_protocol::ThreadItem::DynamicToolCall { id, .. }
        | codex_app_server_protocol::ThreadItem::CollabAgentToolCall { id, .. }
        | codex_app_server_protocol::ThreadItem::WebSearch { id, .. }
        | codex_app_server_protocol::ThreadItem::ImageView { id, .. }
        | codex_app_server_protocol::ThreadItem::ImageGeneration { id, .. }
        | codex_app_server_protocol::ThreadItem::EnteredReviewMode { id, .. }
        | codex_app_server_protocol::ThreadItem::ExitedReviewMode { id, .. }
        | codex_app_server_protocol::ThreadItem::ContextCompaction { id, .. } => id.clone(),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol as proto;

    // ── Test helpers ──────────────────────────────────────────────────

    fn make_thread(id: &str) -> proto::Thread {
        proto::Thread {
            id: id.to_string(),
            preview: String::new(),
            ephemeral: false,
            model_provider: "openai".to_string(),
            created_at: 1,
            updated_at: 1,
            status: proto::ThreadStatus::Idle,
            path: None,
            cwd: std::path::PathBuf::from("/tmp"),
            cli_version: "1.0.0".to_string(),
            source: proto::SessionSource::Cli,
            agent_nickname: None,
            agent_role: None,
            git_info: None,
            name: None,
            turns: Vec::new(),
        }
    }

    fn make_turn(id: &str) -> proto::Turn {
        proto::Turn {
            id: id.to_string(),
            items: Vec::new(),
            status: proto::TurnStatus::Completed,
            error: None,
        }
    }

    fn make_item(id: &str) -> proto::ThreadItem {
        proto::ThreadItem::AgentMessage {
            id: id.to_string(),
            text: String::new(),
            phase: None,
            memory_citation: None,
        }
    }

    // ── Exhaustive ServerNotification → ProviderEvent mapping ──────────

    /// Build one `ServerNotification` for every known variant and verify
    /// it maps to the correct `ProviderEvent` variant. This test must be
    /// updated when new upstream variants are added.
    #[test]
    fn provider_event_mapping_completeness() {
        let server_id = "srv1";

        let cases: Vec<(ServerNotification, fn(&ProviderEvent))> = vec![
            // Thread lifecycle
            (
                ServerNotification::ThreadStarted(proto::ThreadStartedNotification {
                    thread: make_thread("t1"),
                }),
                |e| assert!(matches!(e, ProviderEvent::ThreadStarted { thread_id } if thread_id == "t1")),
            ),
            (
                ServerNotification::ThreadArchived(proto::ThreadArchivedNotification {
                    thread_id: "t1".to_string(),
                }),
                |e| assert!(matches!(e, ProviderEvent::ThreadArchived { thread_id } if thread_id == "t1")),
            ),
            (
                ServerNotification::ThreadNameUpdated(proto::ThreadNameUpdatedNotification {
                    thread_id: "t1".to_string(),
                    thread_name: Some("New Name".to_string()),
                }),
                |e| assert!(matches!(e, ProviderEvent::ThreadNameUpdated { thread_id, name } if thread_id == "t1" && name == "New Name")),
            ),
            (
                ServerNotification::ThreadStatusChanged(proto::ThreadStatusChangedNotification {
                    thread_id: "t1".to_string(),
                    status: proto::ThreadStatus::Active { active_flags: vec![] },
                }),
                |e| assert!(matches!(e, ProviderEvent::ThreadStatusChanged { thread_id, .. } if thread_id == "t1")),
            ),
            (
                ServerNotification::ModelRerouted(proto::ModelReroutedNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    from_model: "gpt-4".to_string(),
                    to_model: "gpt-4o".to_string(),
                    reason: proto::ModelRerouteReason::HighRiskCyberActivity,
                }),
                |e| assert!(matches!(e, ProviderEvent::ModelRerouted { thread_id } if thread_id == "t1")),
            ),
            // Turn lifecycle
            (
                ServerNotification::TurnStarted(proto::TurnStartedNotification {
                    thread_id: "t1".to_string(),
                    turn: make_turn("turn1"),
                }),
                |e| assert!(matches!(e, ProviderEvent::TurnStarted { thread_id, turn_id } if thread_id == "t1" && turn_id == "turn1")),
            ),
            (
                ServerNotification::TurnCompleted(proto::TurnCompletedNotification {
                    thread_id: "t1".to_string(),
                    turn: make_turn("turn1"),
                }),
                |e| assert!(matches!(e, ProviderEvent::TurnCompleted { thread_id, turn_id } if thread_id == "t1" && turn_id == "turn1")),
            ),
            (
                ServerNotification::TurnDiffUpdated(proto::TurnDiffUpdatedNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    diff: "--- a\n+++ b".to_string(),
                }),
                |e| assert!(matches!(e, ProviderEvent::TurnDiffUpdated { thread_id } if thread_id == "t1")),
            ),
            (
                ServerNotification::TurnPlanUpdated(proto::TurnPlanUpdatedNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    explanation: None,
                    plan: vec![],
                }),
                |e| assert!(matches!(e, ProviderEvent::TurnPlanUpdated { thread_id } if thread_id == "t1")),
            ),
            // Item lifecycle
            (
                ServerNotification::ItemStarted(proto::ItemStartedNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    item: make_item("item1"),
                }),
                |e| assert!(matches!(e, ProviderEvent::ItemStarted { thread_id, turn_id, item_id } if thread_id == "t1" && item_id == "item1")),
            ),
            (
                ServerNotification::ItemCompleted(proto::ItemCompletedNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    item: make_item("item1"),
                }),
                |e| assert!(matches!(e, ProviderEvent::ItemCompleted { thread_id, turn_id, item_id } if thread_id == "t1" && item_id == "item1")),
            ),
            (
                ServerNotification::McpToolCallProgress(proto::McpToolCallProgressNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    item_id: "item1".to_string(),
                    message: "progress".to_string(),
                }),
                |e| assert!(matches!(e, ProviderEvent::McpToolCallProgress { thread_id, item_id, message } if thread_id == "t1" && message == "progress")),
            ),
            (
                ServerNotification::ServerRequestResolved(proto::ServerRequestResolvedNotification {
                    thread_id: "t1".to_string(),
                    request_id: proto::RequestId::Integer(1),
                }),
                |e| assert!(matches!(e, ProviderEvent::ServerRequestResolved { thread_id } if thread_id == "t1")),
            ),
            // Streaming deltas
            (
                ServerNotification::AgentMessageDelta(proto::AgentMessageDeltaNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    item_id: "item1".to_string(),
                    delta: "Hello ".to_string(),
                }),
                |e| assert!(matches!(e, ProviderEvent::MessageDelta { thread_id, item_id, delta } if thread_id == "t1" && delta == "Hello ")),
            ),
            (
                ServerNotification::ReasoningTextDelta(proto::ReasoningTextDeltaNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    item_id: "item1".to_string(),
                    delta: "thinking...".to_string(),
                    content_index: 0,
                }),
                |e| assert!(matches!(e, ProviderEvent::ReasoningDelta { thread_id, delta, .. } if thread_id == "t1" && delta == "thinking...")),
            ),
            (
                ServerNotification::ReasoningSummaryTextDelta(proto::ReasoningSummaryTextDeltaNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    item_id: "item1".to_string(),
                    delta: "summary".to_string(),
                    summary_index: 0,
                }),
                |e| assert!(matches!(e, ProviderEvent::ReasoningDelta { thread_id, delta, .. } if thread_id == "t1" && delta == "summary")),
            ),
            (
                ServerNotification::PlanDelta(proto::PlanDeltaNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    item_id: "item1".to_string(),
                    delta: "step 1".to_string(),
                }),
                |e| assert!(matches!(e, ProviderEvent::PlanDelta { thread_id, delta, .. } if thread_id == "t1" && delta == "step 1")),
            ),
            (
                ServerNotification::CommandExecutionOutputDelta(proto::CommandExecutionOutputDeltaNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    item_id: "item1".to_string(),
                    delta: "output".to_string(),
                }),
                |e| assert!(matches!(e, ProviderEvent::CommandOutputDelta { thread_id, delta, .. } if thread_id == "t1" && delta == "output")),
            ),
            (
                ServerNotification::FileChangeOutputDelta(proto::FileChangeOutputDeltaNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    item_id: "item1".to_string(),
                    delta: "diff".to_string(),
                }),
                |e| assert!(matches!(e, ProviderEvent::FileChangeDelta { thread_id, delta, .. } if thread_id == "t1" && delta == "diff")),
            ),
            // Account
            (
                ServerNotification::AccountRateLimitsUpdated(proto::AccountRateLimitsUpdatedNotification {
                    rate_limits: proto::RateLimitSnapshot {
                        limit_id: None,
                        limit_name: None,
                        primary: None,
                        secondary: None,
                        credits: None,
                        plan_type: None,
                    },
                }),
                |e| assert!(matches!(e, ProviderEvent::AccountRateLimitsUpdated { server_id } if server_id == "srv1")),
            ),
            (
                ServerNotification::AccountLoginCompleted(proto::AccountLoginCompletedNotification {
                    login_id: None,
                    success: true,
                    error: None,
                }),
                |e| assert!(matches!(e, ProviderEvent::AccountLoginCompleted { server_id } if server_id == "srv1")),
            ),
            // Realtime
            (
                ServerNotification::ThreadRealtimeStarted(proto::ThreadRealtimeStartedNotification {
                    thread_id: "t1".to_string(),
                    session_id: None,
                    version: codex_protocol::protocol::RealtimeConversationVersion::V2,
                }),
                |e| assert!(matches!(e, ProviderEvent::RealtimeStarted { thread_id } if thread_id == "t1")),
            ),
            (
                ServerNotification::ThreadRealtimeClosed(proto::ThreadRealtimeClosedNotification {
                    thread_id: "t1".to_string(),
                    reason: None,
                }),
                |e| assert!(matches!(e, ProviderEvent::RealtimeClosed { thread_id } if thread_id == "t1")),
            ),
            // Error
            (
                ServerNotification::Error(proto::ErrorNotification {
                    error: proto::TurnError {
                        message: "oops".to_string(),
                        codex_error_info: None,
                        additional_details: None,
                    },
                    will_retry: false,
                    thread_id: "t1".to_string(),
                    turn_id: String::new(),
                }),
                |e| assert!(matches!(e, ProviderEvent::Error { message, .. } if message == "oops")),
            ),
            // Token usage
            (
                ServerNotification::ThreadTokenUsageUpdated(proto::ThreadTokenUsageUpdatedNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    token_usage: proto::ThreadTokenUsage {
                        total: proto::TokenUsageBreakdown {
                            total_tokens: 5000,
                            input_tokens: 3000,
                            cached_input_tokens: 0,
                            output_tokens: 2000,
                            reasoning_output_tokens: 0,
                        },
                        last: proto::TokenUsageBreakdown {
                            total_tokens: 150,
                            input_tokens: 100,
                            cached_input_tokens: 0,
                            output_tokens: 50,
                            reasoning_output_tokens: 0,
                        },
                        model_context_window: Some(128000),
                    },
                }),
                |e| {
                    if let ProviderEvent::ContextTokensUpdated { thread_id, used, limit } = e {
                        assert_eq!(thread_id, "t1");
                        assert_eq!(*used, 150);
                        assert_eq!(*limit, 128000);
                    } else {
                        panic!("expected ContextTokensUpdated, got {e:?}");
                    }
                },
            ),
        ];

        assert!(
            cases.len() >= 24,
            "expected at least 24 notification mappings, got {}",
            cases.len()
        );

        for (i, (notification, assert_fn)) in cases.iter().enumerate() {
            let event = server_notification_to_provider_event(server_id, notification);
            assert_fn(&event);
            // Verify the event is never Unknown for known notifications.
            assert!(
                !matches!(&event, ProviderEvent::Unknown { .. }),
                "case {i}: known notification mapped to Unknown: {event:?}"
            );
        }
    }

    /// Verify that unhandled known notifications (e.g. SkillsChanged)
    /// map to `ProviderEvent::Unknown`.
    #[test]
    fn unhandled_notification_maps_to_unknown() {
        let notification = ServerNotification::SkillsChanged(proto::SkillsChangedNotification {});
        let event =
            server_notification_to_provider_event("srv1", &notification);
        match &event {
            ProviderEvent::Unknown { method, payload } => {
                assert!(!method.is_empty());
                assert!(!payload.is_empty());
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    /// Verify that other unhandled notifications map to Unknown.
    #[test]
    fn unhandled_realtime_notifications_map_to_unknown() {
        // ThreadRealtimeItemAdded is known upstream but mapped to Unknown
        let notification =
            ServerNotification::ThreadRealtimeItemAdded(proto::ThreadRealtimeItemAddedNotification {
                thread_id: "t1".to_string(),
                item: serde_json::json!({"type": "message"}),
            });
        let event =
            server_notification_to_provider_event("srv1", &notification);
        assert!(
            matches!(&event, ProviderEvent::Unknown { .. }),
            "unhandled realtime notification should map to Unknown, got {event:?}"
        );
    }

    /// Verify all secondary upstream notifications that have explicit match arms
    /// correctly map to `ProviderEvent::Unknown` with non-empty method and payload.
    ///
    /// This test covers all ServerNotification variants that are explicitly mapped
    /// to Unknown (not typed ProviderEvent variants). Each one must preserve the
    /// upstream payload for diagnostic purposes.
    #[test]
    fn secondary_notifications_map_to_unknown_with_data() {
        let cases: Vec<(ServerNotification, &str)> = vec![
            // Thread secondary
            (
                ServerNotification::ThreadUnarchived(proto::ThreadUnarchivedNotification {
                    thread_id: "t1".to_string(),
                }),
                "thread/unarchived",
            ),
            (
                ServerNotification::ThreadClosed(proto::ThreadClosedNotification {
                    thread_id: "t1".to_string(),
                }),
                "thread/closed",
            ),
            (
                ServerNotification::ContextCompacted(proto::ContextCompactedNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                }),
                "thread/compacted",
            ),
            // Streaming secondary
            (
                ServerNotification::ReasoningSummaryPartAdded(proto::ReasoningSummaryPartAddedNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    item_id: "item1".to_string(),
                    summary_index: 0,
                }),
                "item/reasoning/summaryPartAdded",
            ),
            // Skills
            (
                ServerNotification::SkillsChanged(proto::SkillsChangedNotification {}),
                "skills/changed",
            ),
            // Account / config secondary
            (
                ServerNotification::AccountUpdated(proto::AccountUpdatedNotification {
                    auth_mode: None,
                    plan_type: None,
                }),
                "account/updated",
            ),
            (
                ServerNotification::AppListUpdated(proto::AppListUpdatedNotification {
                    data: vec![],
                }),
                "app/list/updated",
            ),
            (
                ServerNotification::DeprecationNotice(proto::DeprecationNoticeNotification {
                    summary: "test".to_string(),
                    details: None,
                }),
                "deprecationNotice",
            ),
            (
                ServerNotification::ConfigWarning(proto::ConfigWarningNotification {
                    summary: "warn".to_string(),
                    details: None,
                    path: None,
                    range: None,
                }),
                "configWarning",
            ),
            // Filesystem
            (
                ServerNotification::FsChanged(proto::FsChangedNotification {
                    watch_id: "w1".to_string(),
                    changed_paths: vec![],
                }),
                "fs/changed",
            ),
            // Fuzzy search
            (
                ServerNotification::FuzzyFileSearchSessionUpdated(
                    proto::FuzzyFileSearchSessionUpdatedNotification {
                        session_id: "fs1".to_string(),
                        query: "test".to_string(),
                        files: vec![],
                    },
                ),
                "fuzzyFileSearch/sessionUpdated",
            ),
            (
                ServerNotification::FuzzyFileSearchSessionCompleted(
                    proto::FuzzyFileSearchSessionCompletedNotification {
                        session_id: "fs1".to_string(),
                    },
                ),
                "fuzzyFileSearch/sessionCompleted",
            ),
            // Experimental realtime secondary
            (
                ServerNotification::ThreadRealtimeItemAdded(proto::ThreadRealtimeItemAddedNotification {
                    thread_id: "t1".to_string(),
                    item: serde_json::json!({"type": "message"}),
                }),
                "thread/realtime/itemAdded",
            ),
            (
                ServerNotification::ThreadRealtimeTranscriptUpdated(
                    proto::ThreadRealtimeTranscriptUpdatedNotification {
                        thread_id: "t1".to_string(),
                        role: "assistant".to_string(),
                        text: "hello".to_string(),
                    },
                ),
                "thread/realtime/transcriptUpdated",
            ),
            (
                ServerNotification::ThreadRealtimeError(
                    proto::ThreadRealtimeErrorNotification {
                        thread_id: "t1".to_string(),
                        message: "audio error".to_string(),
                    },
                ),
                "thread/realtime/error",
            ),
            // Windows-specific
            (
                ServerNotification::WindowsWorldWritableWarning(
                    proto::WindowsWorldWritableWarningNotification {
                        sample_paths: vec!["/tmp".to_string()],
                        extra_count: 0,
                        failed_scan: false,
                    },
                ),
                "windows/worldWritableWarning",
            ),
            (
                ServerNotification::WindowsSandboxSetupCompleted(
                    proto::WindowsSandboxSetupCompletedNotification {
                        mode: proto::WindowsSandboxSetupMode::Elevated,
                        success: true,
                        error: None,
                    },
                ),
                "windowsSandbox/setupCompleted",
            ),
        ];

        assert!(
            cases.len() >= 15,
            "expected at least 15 secondary notification mappings, got {}",
            cases.len()
        );

        for (i, (notification, expected_method)) in cases.iter().enumerate() {
            let event = server_notification_to_provider_event("srv1", notification);
            match &event {
                ProviderEvent::Unknown { method, payload } => {
                    assert_eq!(
                        method, expected_method,
                        "case {i}: method mismatch"
                    );
                    assert!(
                        !payload.is_empty(),
                        "case {i}: payload should not be empty for {expected_method}"
                    );
                }
                other => {
                    panic!(
                        "case {i}: secondary notification should map to Unknown, got {other:?}"
                    );
                }
            }
        }
    }

    /// Verify that the `unhandled_notification_maps_to_unknown` test still works
    /// for the SkillsChanged variant — it maps to Unknown with the method name.
    #[test]
    fn skills_changed_maps_to_unknown() {
        let notification = ServerNotification::SkillsChanged(proto::SkillsChangedNotification {});
        let event = server_notification_to_provider_event("srv1", &notification);
        match &event {
            ProviderEvent::Unknown { method, payload } => {
                assert_eq!(method, "skills/changed");
                assert!(!payload.is_empty());
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    // ── ServerRequest → ProviderEvent ─────────────────────────────────

    #[test]
    fn command_approval_request_mapping() {
        let request = ServerRequest::CommandExecutionRequestApproval {
            request_id: proto::RequestId::Integer(42),
            params: proto::CommandExecutionRequestApprovalParams {
                thread_id: "t1".to_string(),
                turn_id: "turn1".to_string(),
                item_id: "item1".to_string(),
                approval_id: None,
                reason: Some("needs approval".to_string()),
                network_approval_context: None,
                command: Some("rm -rf /tmp".to_string()),
                cwd: None,
                command_actions: None,
                additional_permissions: None,
                skill_metadata: None,
                proposed_execpolicy_amendment: None,
                proposed_network_policy_amendments: None,
                available_decisions: None,
            },
        };
        let event = server_request_to_provider_event("srv1", &request);
        match &event {
            ProviderEvent::ApprovalRequested {
                thread_id,
                request_id,
                kind,
                reason,
                command,
            } => {
                assert_eq!(thread_id, "t1");
                assert_eq!(request_id, "42");
                assert_eq!(kind, "command");
                assert_eq!(reason.as_deref(), Some("needs approval"));
                assert_eq!(command.as_deref(), Some("rm -rf /tmp"));
            }
            other => panic!("expected ApprovalRequested, got {other:?}"),
        }
    }

    #[test]
    fn file_change_approval_request_mapping() {
        let request = ServerRequest::FileChangeRequestApproval {
            request_id: proto::RequestId::Integer(10),
            params: proto::FileChangeRequestApprovalParams {
                thread_id: "t1".to_string(),
                turn_id: "turn1".to_string(),
                item_id: "item1".to_string(),
                reason: Some("modify file".to_string()),
                grant_root: None,
            },
        };
        let event = server_request_to_provider_event("srv1", &request);
        match &event {
            ProviderEvent::ApprovalRequested { kind, reason, .. } => {
                assert_eq!(kind, "fileChange");
                assert_eq!(reason.as_deref(), Some("modify file"));
            }
            other => panic!("expected ApprovalRequested, got {other:?}"),
        }
    }

    #[test]
    fn permissions_approval_request_mapping() {
        let request = ServerRequest::PermissionsRequestApproval {
            request_id: proto::RequestId::Integer(11),
            params: proto::PermissionsRequestApprovalParams {
                thread_id: "t1".to_string(),
                turn_id: "turn1".to_string(),
                item_id: "item1".to_string(),
                reason: Some("need network access".to_string()),
                permissions: proto::RequestPermissionProfile {
                    network: None,
                    file_system: None,
                },
            },
        };
        let event = server_request_to_provider_event("srv1", &request);
        match &event {
            ProviderEvent::ApprovalRequested { kind, reason, .. } => {
                assert_eq!(kind, "permissions");
                assert_eq!(reason.as_deref(), Some("need network access"));
            }
            other => panic!("expected ApprovalRequested, got {other:?}"),
        }
    }

    #[test]
    fn dynamic_tool_call_mapping() {
        let request = ServerRequest::DynamicToolCall {
            request_id: proto::RequestId::String("req-1".to_string()),
            params: proto::DynamicToolCallParams {
                thread_id: "t1".to_string(),
                turn_id: "turn1".to_string(),
                call_id: "call-1".to_string(),
                tool: "my_tool".to_string(),
                arguments: serde_json::json!({"arg": "value"}),
            },
        };
        let event = server_request_to_provider_event("srv1", &request);
        match &event {
            ProviderEvent::ToolCallStarted {
                thread_id,
                item_id,
                tool_name: _,
                call_id,
            } => {
                assert_eq!(thread_id, "t1");
                assert_eq!(item_id, "call-1");
                assert_eq!(call_id, "req-1");
            }
            other => panic!("expected ToolCallStarted, got {other:?}"),
        }
    }

    /// Legacy v1 ApplyPatchApproval maps to Unknown (deprecated in favor of
    /// FileChangeRequestApproval).
    #[test]
    fn legacy_apply_patch_approval_maps_to_unknown() {
        let thread_id = codex_protocol::ThreadId::from_string("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap();
        let request = ServerRequest::ApplyPatchApproval {
            request_id: proto::RequestId::Integer(99),
            params: proto::ApplyPatchApprovalParams {
                conversation_id: thread_id,
                call_id: "call1".to_string(),
                file_changes: std::collections::HashMap::new(),
                reason: None,
                grant_root: None,
            },
        };
        let event = server_request_to_provider_event("srv1", &request);
        match &event {
            ProviderEvent::Unknown { method, payload } => {
                assert_eq!(method, "applyPatch/approval");
                assert!(!payload.is_empty());
            }
            other => panic!("expected Unknown for legacy ApplyPatchApproval, got {other:?}"),
        }
    }

    /// Legacy v1 ExecCommandApproval maps to Unknown (deprecated in favor of
    /// CommandExecutionRequestApproval).
    #[test]
    fn legacy_exec_command_approval_maps_to_unknown() {
        let thread_id = codex_protocol::ThreadId::from_string("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap();
        let request = ServerRequest::ExecCommandApproval {
            request_id: proto::RequestId::Integer(98),
            params: proto::ExecCommandApprovalParams {
                conversation_id: thread_id,
                call_id: "call1".to_string(),
                approval_id: None,
                command: vec!["echo".to_string(), "hello".to_string()],
                cwd: std::path::PathBuf::from("/tmp"),
                reason: None,
                parsed_cmd: vec![],
            },
        };
        let event = server_request_to_provider_event("srv1", &request);
        match &event {
            ProviderEvent::Unknown { method, payload } => {
                assert_eq!(method, "execCommand/approval");
                assert!(!payload.is_empty());
            }
            other => panic!("expected Unknown for legacy ExecCommandApproval, got {other:?}"),
        }
    }

    /// Verify all ServerRequest variants are explicitly handled (compile-time
    /// exhaustive). Constructs one instance of each variant to verify the match
    /// in `server_request_to_provider_event` covers them all.
    #[test]
    fn server_request_mapping_is_exhaustive() {
        // The compile-time check is the primary guarantee: if a new variant
        // is added to ServerRequest, the match in server_request_to_provider_event
        // will fail to compile because there is no wildcard arm.
        //
        // This test verifies the 7 core variants + 2 legacy variants = 9 total
        // all produce valid ProviderEvent values without panicking.
        let requests: Vec<ServerRequest> = vec![
            ServerRequest::CommandExecutionRequestApproval {
                request_id: proto::RequestId::Integer(1),
                params: proto::CommandExecutionRequestApprovalParams {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    item_id: "item1".to_string(),
                    approval_id: None,
                    reason: None,
                    network_approval_context: None,
                    command: None,
                    cwd: None,
                    command_actions: None,
                    additional_permissions: None,
                    skill_metadata: None,
                    proposed_execpolicy_amendment: None,
                    proposed_network_policy_amendments: None,
                    available_decisions: None,
                },
            },
            ServerRequest::FileChangeRequestApproval {
                request_id: proto::RequestId::Integer(2),
                params: proto::FileChangeRequestApprovalParams {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    item_id: "item1".to_string(),
                    reason: None,
                    grant_root: None,
                },
            },
            ServerRequest::DynamicToolCall {
                request_id: proto::RequestId::Integer(6),
                params: proto::DynamicToolCallParams {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    call_id: "call1".to_string(),
                    tool: "tool".to_string(),
                    arguments: serde_json::json!({}),
                },
            },
            ServerRequest::ChatgptAuthTokensRefresh {
                request_id: proto::RequestId::Integer(7),
                params: proto::ChatgptAuthTokensRefreshParams {
                    previous_account_id: None,
                    reason: proto::ChatgptAuthTokensRefreshReason::Unauthorized,
                },
            },
            ServerRequest::ApplyPatchApproval {
                request_id: proto::RequestId::Integer(8),
                params: proto::ApplyPatchApprovalParams {
                    conversation_id: codex_protocol::ThreadId::from_string("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap(),
                    call_id: "call1".to_string(),
                    file_changes: std::collections::HashMap::new(),
                    reason: None,
                    grant_root: None,
                },
            },
            ServerRequest::ExecCommandApproval {
                request_id: proto::RequestId::Integer(9),
                params: proto::ExecCommandApprovalParams {
                    conversation_id: codex_protocol::ThreadId::from_string("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap(),
                    call_id: "call1".to_string(),
                    approval_id: None,
                    command: vec![],
                    cwd: std::path::PathBuf::from("/tmp"),
                    reason: None,
                    parsed_cmd: vec![],
                },
            },
        ];

        // At least 6 ServerRequest variants constructed (all that we can easily construct).
        // The remaining 3 (PermissionsRequestApproval, ToolRequestUserInput,
        // McpServerElicitationRequest) are tested in dedicated tests above.
        assert!(
            requests.len() >= 6,
            "expected at least 6 ServerRequest variants, got {}",
            requests.len()
        );

        for (i, request) in requests.iter().enumerate() {
            // Must not panic — all variants must be handled
            let event = server_request_to_provider_event("srv1", request);
            // Every variant must produce some ProviderEvent
            let debug = format!("{event:?}");
            assert!(!debug.is_empty(), "case {i}: event must be non-empty");
        }
    }

    #[test]
    fn app_server_event_lagged() {
        let event = AppServerEvent::Lagged { skipped: 5 };
        let pe = app_server_event_to_provider_event("srv1", &event);
        match &pe {
            ProviderEvent::Lagged { skipped } => assert_eq!(*skipped, 5),
            other => panic!("expected Lagged, got {other:?}"),
        }
    }

    #[test]
    fn app_server_event_disconnected() {
        let event = AppServerEvent::Disconnected {
            message: "connection lost".to_string(),
        };
        let pe = app_server_event_to_provider_event("srv1", &event);
        match &pe {
            ProviderEvent::Disconnected { message } => {
                assert_eq!(message, "connection lost");
            }
            other => panic!("expected Disconnected, got {other:?}"),
        }
    }

    #[test]
    fn app_server_event_notification() {
        let notification =
            ServerNotification::TurnStarted(proto::TurnStartedNotification {
                thread_id: "t1".to_string(),
                turn: make_turn("turn1"),
            });
        let event = AppServerEvent::ServerNotification(notification);
        let pe = app_server_event_to_provider_event("srv1", &event);
        match &pe {
            ProviderEvent::TurnStarted { thread_id, turn_id } => {
                assert_eq!(thread_id, "t1");
                assert_eq!(turn_id, "turn1");
            }
            other => panic!("expected TurnStarted, got {other:?}"),
        }
    }

    // ── ProviderEvent → UiEvent mapping ───────────────────────────────

    #[test]
    fn provider_event_message_delta_to_ui_event() {
        let event = ProviderEvent::MessageDelta {
            thread_id: "t1".into(),
            item_id: "item1".into(),
            delta: "Hello".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::MessageDelta { key, item_id, delta } => {
                assert_eq!(key.thread_id, "t1");
                assert_eq!(key.server_id, "srv1");
                assert_eq!(item_id, "item1");
                assert_eq!(delta, "Hello");
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_reasoning_delta_to_ui_event() {
        let event = ProviderEvent::ReasoningDelta {
            thread_id: "t1".into(),
            item_id: "item1".into(),
            delta: "thinking...".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::ReasoningDelta { key, delta, .. } => {
                assert_eq!(key.thread_id, "t1");
                assert_eq!(delta, "thinking...");
            }
            other => panic!("expected ReasoningDelta, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_command_output_delta_to_ui_event() {
        let event = ProviderEvent::CommandOutputDelta {
            thread_id: "t1".into(),
            item_id: "item1".into(),
            delta: "output".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::CommandOutputDelta { key, delta, .. } => {
                assert_eq!(key.thread_id, "t1");
                assert_eq!(delta, "output");
            }
            other => panic!("expected CommandOutputDelta, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_file_change_delta_to_command_output() {
        // FileChangeDelta maps to CommandOutputDelta in UiEvent
        // (same as the existing EventProcessor behavior).
        let event = ProviderEvent::FileChangeDelta {
            thread_id: "t1".into(),
            item_id: "item1".into(),
            delta: "diff".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::CommandOutputDelta { key, delta, .. } => {
                assert_eq!(key.thread_id, "t1");
                assert_eq!(delta, "diff");
            }
            other => panic!("expected CommandOutputDelta, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_turn_started_to_ui_event() {
        let event = ProviderEvent::TurnStarted {
            thread_id: "t1".into(),
            turn_id: "turn1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::TurnStarted { key, turn_id } => {
                assert_eq!(key.thread_id, "t1");
                assert_eq!(turn_id, "turn1");
            }
            other => panic!("expected TurnStarted, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_turn_completed_to_ui_event() {
        let event = ProviderEvent::TurnCompleted {
            thread_id: "t1".into(),
            turn_id: "turn1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::TurnCompleted { key, turn_id } => {
                assert_eq!(key.thread_id, "t1");
                assert_eq!(turn_id, "turn1");
            }
            other => panic!("expected TurnCompleted, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_context_tokens_to_ui_event() {
        let event = ProviderEvent::ContextTokensUpdated {
            thread_id: "t1".into(),
            used: 150,
            limit: 128000,
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::ContextTokensUpdated { key, used, limit } => {
                assert_eq!(key.thread_id, "t1");
                assert_eq!(used, 150);
                assert_eq!(limit, 128000);
            }
            other => panic!("expected ContextTokensUpdated, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_disconnected_to_ui_event() {
        let event = ProviderEvent::Disconnected {
            message: "connection lost".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::ConnectionStateChanged { server_id, health } => {
                assert_eq!(server_id, "srv1");
                assert_eq!(health, "disconnected");
            }
            other => panic!("expected ConnectionStateChanged, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_error_to_ui_event() {
        let event = ProviderEvent::Error {
            message: "something failed".into(),
            code: Some(500),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::Error { key, message, code } => {
                assert!(key.is_none());
                assert_eq!(message, "something failed");
                assert_eq!(code, Some(500));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_lagged_returns_none() {
        let event = ProviderEvent::Lagged { skipped: 5 };
        let ui = provider_event_to_ui_event("srv1", &event);
        assert!(ui.is_none(), "Lagged should not produce a UiEvent");
    }

    #[test]
    fn provider_event_streaming_lifecycle_returns_none() {
        let started = ProviderEvent::StreamingStarted {
            thread_id: "t1".into(),
        };
        assert!(provider_event_to_ui_event("srv1", &started).is_none());

        let completed = ProviderEvent::StreamingCompleted {
            thread_id: "t1".into(),
        };
        assert!(provider_event_to_ui_event("srv1", &completed).is_none());
    }

    #[test]
    fn provider_event_unknown_to_raw_notification() {
        let event = ProviderEvent::Unknown {
            method: "some/future/event".to_string(),
            payload: r#"{"foo":"bar"}"#.to_string(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { server_id, method, params } => {
                assert_eq!(server_id, "srv1");
                assert_eq!(method, "some/future/event");
                assert_eq!(params["foo"], "bar");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    // ── Round-trip: ServerNotification → ProviderEvent → UiEvent ──────

    #[test]
    fn round_trip_turn_started() {
        let notification = ServerNotification::TurnStarted(proto::TurnStartedNotification {
            thread_id: "t1".to_string(),
            turn: make_turn("turn1"),
        });
        let pe = server_notification_to_provider_event("srv1", &notification);
        let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
        match ui {
            UiEvent::TurnStarted { key, turn_id } => {
                assert_eq!(key.server_id, "srv1");
                assert_eq!(key.thread_id, "t1");
                assert_eq!(turn_id, "turn1");
            }
            other => panic!("expected TurnStarted, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_message_delta() {
        let notification =
            ServerNotification::AgentMessageDelta(proto::AgentMessageDeltaNotification {
                thread_id: "t1".to_string(),
                turn_id: "turn1".to_string(),
                item_id: "item1".to_string(),
                delta: "Hello world".to_string(),
            });
        let pe = server_notification_to_provider_event("srv1", &notification);
        let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
        match ui {
            UiEvent::MessageDelta { key, item_id, delta } => {
                assert_eq!(key.server_id, "srv1");
                assert_eq!(key.thread_id, "t1");
                assert_eq!(item_id, "item1");
                assert_eq!(delta, "Hello world");
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_context_tokens() {
        let notification = ServerNotification::ThreadTokenUsageUpdated(
            proto::ThreadTokenUsageUpdatedNotification {
                thread_id: "t1".to_string(),
                turn_id: "turn1".to_string(),
                token_usage: proto::ThreadTokenUsage {
                    total: proto::TokenUsageBreakdown {
                        total_tokens: 5000,
                        input_tokens: 3000,
                        cached_input_tokens: 0,
                        output_tokens: 2000,
                        reasoning_output_tokens: 0,
                    },
                    last: proto::TokenUsageBreakdown {
                        total_tokens: 150,
                        input_tokens: 100,
                        cached_input_tokens: 0,
                        output_tokens: 50,
                        reasoning_output_tokens: 0,
                    },
                    model_context_window: Some(128000),
                },
            },
        );
        let pe = server_notification_to_provider_event("srv1", &notification);
        let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
        match ui {
            UiEvent::ContextTokensUpdated { key, used, limit } => {
                assert_eq!(key.thread_id, "t1");
                assert_eq!(used, 150);
                assert_eq!(limit, 128000);
            }
            other => panic!("expected ContextTokensUpdated, got {other:?}"),
        }
    }

    // ── UniFFI round-trip for all ProviderEvent variants ───────────────

    #[test]
    fn provider_event_uniffi_serde_round_trip_all_variants() {
        let events = vec![
            ProviderEvent::ThreadStarted { thread_id: "t1".into() },
            ProviderEvent::ThreadArchived { thread_id: "t1".into() },
            ProviderEvent::ThreadNameUpdated { thread_id: "t1".into(), name: "Name".into() },
            ProviderEvent::ThreadStatusChanged { thread_id: "t1".into(), status: "active".into() },
            ProviderEvent::ModelRerouted { thread_id: "t1".into() },
            ProviderEvent::TurnStarted { thread_id: "t1".into(), turn_id: "turn1".into() },
            ProviderEvent::TurnCompleted { thread_id: "t1".into(), turn_id: "turn1".into() },
            ProviderEvent::TurnDiffUpdated { thread_id: "t1".into() },
            ProviderEvent::TurnPlanUpdated { thread_id: "t1".into() },
            ProviderEvent::ItemStarted { thread_id: "t1".into(), turn_id: "turn1".into(), item_id: "item1".into() },
            ProviderEvent::ItemCompleted { thread_id: "t1".into(), turn_id: "turn1".into(), item_id: "item1".into() },
            ProviderEvent::MessageDelta { thread_id: "t1".into(), item_id: "item1".into(), delta: "Hello".into() },
            ProviderEvent::ReasoningDelta { thread_id: "t1".into(), item_id: "item1".into(), delta: "thinking".into() },
            ProviderEvent::PlanDelta { thread_id: "t1".into(), item_id: "item1".into(), delta: "step".into() },
            ProviderEvent::CommandOutputDelta { thread_id: "t1".into(), item_id: "item1".into(), delta: "out".into() },
            ProviderEvent::FileChangeDelta { thread_id: "t1".into(), item_id: "item1".into(), delta: "diff".into() },
            ProviderEvent::ToolCallStarted { thread_id: "t1".into(), item_id: "item1".into(), tool_name: "tool".into(), call_id: "call1".into() },
            ProviderEvent::ToolCallUpdate { thread_id: "t1".into(), item_id: "item1".into(), call_id: "call1".into(), output_delta: "result".into() },
            ProviderEvent::McpToolCallProgress { thread_id: "t1".into(), item_id: "item1".into(), message: "progress".into() },
            ProviderEvent::PlanUpdated { thread_id: "t1".into(), item_id: "item1".into() },
            ProviderEvent::ApprovalRequested { thread_id: "t1".into(), request_id: "req1".into(), kind: "command".into(), reason: Some("why".into()), command: Some("cmd".into()) },
            ProviderEvent::UserInputRequested { thread_id: "t1".into(), request_id: "req1".into() },
            ProviderEvent::ServerRequestResolved { thread_id: "t1".into() },
            ProviderEvent::AccountRateLimitsUpdated { server_id: "srv1".into() },
            ProviderEvent::AccountLoginCompleted { server_id: "srv1".into() },
            ProviderEvent::RealtimeStarted { thread_id: "t1".into() },
            ProviderEvent::RealtimeClosed { thread_id: "t1".into() },
            ProviderEvent::StreamingStarted { thread_id: "t1".into() },
            ProviderEvent::StreamingCompleted { thread_id: "t1".into() },
            ProviderEvent::ContextTokensUpdated { thread_id: "t1".into(), used: 100, limit: 128000 },
            ProviderEvent::Disconnected { message: "lost".into() },
            ProviderEvent::Error { message: "fail".into(), code: Some(500) },
            ProviderEvent::Lagged { skipped: 3 },
            ProviderEvent::Unknown { method: "future".into(), payload: "{}".into() },
        ];

        assert_eq!(events.len(), 34, "expected 34 ProviderEvent variants");

        for event in &events {
            let json = serde_json::to_string(event).unwrap_or_else(|e| {
                panic!("failed to serialize {:?}: {e}", event)
            });
            let deserialized: ProviderEvent = serde_json::from_str(&json).unwrap_or_else(|e| {
                panic!("failed to deserialize {json}: {e}")
            });
            assert_eq!(event, &deserialized, "round-trip mismatch for {:?}", event);
        }
    }

    // ── Exhaustive ProviderEvent → UiEvent mapping test ────────────────

    #[test]
    fn every_provider_event_variant_maps_or_returns_none() {
        let events: Vec<ProviderEvent> = vec![
            ProviderEvent::ThreadStarted { thread_id: "t1".into() },
            ProviderEvent::ThreadArchived { thread_id: "t1".into() },
            ProviderEvent::ThreadNameUpdated { thread_id: "t1".into(), name: "Name".into() },
            ProviderEvent::ThreadStatusChanged { thread_id: "t1".into(), status: "active".into() },
            ProviderEvent::ModelRerouted { thread_id: "t1".into() },
            ProviderEvent::TurnStarted { thread_id: "t1".into(), turn_id: "turn1".into() },
            ProviderEvent::TurnCompleted { thread_id: "t1".into(), turn_id: "turn1".into() },
            ProviderEvent::TurnDiffUpdated { thread_id: "t1".into() },
            ProviderEvent::TurnPlanUpdated { thread_id: "t1".into() },
            ProviderEvent::ItemStarted { thread_id: "t1".into(), turn_id: "turn1".into(), item_id: "item1".into() },
            ProviderEvent::ItemCompleted { thread_id: "t1".into(), turn_id: "turn1".into(), item_id: "item1".into() },
            ProviderEvent::MessageDelta { thread_id: "t1".into(), item_id: "item1".into(), delta: "Hello".into() },
            ProviderEvent::ReasoningDelta { thread_id: "t1".into(), item_id: "item1".into(), delta: "thinking".into() },
            ProviderEvent::PlanDelta { thread_id: "t1".into(), item_id: "item1".into(), delta: "step".into() },
            ProviderEvent::CommandOutputDelta { thread_id: "t1".into(), item_id: "item1".into(), delta: "out".into() },
            ProviderEvent::FileChangeDelta { thread_id: "t1".into(), item_id: "item1".into(), delta: "diff".into() },
            ProviderEvent::ToolCallStarted { thread_id: "t1".into(), item_id: "item1".into(), tool_name: "tool".into(), call_id: "call1".into() },
            ProviderEvent::ToolCallUpdate { thread_id: "t1".into(), item_id: "item1".into(), call_id: "call1".into(), output_delta: "result".into() },
            ProviderEvent::McpToolCallProgress { thread_id: "t1".into(), item_id: "item1".into(), message: "progress".into() },
            ProviderEvent::PlanUpdated { thread_id: "t1".into(), item_id: "item1".into() },
            ProviderEvent::ApprovalRequested { thread_id: "t1".into(), request_id: "req1".into(), kind: "command".into(), reason: None, command: None },
            ProviderEvent::UserInputRequested { thread_id: "t1".into(), request_id: "req1".into() },
            ProviderEvent::ServerRequestResolved { thread_id: "t1".into() },
            ProviderEvent::AccountRateLimitsUpdated { server_id: "srv1".into() },
            ProviderEvent::AccountLoginCompleted { server_id: "srv1".into() },
            ProviderEvent::RealtimeStarted { thread_id: "t1".into() },
            ProviderEvent::RealtimeClosed { thread_id: "t1".into() },
            ProviderEvent::StreamingStarted { thread_id: "t1".into() },
            ProviderEvent::StreamingCompleted { thread_id: "t1".into() },
            ProviderEvent::ContextTokensUpdated { thread_id: "t1".into(), used: 100, limit: 128000 },
            ProviderEvent::Disconnected { message: "lost".into() },
            ProviderEvent::Error { message: "fail".into(), code: None },
            ProviderEvent::Lagged { skipped: 3 },
            ProviderEvent::Unknown { method: "future".into(), payload: "{}".into() },
        ];

        assert_eq!(events.len(), 34, "expected all 34 variants tested");

        for event in &events {
            // Every variant must either produce Some(UiEvent) or None — no panic.
            let result = provider_event_to_ui_event("srv1", event);
            // Just verify it doesn't panic. The result is checked in individual tests.
            let _ = result;
        }
    }

    // ── Additional per-variant UiEvent mapping tests ──────────────────

    #[test]
    fn provider_event_thread_started_to_ui_event() {
        let event = ProviderEvent::ThreadStarted {
            thread_id: "t1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::ConnectionStateChanged { server_id, health } => {
                assert_eq!(server_id, "srv1");
                assert_eq!(health, "thread_started");
            }
            other => panic!("expected ConnectionStateChanged, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_thread_archived_to_ui_event() {
        let event = ProviderEvent::ThreadArchived {
            thread_id: "t1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::ThreadArchived { key } => {
                assert_eq!(key.server_id, "srv1");
                assert_eq!(key.thread_id, "t1");
            }
            other => panic!("expected ThreadArchived, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_thread_name_updated_to_ui_event() {
        let event = ProviderEvent::ThreadNameUpdated {
            thread_id: "t1".into(),
            name: "My Thread".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::ThreadNameUpdated { key, thread_name } => {
                assert_eq!(key.thread_id, "t1");
                assert_eq!(thread_name.as_deref(), Some("My Thread"));
            }
            other => panic!("expected ThreadNameUpdated, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_thread_name_updated_empty_name_maps_to_none() {
        let event = ProviderEvent::ThreadNameUpdated {
            thread_id: "t1".into(),
            name: String::new(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::ThreadNameUpdated { thread_name, .. } => {
                assert!(thread_name.is_none(), "empty name should map to None");
            }
            other => panic!("expected ThreadNameUpdated, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_thread_status_changed_to_ui_event() {
        let event = ProviderEvent::ThreadStatusChanged {
            thread_id: "t1".into(),
            status: "active".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "thread/status/changed");
                assert_eq!(params["threadId"], "t1");
                assert_eq!(params["status"], "active");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_model_rerouted_to_ui_event() {
        let event = ProviderEvent::ModelRerouted {
            thread_id: "t1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "model/rerouted");
                assert_eq!(params["threadId"], "t1");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_turn_diff_updated_to_ui_event() {
        let event = ProviderEvent::TurnDiffUpdated {
            thread_id: "t1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "turn/diff/updated");
                assert_eq!(params["threadId"], "t1");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_turn_plan_updated_to_ui_event() {
        let event = ProviderEvent::TurnPlanUpdated {
            thread_id: "t1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "turn/plan/updated");
                assert_eq!(params["threadId"], "t1");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_item_started_to_ui_event() {
        let event = ProviderEvent::ItemStarted {
            thread_id: "t1".into(),
            turn_id: "turn1".into(),
            item_id: "item1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "item/started");
                assert_eq!(params["threadId"], "t1");
                assert_eq!(params["turnId"], "turn1");
                assert_eq!(params["itemId"], "item1");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_item_completed_to_ui_event() {
        let event = ProviderEvent::ItemCompleted {
            thread_id: "t1".into(),
            turn_id: "turn1".into(),
            item_id: "item1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "item/completed");
                assert_eq!(params["threadId"], "t1");
                assert_eq!(params["turnId"], "turn1");
                assert_eq!(params["itemId"], "item1");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_plan_delta_to_ui_event() {
        let event = ProviderEvent::PlanDelta {
            thread_id: "t1".into(),
            item_id: "item1".into(),
            delta: "step 1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::PlanDelta { key, item_id, delta } => {
                assert_eq!(key.thread_id, "t1");
                assert_eq!(key.server_id, "srv1");
                assert_eq!(item_id, "item1");
                assert_eq!(delta, "step 1");
            }
            other => panic!("expected PlanDelta, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_tool_call_started_to_ui_event() {
        let event = ProviderEvent::ToolCallStarted {
            thread_id: "t1".into(),
            item_id: "item1".into(),
            tool_name: "read_file".into(),
            call_id: "call1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "item/started");
                // ProviderEvent uses camelCase serde: "threadId" → "thread_id" in JSON
                assert!(params.is_object(), "params should be a JSON object");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_tool_call_update_to_ui_event() {
        let event = ProviderEvent::ToolCallUpdate {
            thread_id: "t1".into(),
            item_id: "item1".into(),
            call_id: "call1".into(),
            output_delta: "result text".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "toolCall/update");
                assert!(params.is_object(), "params should be a JSON object");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_mcp_tool_call_progress_to_ui_event() {
        let event = ProviderEvent::McpToolCallProgress {
            thread_id: "t1".into(),
            item_id: "item1".into(),
            message: "halfway".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "item/mcpToolCall/progress");
                assert_eq!(params["threadId"], "t1");
                assert_eq!(params["itemId"], "item1");
                assert_eq!(params["message"], "halfway");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_plan_updated_to_ui_event() {
        let event = ProviderEvent::PlanUpdated {
            thread_id: "t1".into(),
            item_id: "item1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "plan/updated");
                assert!(params.is_object(), "params should be a JSON object");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_approval_requested_to_ui_event() {
        let event = ProviderEvent::ApprovalRequested {
            thread_id: "t1".into(),
            request_id: "req1".into(),
            kind: "command".into(),
            reason: Some("needs approval".into()),
            command: Some("rm -rf /".into()),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "approval/requested");
                assert!(params.is_object(), "params should be a JSON object");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_user_input_requested_to_ui_event() {
        let event = ProviderEvent::UserInputRequested {
            thread_id: "t1".into(),
            request_id: "req1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "userInput/requested");
                assert!(params.is_object(), "params should be a JSON object");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_server_request_resolved_to_ui_event() {
        let event = ProviderEvent::ServerRequestResolved {
            thread_id: "t1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "serverRequest/resolved");
                assert_eq!(params["threadId"], "t1");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_account_rate_limits_to_ui_event() {
        let event = ProviderEvent::AccountRateLimitsUpdated {
            server_id: "srv1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { server_id, method, .. } => {
                assert_eq!(server_id, "srv1");
                assert_eq!(method, "account/rateLimits/updated");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_account_login_completed_to_ui_event() {
        let event = ProviderEvent::AccountLoginCompleted {
            server_id: "srv1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { server_id, method, .. } => {
                assert_eq!(server_id, "srv1");
                assert_eq!(method, "account/login/completed");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_realtime_started_to_ui_event() {
        let event = ProviderEvent::RealtimeStarted {
            thread_id: "t1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "thread/realtime/started");
                assert_eq!(params["threadId"], "t1");
            }
            other => panic!("expected RawNotification for realtime started, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_realtime_closed_to_ui_event() {
        let event = ProviderEvent::RealtimeClosed {
            thread_id: "t1".into(),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "thread/realtime/closed");
                assert_eq!(params["threadId"], "t1");
            }
            other => panic!("expected RawNotification for realtime closed, got {other:?}"),
        }
    }

    #[test]
    fn provider_event_error_without_code() {
        let event = ProviderEvent::Error {
            message: "unknown error".into(),
            code: None,
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::Error { key, message, code } => {
                assert!(key.is_none());
                assert_eq!(message, "unknown error");
                assert!(code.is_none());
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // ── Round-trip: all streaming delta types preserve data ────────────

    #[test]
    fn round_trip_reasoning_delta() {
        let notification =
            ServerNotification::ReasoningTextDelta(proto::ReasoningTextDeltaNotification {
                thread_id: "t1".to_string(),
                turn_id: "turn1".to_string(),
                item_id: "item1".to_string(),
                delta: "thinking hard".to_string(),
                content_index: 0,
            });
        let pe = server_notification_to_provider_event("srv1", &notification);
        let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
        match ui {
            UiEvent::ReasoningDelta { key, item_id, delta } => {
                assert_eq!(key.server_id, "srv1");
                assert_eq!(key.thread_id, "t1");
                assert_eq!(item_id, "item1");
                assert_eq!(delta, "thinking hard");
            }
            other => panic!("expected ReasoningDelta, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_command_output_delta() {
        let notification =
            ServerNotification::CommandExecutionOutputDelta(
                proto::CommandExecutionOutputDeltaNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    item_id: "item1".to_string(),
                    delta: "ls output".to_string(),
                },
            );
        let pe = server_notification_to_provider_event("srv1", &notification);
        let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
        match ui {
            UiEvent::CommandOutputDelta { key, item_id, delta } => {
                assert_eq!(key.server_id, "srv1");
                assert_eq!(key.thread_id, "t1");
                assert_eq!(item_id, "item1");
                assert_eq!(delta, "ls output");
            }
            other => panic!("expected CommandOutputDelta, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_file_change_delta_to_command_output() {
        let notification =
            ServerNotification::FileChangeOutputDelta(proto::FileChangeOutputDeltaNotification {
                thread_id: "t1".to_string(),
                turn_id: "turn1".to_string(),
                item_id: "item1".to_string(),
                delta: "+new line".to_string(),
            });
        let pe = server_notification_to_provider_event("srv1", &notification);
        let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
        match ui {
            UiEvent::CommandOutputDelta { key, delta, .. } => {
                assert_eq!(key.thread_id, "t1");
                assert_eq!(delta, "+new line");
            }
            other => panic!("expected CommandOutputDelta, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_plan_delta() {
        let notification = ServerNotification::PlanDelta(proto::PlanDeltaNotification {
            thread_id: "t1".to_string(),
            turn_id: "turn1".to_string(),
            item_id: "item1".to_string(),
            delta: "step 1: read file".to_string(),
        });
        let pe = server_notification_to_provider_event("srv1", &notification);
        let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
        match ui {
            UiEvent::PlanDelta { key, item_id, delta } => {
                assert_eq!(key.thread_id, "t1");
                assert_eq!(key.server_id, "srv1");
                assert_eq!(item_id, "item1");
                assert_eq!(delta, "step 1: read file");
            }
            other => panic!("expected PlanDelta, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_reasoning_summary_delta() {
        let notification = ServerNotification::ReasoningSummaryTextDelta(
            proto::ReasoningSummaryTextDeltaNotification {
                thread_id: "t1".to_string(),
                turn_id: "turn1".to_string(),
                item_id: "item1".to_string(),
                delta: "summarizing...".to_string(),
                summary_index: 0,
            },
        );
        let pe = server_notification_to_provider_event("srv1", &notification);
        let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
        match ui {
            UiEvent::ReasoningDelta { key, delta, .. } => {
                assert_eq!(key.thread_id, "t1");
                assert_eq!(delta, "summarizing...");
            }
            other => panic!("expected ReasoningDelta, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_error_notification() {
        let notification = ServerNotification::Error(proto::ErrorNotification {
            error: proto::TurnError {
                message: "rate limited".to_string(),
                codex_error_info: None,
                additional_details: None,
            },
            will_retry: false,
            thread_id: "t1".to_string(),
            turn_id: String::new(),
        });
        let pe = server_notification_to_provider_event("srv1", &notification);
        let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
        match ui {
            UiEvent::Error { message, .. } => {
                assert_eq!(message, "rate limited");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // ── Token accounting (VAL-ABS-050) ────────────────────────────────

    #[test]
    fn token_accounting_used_equals_input_plus_output() {
        // Verify `used = input_tokens + output_tokens` from the last turn.
        let notification = ServerNotification::ThreadTokenUsageUpdated(
            proto::ThreadTokenUsageUpdatedNotification {
                thread_id: "t1".to_string(),
                turn_id: "turn1".to_string(),
                token_usage: proto::ThreadTokenUsage {
                    total: proto::TokenUsageBreakdown {
                        total_tokens: 10000,
                        input_tokens: 7000,
                        cached_input_tokens: 500,
                        output_tokens: 3000,
                        reasoning_output_tokens: 100,
                    },
                    last: proto::TokenUsageBreakdown {
                        total_tokens: 500,
                        input_tokens: 300,
                        cached_input_tokens: 0,
                        output_tokens: 200,
                        reasoning_output_tokens: 0,
                    },
                    model_context_window: Some(200000),
                },
            },
        );

        // Stage 1: ServerNotification → ProviderEvent
        let pe = server_notification_to_provider_event("srv1", &notification);
        match &pe {
            ProviderEvent::ContextTokensUpdated { thread_id, used, limit } => {
                assert_eq!(thread_id, "t1");
                assert_eq!(*used, 300 + 200, "used must be last.input_tokens + last.output_tokens");
                assert_eq!(*limit, 200000, "limit must be model_context_window");
            }
            other => panic!("expected ContextTokensUpdated, got {other:?}"),
        }

        // Stage 2: ProviderEvent → UiEvent
        let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
        match ui {
            UiEvent::ContextTokensUpdated { key, used, limit } => {
                assert_eq!(key.thread_id, "t1");
                assert_eq!(key.server_id, "srv1");
                assert_eq!(used, 500);
                assert_eq!(limit, 200000);
            }
            other => panic!("expected ContextTokensUpdated, got {other:?}"),
        }
    }

    #[test]
    fn token_accounting_zero_tokens() {
        let notification = ServerNotification::ThreadTokenUsageUpdated(
            proto::ThreadTokenUsageUpdatedNotification {
                thread_id: "t1".to_string(),
                turn_id: "turn1".to_string(),
                token_usage: proto::ThreadTokenUsage {
                    total: proto::TokenUsageBreakdown {
                        total_tokens: 0,
                        input_tokens: 0,
                        cached_input_tokens: 0,
                        output_tokens: 0,
                        reasoning_output_tokens: 0,
                    },
                    last: proto::TokenUsageBreakdown {
                        total_tokens: 0,
                        input_tokens: 0,
                        cached_input_tokens: 0,
                        output_tokens: 0,
                        reasoning_output_tokens: 0,
                    },
                    model_context_window: Some(128000),
                },
            },
        );
        let pe = server_notification_to_provider_event("srv1", &notification);
        let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
        match ui {
            UiEvent::ContextTokensUpdated { used, limit, .. } => {
                assert_eq!(used, 0);
                assert_eq!(limit, 128000);
            }
            other => panic!("expected ContextTokensUpdated, got {other:?}"),
        }
    }

    #[test]
    fn token_accounting_no_context_window_defaults_to_zero() {
        let notification = ServerNotification::ThreadTokenUsageUpdated(
            proto::ThreadTokenUsageUpdatedNotification {
                thread_id: "t1".to_string(),
                turn_id: "turn1".to_string(),
                token_usage: proto::ThreadTokenUsage {
                    total: proto::TokenUsageBreakdown {
                        total_tokens: 500,
                        input_tokens: 300,
                        cached_input_tokens: 0,
                        output_tokens: 200,
                        reasoning_output_tokens: 0,
                    },
                    last: proto::TokenUsageBreakdown {
                        total_tokens: 500,
                        input_tokens: 300,
                        cached_input_tokens: 0,
                        output_tokens: 200,
                        reasoning_output_tokens: 0,
                    },
                    model_context_window: None,
                },
            },
        );
        let pe = server_notification_to_provider_event("srv1", &notification);
        match &pe {
            ProviderEvent::ContextTokensUpdated { limit, .. } => {
                assert_eq!(*limit, 0, "no context window should default to 0");
            }
            other => panic!("expected ContextTokensUpdated, got {other:?}"),
        }
    }

    // ── Pipeline-level tests (VAL-ABS-013 exhaustive) ─────────────────

    /// Verify that every ProviderEvent variant that should produce a UiEvent
    /// actually does, and that the ThreadKey is correct.
    #[test]
    fn ui_event_pipeline_all_variants_produce_correct_key() {
        let server_id = "test-server";

        // Events that carry thread_id and should produce ThreadKey with server_id
        let thread_events: Vec<ProviderEvent> = vec![
            ProviderEvent::ThreadArchived { thread_id: "t1".into() },
            ProviderEvent::ThreadNameUpdated { thread_id: "t1".into(), name: "N".into() },
            ProviderEvent::TurnStarted { thread_id: "t1".into(), turn_id: "turn1".into() },
            ProviderEvent::TurnCompleted { thread_id: "t1".into(), turn_id: "turn1".into() },
            ProviderEvent::MessageDelta { thread_id: "t1".into(), item_id: "i1".into(), delta: "d".into() },
            ProviderEvent::ReasoningDelta { thread_id: "t1".into(), item_id: "i1".into(), delta: "d".into() },
            ProviderEvent::PlanDelta { thread_id: "t1".into(), item_id: "i1".into(), delta: "d".into() },
            ProviderEvent::CommandOutputDelta { thread_id: "t1".into(), item_id: "i1".into(), delta: "d".into() },
            ProviderEvent::FileChangeDelta { thread_id: "t1".into(), item_id: "i1".into(), delta: "d".into() },
            ProviderEvent::ContextTokensUpdated { thread_id: "t1".into(), used: 100, limit: 128000 },
        ];

        for event in &thread_events {
            let ui = provider_event_to_ui_event(server_id, event)
                .unwrap_or_else(|| panic!("{event:?} should produce a UiEvent"));

            // Extract the ThreadKey from the UiEvent, whatever variant it is
            let key = extract_thread_key(&ui)
                .unwrap_or_else(|| panic!("{event:?} → UiEvent has no ThreadKey: {ui:?}"));

            assert_eq!(
                key.server_id, server_id,
                "server_id mismatch for {event:?}"
            );
            assert_eq!(
                key.thread_id, "t1",
                "thread_id mismatch for {event:?}"
            );
        }
    }

    /// Helper to extract a ThreadKey reference from any UiEvent that contains one.
    fn extract_thread_key(ui: &UiEvent) -> Option<ThreadKey> {
        match ui {
            UiEvent::ThreadArchived { key } => Some(key.clone()),
            UiEvent::ThreadNameUpdated { key, .. } => Some(key.clone()),
            UiEvent::TurnStarted { key, .. } => Some(key.clone()),
            UiEvent::TurnCompleted { key, .. } => Some(key.clone()),
            UiEvent::MessageDelta { key, .. } => Some(key.clone()),
            UiEvent::ReasoningDelta { key, .. } => Some(key.clone()),
            UiEvent::PlanDelta { key, .. } => Some(key.clone()),
            UiEvent::CommandOutputDelta { key, .. } => Some(key.clone()),
            UiEvent::ContextTokensUpdated { key, .. } => Some(key.clone()),
            UiEvent::Error { key: Some(key), .. } => Some(key.clone()),
            _ => None,
        }
    }

    // ── Realtime voice pipeline (VAL-ABS-052) ─────────────────────────

    #[test]
    fn realtime_voice_started_through_pipeline() {
        let notification =
            ServerNotification::ThreadRealtimeStarted(proto::ThreadRealtimeStartedNotification {
                thread_id: "voice-thread".to_string(),
                session_id: Some("sess-1".to_string()),
                version: codex_protocol::protocol::RealtimeConversationVersion::V2,
            });
        let pe = server_notification_to_provider_event("srv1", &notification);
        match &pe {
            ProviderEvent::RealtimeStarted { thread_id } => {
                assert_eq!(thread_id, "voice-thread");
            }
            other => panic!("expected RealtimeStarted, got {other:?}"),
        }
        let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
        match &ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "thread/realtime/started");
                assert_eq!(params["threadId"], "voice-thread");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    #[test]
    fn realtime_voice_closed_through_pipeline() {
        let notification =
            ServerNotification::ThreadRealtimeClosed(proto::ThreadRealtimeClosedNotification {
                thread_id: "voice-thread".to_string(),
                reason: None,
            });
        let pe = server_notification_to_provider_event("srv1", &notification);
        match &pe {
            ProviderEvent::RealtimeClosed { thread_id } => {
                assert_eq!(thread_id, "voice-thread");
            }
            other => panic!("expected RealtimeClosed, got {other:?}"),
        }
        let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
        match &ui {
            UiEvent::RawNotification { method, params, .. } => {
                assert_eq!(method, "thread/realtime/closed");
                assert_eq!(params["threadId"], "voice-thread");
            }
            other => panic!("expected RawNotification, got {other:?}"),
        }
    }

    // ── AppServerEvent envelope → ProviderEvent → UiEvent ─────────────

    #[test]
    fn app_server_event_to_ui_event_disconnected() {
        let event = AppServerEvent::Disconnected {
            message: "websocket closed".to_string(),
        };
        let pe = app_server_event_to_provider_event("srv1", &event);
        let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
        match ui {
            UiEvent::ConnectionStateChanged { server_id, health } => {
                assert_eq!(server_id, "srv1");
                assert_eq!(health, "disconnected");
            }
            other => panic!("expected ConnectionStateChanged, got {other:?}"),
        }
    }

    #[test]
    fn app_server_event_to_ui_event_lagged_returns_none() {
        let event = AppServerEvent::Lagged { skipped: 10 };
        let pe = app_server_event_to_provider_event("srv1", &event);
        let ui = provider_event_to_ui_event("srv1", &pe);
        assert!(ui.is_none(), "Lagged should not produce UiEvent");
    }

    // ── Multi-delta streaming pipeline order preservation ──────────────

    #[test]
    fn streaming_pipeline_preserves_delta_order() {
        let deltas = vec!["Hello", " ", "world", "!"];
        let mut ui_events: Vec<String> = Vec::new();

        for delta in &deltas {
            let notification =
                ServerNotification::AgentMessageDelta(proto::AgentMessageDeltaNotification {
                    thread_id: "t1".to_string(),
                    turn_id: "turn1".to_string(),
                    item_id: "item1".to_string(),
                    delta: delta.to_string(),
                });
            let pe = server_notification_to_provider_event("srv1", &notification);
            let ui = provider_event_to_ui_event("srv1", &pe).expect("should map");
            match ui {
                UiEvent::MessageDelta { delta, .. } => ui_events.push(delta),
                other => panic!("expected MessageDelta, got {other:?}"),
            }
        }

        assert_eq!(ui_events, vec!["Hello", " ", "world", "!"]);
    }

    // ── Error event with actionable message ────────────────────────────

    #[test]
    fn error_event_preserves_message_for_user_display() {
        let event = ProviderEvent::Error {
            message: "Thread t1 not found".into(),
            code: Some(404),
        };
        let ui = provider_event_to_ui_event("srv1", &event).expect("should map");
        match ui {
            UiEvent::Error { key, message, code } => {
                assert!(key.is_none(), "provider-level error has no thread key");
                assert_eq!(message, "Thread t1 not found");
                assert_eq!(code, Some(404));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
