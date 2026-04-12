# Provider Transport Layer

## Overview

The provider abstraction layer (`src/provider/mod.rs`) defines the `ProviderTransport` trait and shared types used by all agent implementations (Codex, ACP, Pi, Droid).

## Key Types

### `ProviderTransport` trait
- Object-safe, `Send + Sync + 'static`
- Can be used as `Box<dyn ProviderTransport>` or `Arc<dyn ProviderTransport>`
- Methods: `connect`, `disconnect`, `send_request`, `send_notification`, `next_event`, `event_receiver`, `list_sessions`, `is_connected`

### `AgentType` enum (UniFFI)
- 6 variants: `Codex`, `PiAcp`, `PiNative`, `DroidAcp`, `DroidNative`, `GenericAcp`
- Derives `Debug, Clone, Copy, PartialEq, Eq, Hash`
- UniFFI-safe with serde support

### `ProviderEvent` enum (UniFFI)
- 34 variants covering all upstream Codex events plus ACP/Pi/Droid-specific events
- Includes `Unknown` catch-all for forward compatibility
- All fields use UniFFI-safe types (String, u64, i64, bool, Vec, Option)

### `AgentInfo` record (UniFFI)
- Fields: `id`, `display_name`, `description`, `detected_transports: Vec<AgentType>`, `capabilities: Vec<String>`

### `SessionInfo` record (UniFFI)
- Fields: `id`, `title`, `created_at`, `updated_at`

### `ProviderConfig`
- Non-UniFFI internal config struct for connection parameters
- Fields: `websocket_url`, `ssh_host`, `ssh_port`, `remote_port`, `working_dir`, `agent_type`, `client_name`, `client_version`

## DiscoveredServer Extension

`DiscoveredServer` now has an `agent_types: Vec<AgentType>` field that defaults to `vec![AgentType::Codex]` for backward compatibility.

`AppDiscoveredServer` (UniFFI record) also exposes `agent_types: Vec<AgentType>`.

## Event Mapping (provider/mapping.rs)

The mapping module provides the complete event pipeline:

### Functions
- `app_server_event_to_provider_event(server_id, &AppServerEvent) -> ProviderEvent` — top-level envelope mapping
- `server_notification_to_provider_event(server_id, &ServerNotification) -> ProviderEvent` — notification mapping
- `server_request_to_provider_event(server_id, &ServerRequest) -> ProviderEvent` — request mapping
- `provider_event_to_ui_event(server_id, &ProviderEvent) -> Option<UiEvent>` — normalized event → UI event

### Mapping Coverage
- All 24 explicitly-mapped `ServerNotification` variants produce typed `ProviderEvent`s
- Unhandled known notifications (e.g. `SkillsChanged`, `ThreadRealtimeItemAdded`, etc.) map to `ProviderEvent::Unknown` with a warning log
- All `ServerRequest` approval variants map to `ProviderEvent::ApprovalRequested`
- `DynamicToolCall` maps to `ProviderEvent::ToolCallStarted`
- `AppServerEvent::Lagged` → `ProviderEvent::Lagged`, `Disconnected` → `ProviderEvent::Disconnected`
- All 34 `ProviderEvent` variants have explicit handling in `provider_event_to_ui_event`
- `ProviderEvent::Lagged`, `StreamingStarted`, `StreamingCompleted` return `None` (internal-only)
- All serde round-trips verified for all 34 `ProviderEvent` variants

### Tests
- 27 tests in `provider::mapping::tests`
- Run: `cargo test --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml --lib -- provider::mapping`

## Build Notes

- `cargo check` and `cargo test --lib` pass (447 tests total, 27 mapping tests)
- Clippy has pre-existing error in `ffi/app_store.rs` (loop never loops) — not from provider code
- Clippy has pre-existing warnings in `codex-ipc`, `store/voice.rs`, `store/reducer.rs` — not from provider code
- UniFFI binding regeneration requires building the cdylib, which needs a Mac host or proper cross-compilation setup
- The provider module is public (`pub mod provider`) so it can be consumed by other modules

## Upstream Event Mapping Coverage

The upstream `ServerNotification` enum (defined via `server_notification_definitions!` macro in `shared/third_party/codex/codex-rs/app-server-protocol/src/protocol/common.rs`) has ~47 variants. Of these:

- **24 explicitly mapped** to typed `ProviderEvent` variants (see mapping.rs)
- **~23 handled via wildcard** `other =>` arm → `ProviderEvent::Unknown` with warning log

Unmapped known variants include: HookStarted, HookCompleted, ItemGuardianApprovalReviewStarted/Completed, RawResponseItemCompleted, CommandExecOutputDelta, TerminalInteraction, McpServerOauthLoginCompleted, McpServerStatusUpdated, AccountUpdated, AppListUpdated, FsChanged, ReasoningSummaryPartAdded, ContextCompacted, DeprecationNotice, ConfigWarning, FuzzyFileSearchSessionUpdated/Completed, WindowsWorldWritableWarning, WindowsSandboxSetupCompleted, ThreadUnarchived, ThreadClosed.

These can be individually typed as needed in follow-up work. The wildcard approach ensures forward compatibility when upstream adds new variants.

## Broadcast Channel Behavior

`tokio::sync::broadcast::Sender` silently drops events sent before any subscriber exists. This is expected tokio broadcast behavior and is tested explicitly. When implementing providers, ensure the event reader subscribes before any events could be emitted.

## Future Work
- Implementations: ACP, Pi, Droid providers in submodules under `src/provider/`
- Adapt reconnect retry loop logic for the provider trait (currently reconnect_remote_client operates on AppServerClient)
- Add config change rejection logic to real CodexProvider (currently only in ErrorMockProvider)

## CodexProvider (provider/codex.rs)

Wraps existing `AppServerClient` behind `ProviderTransport`. Creates an internal worker task that drives the client's event loop and multiplexes commands.

### Construction
- `CodexProvider::new(client: AppServerClient)` — wraps an already-connected client
- `create_codex_provider(client)` — factory function returning `Box<dyn ProviderTransport>`
- `create_provider_for_agent_type(agent_type)` — factory that rejects non-Codex types

### Worker Architecture
- The worker task runs a `tokio::select!` loop:
  - Commands from `ProviderCommand` channel (Request, Notification, ResolveServerRequest, RejectServerRequest, Shutdown)
  - Events from `client.next_event()` mapped through `app_server_event_to_provider_event()`
- Events are broadcast via internal `broadcast::Sender<ProviderEvent>`

### Key Behaviors
- `connect()` is a no-op (client is connected at construction time)
- `disconnect()` sends Shutdown command and aborts worker handle (idempotent)
- `send_request()` serializes method+params to JSON, builds ClientRequest, sends to worker
- `send_notification()` similarly serializes to ClientNotification
- `list_sessions()` delegates to `send_request("thread/list", ...)` and parses response
- Post-disconnect `send_request` returns `Err(RpcError::Transport(TransportError::Disconnected))`

## ServerSession Provider Path (session/connection.rs)

### from_provider constructor
- `ServerSession::from_provider(config, Box<dyn ProviderTransport>)` — creates a session backed by a provider
- The provider is wrapped in `Arc<Mutex<>>` for sharing between command dispatch and event consumption tasks
- Events from `provider.event_receiver()` are mapped to `ServerEvent::LegacyNotification` via `provider_event_to_server_event()`
- Commands (Request, Notify) are serialized and forwarded to `provider.send_request/send_notification()`
- Server request resolution uses synthetic `$resolve`/`$reject` method names (CodexProvider handles these internally)

### provider_event_to_server_event mapping
- Converts `ProviderEvent` variants to `ServerEvent::LegacyNotification` with method names like `codex/event/turnStarted`
- `ProviderEvent::Disconnected` is handled by health transition, not forwarded as ServerEvent
- `ProviderEvent::Unknown` preserves original method name
- This mapping ensures backward compatibility with the existing EventProcessor pipeline

## MobileClient Provider Integration (mobile_client/mod.rs)

### connect_with_provider method
- `MobileClient::connect_with_provider(config, Box<dyn ProviderTransport>)` — creates provider-backed session
- Wires up event reader, health reader, and store updates
- Follows same pattern as `connect_remote()` but uses `ServerSession::from_provider()` internally
- Supports session reuse (returns existing server_id if session is active)

### Agent Type Routing
- `create_provider_for_agent_type(AgentType)` rejects all non-Codex types with `TransportError::ConnectionFailed("unsupported agent type: ...")`
- Codex type also returns error since AppServerClient must be provided directly via `CodexProvider::new()`
- Future milestones will add factory methods for Pi, Droid, ACP providers

## Test Coverage
- 499 total tests pass (468 pre-existing + 31 new error handling tests)
- New tests in `provider::codex::tests` (12 tests): mock provider, factory functions, object safety
- New tests in `session::connection::tests` (5 tests): from_provider lifecycle, request, events, disconnect, notify
- New tests in `mobile_client::tests` (4 tests): connect_with_provider, session reuse, unsupported agent type rejection
- New tests in `provider::error_handling::tests` (31 tests): comprehensive error handling coverage

## Provider Error Handling (provider/error_handling.rs)

### ErrorMockProvider
- Configurable mock provider for testing all error handling scenarios
- Uses `Arc<MockProviderState>` for shared state access behind `Box<dyn ProviderTransport>`
- Supports error injection: connect failure, handshake timeout, streaming state, config change rejection
- Config changes (model, reasoning_effort) via `thread/update` or `config/change`:
  - Rejected during active streaming with `RpcError::Server { code: -32600 }`
  - Succeed when idle, applied to internal state
  - Server-side rejection (queued error response) prevents state mutation

### Error Handling Contract
- **Mid-stream disconnect**: Emits `ProviderEvent::Disconnected`, no panic (VAL-PROV-012)
- **Reconnection**: `connect()` after `disconnect()` succeeds, provider reusable (VAL-PROV-013)
- **Connect failure**: Returns `TransportError::ConnectionFailed`, provider reusable (VAL-PROV-003)
- **Disconnect idempotent**: Multiple `disconnect()` calls are no-ops (VAL-PROV-004)
- **Post-disconnect**: `send_request`/`send_notification`/`list_sessions` return `TransportError::Disconnected`
- **Handshake timeout**: Returns `TransportError::Timeout`, provider in clean state for retry (VAL-PROV-018)
- **Cancel mid-stream**: Streaming state cleared, next prompt succeeds (VAL-PROV-016)
- **Config change idle**: Model and reasoning_effort changes succeed (VAL-PROV-019)
- **Config change streaming**: Returns error, stream not disrupted, previous config preserved (VAL-PROV-019)
