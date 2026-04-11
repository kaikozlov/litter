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

## Future Work
- `codex-provider-refactor` feature: Create CodexProvider that wraps existing AppServerAdapter
- Implementations: ACP, Pi, Droid providers in submodules under `src/provider/`
