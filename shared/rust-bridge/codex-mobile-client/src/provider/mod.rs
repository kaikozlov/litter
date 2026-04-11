//! Provider abstraction layer for multi-agent support.
//!
//! Defines the `ProviderTransport` trait (object-safe, `Send + Sync + 'static`)
//! and shared types (`ProviderEvent`, `AgentType`, `AgentInfo`) used by all
//! provider implementations (Codex, ACP, Pi, Droid).
//!
//! Each provider maps its protocol-specific events to the normalized
//! `ProviderEvent` enum, which downstream code then maps to `UiEvent`
//! for the store/reducer pipeline.

use std::fmt;

use crate::transport::{RpcError, TransportError};

// ── AgentType ──────────────────────────────────────────────────────────────

/// Identifies the type of agent behind a server connection.
///
/// Used for provider routing and UI display. Each variant corresponds to a
/// distinct transport protocol or agent binary.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, uniffi::Enum,
)]
#[serde(rename_all = "camelCase")]
pub enum AgentType {
    /// Codex app-server (JSON-RPC over WebSocket, in-process, or SSH tunnel).
    Codex,
    /// Pi agent over ACP (SSH → `npx pi-acp`).
    PiAcp,
    /// Pi agent over native JSONL RPC (SSH → `pi --mode rpc`).
    PiNative,
    /// Droid agent over ACP (SSH → `droid exec --output-format acp`).
    DroidAcp,
    /// Droid agent over native Factory API JSON-RPC (SSH → `droid exec --stream-jsonrpc`).
    DroidNative,
    /// Any ACP-compatible agent (SSH → generic ACP adapter).
    GenericAcp,
}

impl fmt::Display for AgentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codex => write!(f, "Codex"),
            Self::PiAcp => write!(f, "Pi (ACP)"),
            Self::PiNative => write!(f, "Pi (Native)"),
            Self::DroidAcp => write!(f, "Droid (ACP)"),
            Self::DroidNative => write!(f, "Droid (Native)"),
            Self::GenericAcp => write!(f, "Generic ACP"),
        }
    }
}

// ── AgentInfo ──────────────────────────────────────────────────────────────

/// Describes a detected agent on a server.
///
/// Populated during discovery probing and passed to the UI for agent selection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    /// Unique identifier for this agent (e.g. binary path or service name).
    pub id: String,
    /// Human-readable name displayed in agent picker.
    pub display_name: String,
    /// Short description of the agent's purpose.
    pub description: String,
    /// Transport types that this agent supports.
    pub detected_transports: Vec<AgentType>,
    /// Capability strings advertised by the agent (e.g. "streaming", "tools", "plans").
    pub capabilities: Vec<String>,
}

// ── SessionInfo ────────────────────────────────────────────────────────────

/// Summary of a session available on a provider.
///
/// Used by `ProviderTransport::list_sessions` for session history.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    /// Unique session identifier.
    pub id: String,
    /// Display title (may be empty for new sessions).
    pub title: String,
    /// ISO 8601 timestamp of session creation.
    pub created_at: String,
    /// ISO 8601 timestamp of last activity.
    pub updated_at: String,
}

// ── ProviderEvent ──────────────────────────────────────────────────────────

/// Normalized event type produced by all provider implementations.
///
/// Each provider maps its protocol-specific events to these variants.
/// Downstream code (store/reducer) receives `ProviderEvent` and maps to `UiEvent`.
///
/// All fields use only UniFFI-safe types (`String`, `u16`, `u32`, `i64`,
/// `bool`, `Vec<T>`, `Option<T>`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Enum)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ProviderEvent {
    // ── Thread/Turn lifecycle ──────────────────────────────────────────
    /// A new thread was created.
    ThreadStarted {
        thread_id: String,
    },
    /// A thread was archived.
    ThreadArchived {
        thread_id: String,
    },
    /// A thread's display name changed.
    ThreadNameUpdated {
        thread_id: String,
        name: String,
    },
    /// A thread's status changed (idle, active, error).
    ThreadStatusChanged {
        thread_id: String,
        status: String,
    },
    /// The model was rerouted for a thread.
    ModelRerouted {
        thread_id: String,
    },
    /// A new turn started in a thread.
    TurnStarted {
        thread_id: String,
        turn_id: String,
    },
    /// A turn completed.
    TurnCompleted {
        thread_id: String,
        turn_id: String,
    },
    /// A turn's diff was updated.
    TurnDiffUpdated {
        thread_id: String,
    },
    /// A turn's plan was updated.
    TurnPlanUpdated {
        thread_id: String,
    },
    /// An item (message, command, etc.) started streaming.
    ItemStarted {
        thread_id: String,
        turn_id: String,
        item_id: String,
    },
    /// An item finished streaming.
    ItemCompleted {
        thread_id: String,
        turn_id: String,
        item_id: String,
    },

    // ── Streaming deltas ───────────────────────────────────────────────
    /// Incremental assistant message text.
    MessageDelta {
        thread_id: String,
        item_id: String,
        delta: String,
    },
    /// Incremental reasoning/thinking text.
    ReasoningDelta {
        thread_id: String,
        item_id: String,
        delta: String,
    },
    /// Incremental plan text.
    PlanDelta {
        thread_id: String,
        item_id: String,
        delta: String,
    },
    /// Incremental command execution output.
    CommandOutputDelta {
        thread_id: String,
        item_id: String,
        delta: String,
    },
    /// Incremental file change output.
    FileChangeDelta {
        thread_id: String,
        item_id: String,
        delta: String,
    },

    // ── Tool calls ─────────────────────────────────────────────────────
    /// A tool call started.
    ToolCallStarted {
        thread_id: String,
        item_id: String,
        tool_name: String,
        call_id: String,
    },
    /// A tool call was updated with progress or output.
    ToolCallUpdate {
        thread_id: String,
        item_id: String,
        call_id: String,
        output_delta: String,
    },
    /// An MCP tool call progress notification.
    McpToolCallProgress {
        thread_id: String,
        item_id: String,
        message: String,
    },

    // ── Plans ──────────────────────────────────────────────────────────
    /// The proposed plan was updated with new steps.
    PlanUpdated {
        thread_id: String,
        item_id: String,
    },

    // ── Approvals / user input ─────────────────────────────────────────
    /// The agent is requesting user approval for an action.
    ApprovalRequested {
        thread_id: String,
        request_id: String,
        kind: String,
        reason: Option<String>,
        command: Option<String>,
    },
    /// The agent is requesting user input (e.g. elicit questions).
    UserInputRequested {
        thread_id: String,
        request_id: String,
    },
    /// A server request was resolved (approved/rejected).
    ServerRequestResolved {
        thread_id: String,
    },

    // ── Account / limits ───────────────────────────────────────────────
    /// Account rate limits were updated.
    AccountRateLimitsUpdated {
        server_id: String,
    },
    /// Account login completed.
    AccountLoginCompleted {
        server_id: String,
    },

    // ── Realtime voice ─────────────────────────────────────────────────
    /// Realtime voice session started.
    RealtimeStarted {
        thread_id: String,
    },
    /// Realtime voice session ended.
    RealtimeClosed {
        thread_id: String,
    },

    // ── Streaming lifecycle ────────────────────────────────────────────
    /// The agent started streaming output.
    StreamingStarted {
        thread_id: String,
    },
    /// The agent finished streaming output.
    StreamingCompleted {
        thread_id: String,
    },

    // ── Context ────────────────────────────────────────────────────────
    /// Token usage was updated for a thread.
    ContextTokensUpdated {
        thread_id: String,
        used: u64,
        limit: u64,
    },

    // ── Connection lifecycle ───────────────────────────────────────────
    /// The transport disconnected.
    Disconnected {
        message: String,
    },
    /// The transport encountered an error.
    Error {
        message: String,
        code: Option<i64>,
    },
    /// Some events were dropped due to slow consumer.
    Lagged {
        skipped: u32,
    },

    // ── Forward compatibility ──────────────────────────────────────────
    /// An unrecognized event from upstream. Forward-compatibility catch-all.
    Unknown {
        method: String,
        payload: String,
    },
}

// ── ProviderTransport trait ────────────────────────────────────────────────

/// Configuration for establishing a provider connection.
///
/// Each provider implementation interprets the relevant fields.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// WebSocket URL (Codex remote).
    pub websocket_url: Option<String>,
    /// Remote host for SSH-based connections.
    pub ssh_host: Option<String>,
    /// Remote port for SSH connections.
    pub ssh_port: Option<u16>,
    /// Remote port for the agent's service (e.g. Codex port 8390).
    pub remote_port: Option<u16>,
    /// Working directory on the remote host.
    pub working_dir: Option<String>,
    /// Agent type to connect as.
    pub agent_type: AgentType,
    /// Client name reported during handshakes.
    pub client_name: String,
    /// Client version reported during handshakes.
    pub client_version: String,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            websocket_url: None,
            ssh_host: None,
            ssh_port: None,
            remote_port: None,
            working_dir: None,
            agent_type: AgentType::Codex,
            client_name: "litter".to_string(),
            client_version: "0.1.0".to_string(),
        }
    }
}

/// Object-safe transport trait for provider communication.
///
/// All agent providers implement this trait. It abstracts connection
/// lifecycle, RPC, event streaming, and session management behind a
/// unified interface that `ServerSession` can use via `dyn ProviderTransport`.
///
/// # Object Safety
///
/// The trait is object-safe: all methods take `&self` or `&mut self` and
/// return concrete types (no `impl Trait` returns). It can be used as
/// `Box<dyn ProviderTransport>` or `Arc<dyn ProviderTransport>`.
#[async_trait::async_trait]
pub trait ProviderTransport: Send + Sync + 'static {
    /// Connect to the agent using the given configuration.
    ///
    /// After a successful connection, `is_connected()` must return `true`
    /// and `send_request`/`next_event` must be usable.
    async fn connect(&mut self, config: &ProviderConfig) -> Result<(), TransportError>;

    /// Disconnect from the agent and release all resources.
    ///
    /// Must be idempotent — calling disconnect twice must not panic.
    /// After disconnect, `send_request` must return `Err(TransportError::Disconnected)`.
    async fn disconnect(&mut self);

    /// Send an RPC request and return the response value.
    ///
    /// Returns `Err(TransportError::Disconnected)` if the transport is not connected.
    async fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError>;

    /// Send a fire-and-forget notification.
    ///
    /// Returns `Err(TransportError::Disconnected)` if the transport is not connected.
    async fn send_notification(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), RpcError>;

    /// Return the next buffered event, or `None` if no event is available.
    ///
    /// This is a non-blocking peek. Use `event_receiver()` for async streaming.
    fn next_event(&self) -> Option<ProviderEvent>;

    /// Return a new broadcast receiver for the event stream.
    ///
    /// Each call returns a fresh receiver. The provider internally broadcasts
    /// events to all active receivers.
    fn event_receiver(&self) -> tokio::sync::broadcast::Receiver<ProviderEvent>;

    /// List sessions available on this provider.
    async fn list_sessions(&mut self) -> Result<Vec<SessionInfo>, RpcError>;

    /// Check if the transport is currently connected.
    fn is_connected(&self) -> bool;
}

pub mod codex;
pub mod error_handling;
pub mod mapping;

// ── Factory functions ──────────────────────────────────────────────────────

/// Create a provider for the given agent type.
///
/// For Milestone 1, only `AgentType::Codex` is supported via
/// `create_codex_provider()`. Other agent types return an error with a clear
/// message indicating the unsupported agent type.
pub fn create_provider_for_agent_type(
    agent_type: AgentType,
) -> Result<Box<dyn ProviderTransport>, TransportError> {
    match agent_type {
        AgentType::Codex => {
            // CodexProvider requires an AppServerClient at construction time,
            // so we can't create a blank one here. The caller should use
            // CodexProvider::new() directly with an established client.
            Err(TransportError::ConnectionFailed(
                "Codex provider requires an existing AppServerClient; use CodexProvider::new()".to_string(),
            ))
        }
        other => Err(TransportError::ConnectionFailed(format!(
            "unsupported agent type: {other}"
        ))),
    }
}

/// Create a `CodexProvider` from an existing `AppServerClient`.
///
/// This is the primary factory for Codex connections. The client should
/// already be connected (in-process or remote).
pub fn create_codex_provider(
    client: codex_app_server_client::AppServerClient,
) -> Box<dyn ProviderTransport> {
    Box::new(codex::CodexProvider::new(client))
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Object safety ──────────────────────────────────────────────────

    /// Compile-time assertion that `ProviderTransport` is object-safe
    /// and `Box<dyn ProviderTransport>` is `Send + Sync`.
    #[test]
    fn provider_trait_object_safe() {
        fn _assert_send_sync<T: Send + Sync>() {}
        _assert_send_sync::<Box<dyn ProviderTransport>>();
    }

    // ── AgentType variants ─────────────────────────────────────────────

    #[test]
    fn agent_type_all_variants() {
        let variants = [
            AgentType::Codex,
            AgentType::PiAcp,
            AgentType::PiNative,
            AgentType::DroidAcp,
            AgentType::DroidNative,
            AgentType::GenericAcp,
        ];

        for variant in &variants {
            // Debug formatting works.
            let debug = format!("{variant:?}");
            assert!(!debug.is_empty());

            // Display formatting works.
            let display = format!("{variant}");
            assert!(!display.is_empty());

            // Clone works.
            let cloned = variant.clone();
            assert_eq!(*variant, cloned);

            // Copy works.
            let copied = *variant;
            assert_eq!(*variant, copied);
        }

        // All 6 variants present.
        assert_eq!(variants.len(), 6);

        // PartialEq round-trip.
        assert_eq!(AgentType::Codex, AgentType::Codex);
        assert_ne!(AgentType::Codex, AgentType::PiAcp);
    }

    #[test]
    fn agent_type_serde_roundtrip() {
        for variant in [
            AgentType::Codex,
            AgentType::PiAcp,
            AgentType::PiNative,
            AgentType::DroidAcp,
            AgentType::DroidNative,
            AgentType::GenericAcp,
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            let deserialized: AgentType = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, deserialized, "round-trip failed for {variant:?}");
        }
    }

    #[test]
    fn agent_type_display() {
        assert_eq!(format!("{}", AgentType::Codex), "Codex");
        assert_eq!(format!("{}", AgentType::PiAcp), "Pi (ACP)");
        assert_eq!(format!("{}", AgentType::PiNative), "Pi (Native)");
        assert_eq!(format!("{}", AgentType::DroidAcp), "Droid (ACP)");
        assert_eq!(format!("{}", AgentType::DroidNative), "Droid (Native)");
        assert_eq!(format!("{}", AgentType::GenericAcp), "Generic ACP");
    }

    // ── AgentInfo ──────────────────────────────────────────────────────

    #[test]
    fn agent_info_record_construction() {
        let info = AgentInfo {
            id: "pi-native".to_string(),
            display_name: "Pi".to_string(),
            description: "Pi coding agent".to_string(),
            detected_transports: vec![AgentType::PiNative, AgentType::PiAcp],
            capabilities: vec!["streaming".to_string(), "tools".to_string()],
        };

        assert_eq!(info.id, "pi-native");
        assert_eq!(info.display_name, "Pi");
        assert_eq!(info.description, "Pi coding agent");
        assert_eq!(info.detected_transports.len(), 2);
        assert_eq!(info.capabilities.len(), 2);
    }

    #[test]
    fn agent_info_serde_roundtrip() {
        let info = AgentInfo {
            id: "codex-1".to_string(),
            display_name: "Codex".to_string(),
            description: "Codex agent".to_string(),
            detected_transports: vec![AgentType::Codex],
            capabilities: vec!["streaming".to_string()],
        };

        let json = serde_json::to_string(&info).unwrap();
        let deserialized: AgentInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, deserialized);
    }

    // ── SessionInfo ────────────────────────────────────────────────────

    #[test]
    fn session_info_record_construction() {
        let info = SessionInfo {
            id: "sess-123".to_string(),
            title: "My Session".to_string(),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: "2025-01-01T01:00:00Z".to_string(),
        };

        assert_eq!(info.id, "sess-123");
        assert_eq!(info.title, "My Session");
    }

    // ── ProviderEvent ──────────────────────────────────────────────────

    #[test]
    fn provider_event_all_variants_instantiate() {
        // Verify every ProviderEvent variant can be constructed.
        let events = vec![
            ProviderEvent::ThreadStarted {
                thread_id: "t1".into(),
            },
            ProviderEvent::ThreadArchived {
                thread_id: "t1".into(),
            },
            ProviderEvent::ThreadNameUpdated {
                thread_id: "t1".into(),
                name: "New Name".into(),
            },
            ProviderEvent::ThreadStatusChanged {
                thread_id: "t1".into(),
                status: "active".into(),
            },
            ProviderEvent::ModelRerouted {
                thread_id: "t1".into(),
            },
            ProviderEvent::TurnStarted {
                thread_id: "t1".into(),
                turn_id: "turn1".into(),
            },
            ProviderEvent::TurnCompleted {
                thread_id: "t1".into(),
                turn_id: "turn1".into(),
            },
            ProviderEvent::TurnDiffUpdated {
                thread_id: "t1".into(),
            },
            ProviderEvent::TurnPlanUpdated {
                thread_id: "t1".into(),
            },
            ProviderEvent::ItemStarted {
                thread_id: "t1".into(),
                turn_id: "turn1".into(),
                item_id: "item1".into(),
            },
            ProviderEvent::ItemCompleted {
                thread_id: "t1".into(),
                turn_id: "turn1".into(),
                item_id: "item1".into(),
            },
            ProviderEvent::MessageDelta {
                thread_id: "t1".into(),
                item_id: "item1".into(),
                delta: "Hello".into(),
            },
            ProviderEvent::ReasoningDelta {
                thread_id: "t1".into(),
                item_id: "item1".into(),
                delta: "thinking...".into(),
            },
            ProviderEvent::PlanDelta {
                thread_id: "t1".into(),
                item_id: "item1".into(),
                delta: "step 1".into(),
            },
            ProviderEvent::CommandOutputDelta {
                thread_id: "t1".into(),
                item_id: "item1".into(),
                delta: "output".into(),
            },
            ProviderEvent::FileChangeDelta {
                thread_id: "t1".into(),
                item_id: "item1".into(),
                delta: "diff".into(),
            },
            ProviderEvent::ToolCallStarted {
                thread_id: "t1".into(),
                item_id: "item1".into(),
                tool_name: "read_file".into(),
                call_id: "call1".into(),
            },
            ProviderEvent::ToolCallUpdate {
                thread_id: "t1".into(),
                item_id: "item1".into(),
                call_id: "call1".into(),
                output_delta: "result".into(),
            },
            ProviderEvent::McpToolCallProgress {
                thread_id: "t1".into(),
                item_id: "item1".into(),
                message: "progress".into(),
            },
            ProviderEvent::PlanUpdated {
                thread_id: "t1".into(),
                item_id: "item1".into(),
            },
            ProviderEvent::ApprovalRequested {
                thread_id: "t1".into(),
                request_id: "req1".into(),
                kind: "command".into(),
                reason: Some("needs approval".into()),
                command: Some("rm -rf /".into()),
            },
            ProviderEvent::UserInputRequested {
                thread_id: "t1".into(),
                request_id: "req1".into(),
            },
            ProviderEvent::ServerRequestResolved {
                thread_id: "t1".into(),
            },
            ProviderEvent::AccountRateLimitsUpdated {
                server_id: "srv1".into(),
            },
            ProviderEvent::AccountLoginCompleted {
                server_id: "srv1".into(),
            },
            ProviderEvent::RealtimeStarted {
                thread_id: "t1".into(),
            },
            ProviderEvent::RealtimeClosed {
                thread_id: "t1".into(),
            },
            ProviderEvent::StreamingStarted {
                thread_id: "t1".into(),
            },
            ProviderEvent::StreamingCompleted {
                thread_id: "t1".into(),
            },
            ProviderEvent::ContextTokensUpdated {
                thread_id: "t1".into(),
                used: 100,
                limit: 128000,
            },
            ProviderEvent::Disconnected {
                message: "connection lost".into(),
            },
            ProviderEvent::Error {
                message: "something failed".into(),
                code: Some(500),
            },
            ProviderEvent::Lagged { skipped: 3 },
            ProviderEvent::Unknown {
                method: "future/event".into(),
                payload: "{}".into(),
            },
        ];

        // Count unique variants — should be 34.
        assert_eq!(events.len(), 34, "expected 34 ProviderEvent variants");

        // Verify Debug formatting works for all.
        for event in &events {
            let debug = format!("{event:?}");
            assert!(!debug.is_empty());
        }
    }

    #[test]
    fn provider_event_serde_roundtrip() {
        let event = ProviderEvent::MessageDelta {
            thread_id: "t1".into(),
            item_id: "item1".into(),
            delta: "Hello world".into(),
        };

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: ProviderEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn provider_event_unknown_catch_all() {
        let event = ProviderEvent::Unknown {
            method: "some/new/notification".to_string(),
            payload: r#"{"foo":"bar"}"#.to_string(),
        };

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: ProviderEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, deserialized);

        match deserialized {
            ProviderEvent::Unknown { method, payload } => {
                assert_eq!(method, "some/new/notification");
                assert_eq!(payload, r#"{"foo":"bar"}"#);
            }
            _ => panic!("expected Unknown variant"),
        }
    }

    // ── ProviderConfig defaults ────────────────────────────────────────

    #[test]
    fn provider_config_default_is_codex() {
        let config = ProviderConfig::default();
        assert_eq!(config.agent_type, AgentType::Codex);
        assert!(config.websocket_url.is_none());
        assert!(config.ssh_host.is_none());
        assert_eq!(config.client_name, "litter");
    }
}
