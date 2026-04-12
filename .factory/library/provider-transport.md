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

## ACP Client Transport (provider/acp/)

### NDJSON Framing (provider/acp/framing.rs)
- `decode_line(line: &str)` — synchronous line-level decoding, returns `Option<DecodedLine>` (ClientMessage, AgentNotification, AgentRequest, Skipped)
- `NdjsonReader<R: AsyncRead>` — async buffered reader that splits incoming byte stream on newline boundaries
- `serialize_client_message()` / `deserialize_client_message()` — helpers for all client-to-agent message types
- Handles: empty lines (skip), invalid JSON (skip + warn), partial messages (buffered), large messages (>100KB), null bytes, binary garbage, multi-byte UTF-8 split across reads

### Mock Transport (provider/acp/mock.rs)
- `MockTransport` implements `AsyncRead + AsyncWrite` for in-memory bidirectional I/O
- Response queuing: `queue_response(data)` for sequential reads
- Write capture: `sent_data()` returns all bytes written by client
- Disconnect simulation: `simulate_disconnect()` closes the read side
- Reset: `reset()` clears captured data for reuse across test cases

### ACP Client Lifecycle (provider/acp/client.rs)
- `AcpClient` — main client struct managing the full ACP protocol lifecycle
- States: `Uninitialized` → `Initialized` → `Authenticated` → (session operations)
- `initialize()` — version negotiation, capabilities exchange
- `authenticate()` — credential exchange with transient retry (1 retry) and permanent failure (no retry)
- `session_new()` — creates new session with cwd and options
- `session_prompt()` — sends prompt, streams response via event channel, resolves when complete
- `session_cancel()` — terminates active prompt; no-op when idle
- `session_list()` — returns available sessions (empty array if none)
- `session_load()` — loads existing session by ID, error if not found
- `subscribe()` — returns `broadcast::Receiver<ProviderEvent>` for event streaming
- Internal: single-task `io_loop` combining read and write, avoiding transport split-ownership
- **Gotcha**: `AgentResponse` uses `#[serde(untagged)]`, causing deserialization ambiguity. The client tracks pending method names (`HashMap<u64, String>`) and uses method-aware deserialization in `process_raw_line()` to work around this.

### Streaming Event Mapping (provider/acp/mapping.rs)
- `AgentMessageChunk` → `ProviderEvent::MessageDelta`
- `AgentThoughtChunk` → `ProviderEvent::ReasoningDelta`
- `ToolCall` → `ProviderEvent::ToolCallStarted`
- `ToolCallUpdate` → `ProviderEvent::ToolCallUpdate`
- `Plan` → `ProviderEvent::PlanUpdated`

### Client-Side Request Handlers (provider/acp/handlers.rs)
- `fs/read_text_file` — reads file via injectable `FsDelegate` trait; returns "unsupported" (-32601) if no delegate configured
- `fs/write_text_file` — writes file via `FsDelegate`; returns "unsupported" by default
- `request_permission` — applies `AgentPermissionPolicy`: AutoApproveAll (auto-resolve), AutoRejectHighRisk (auto-reject), PromptAlways (emit `ProviderEvent::ApprovalRequested`)
- `terminal/*` — returns MethodNotFound (-32601) for all terminal operations (mobile-unsupported)
- Handlers are implemented and tested but **not yet wired** into the ACP client I/O loop (agent requests are logged but not dispatched)

### AgentPermissionPolicy (UniFFI enum)
- `AutoApproveAll` — automatically approve all permission requests
- `AutoRejectHighRisk` — automatically reject high-risk operations
- `PromptAlways` (Default) — surface request to platform layer via ProviderEvent
- Derives: `Debug, Clone, PartialEq, Eq, uniffi::Enum, serde::Serialize, serde::Deserialize`

### Test Coverage
- 80+ ACP-specific tests across framing (28), lifecycle (20), streaming/handlers (32)
- Full suite: 581 passed; 0 failed; 3 ignored

## Pi Native Transport (provider/pi/)

### Module Structure
- `protocol.rs` — Pi RPC protocol types: `PiCommand` (8 variants: prompt, abort, get_state, set_model, get_available_models, set_thinking_level, set_steering_mode, compact), `PiEvent` (15 variants), `PiThinkingLevel` (6 levels: off/minimal/low/medium/high/xhigh), JSONL serialization/deserialization
- `transport.rs` — `PiNativeTransport` implementing `ProviderTransport`, manages bidirectional stream lifecycle, JSONL framing, Pi event → ProviderEvent mapping, and command response correlation
- `mock.rs` — `MockPiChannel` implementing `AsyncRead + AsyncWrite` for in-memory testing with `Notify`-based async wake signaling

### PiNativeTransport Architecture
- Created with `PiNativeTransport::new(stream)` where `stream: AsyncRead + AsyncWrite`
- Spawns a single IO loop task that reads JSONL lines and writes outgoing commands
- Uses `broadcast::Sender<ProviderEvent>` for event distribution
- Tracks pending commands by type for response correlation (e.g., `get_state`, `get_available_models`)
- Command timeout: 30 seconds

### Pi Event Mapping
| Pi Event | ProviderEvent |
|---|---|
| `agent_start` | `StreamingStarted` |
| `agent_end` | `StreamingCompleted` |
| `turn_start` / `turn_end` | `TurnStarted` / `TurnCompleted` |
| `message_start` / `message_end` | `ItemStarted` / `ItemCompleted` |
| `message_update` (text_delta) | `MessageDelta` |
| `message_update` (thinking_delta) | `ReasoningDelta` |
| `toolcall_start` | `ToolCallStarted` |
| `toolcall_delta` / `toolcall_end` | `ToolCallUpdate` |
| `tool_execution_start` | `ToolCallStarted` |
| `tool_execution_update` (stdout) | `CommandOutputDelta` |
| `tool_execution_update` (stderr) | `CommandOutputDelta` (prefixed `[stderr]`) |
| `tool_execution_end` | `ToolCallUpdate` (with exit code) |
| `response` (correlated) | Resolves pending command |
| `error` | `ProviderEvent::Error` |

### Pi RPC Commands (via ProviderTransport::send_request)
- `prompt` — sends prompt, streams events via broadcast
- `abort` — cancels active prompt
- `set_thinking_level` — maps to 6 levels (off/minimal/low/medium/high/xhigh)
- `set_model` — changes model for next prompt
- `get_available_models` — returns model list
- `get_state` — queries current session state
- `set_steering_mode` — sets steering mode
- `compact` — compacts session context

### Error Handling
- **Pi process crash**: Stream EOF → `ProviderEvent::Disconnected` emitted, transport marks disconnected
- **Malformed JSONL**: Log warning, skip line, continue processing
- **Unknown event type**: Serde fails → logged as warning, processing continues
- **Pi binary not found**: Empty stream → immediate EOF → `ProviderEvent::Disconnected`
- **Post-disconnect requests**: Return `Err(TransportError::Disconnected)`
- **Disconnect idempotent**: Multiple disconnect calls are no-ops

### Test Coverage
- 54 Pi-specific tests: protocol (28), transport (21), mock (5)
- Full suite: 692 passed; 0 failed; 3 ignored

## Pi ACP Transport

### Architecture
- `PiAcpTransport` wraps the universal `AcpClient` to implement `ProviderTransport`
- Connects via SSH → spawns `npx pi-acp` → ACP JSON-RPC over NDJSON
- Uses `Arc<Mutex<AcpClient>>` internally for async access

### Key Design Decisions
- `next_event()` always returns `None` — all event consumption goes through `event_receiver()` since AcpClient doesn't expose a non-blocking peek
- No transport-level reconnection — reconnection requires creating a new `PiAcpTransport` instance at the MobileClient/ServerSession level
- Uses `Arc<Mutex<bool>>` for `initialized`/`disconnected` flags (unlike PiNativeTransport which uses `Arc<AtomicBool>`)

### Pi Auto-Detection
- Module: `provider/pi/detection.rs`
- Three probe functions: `probe_pi_binary` (which pi), `probe_pi_version` (pi --version), `probe_pi_acp` (npx pi-acp --version)
- Returns `DetectedPiAgent` with flags for native and ACP availability
- Probe functions require real SSH — no direct unit test coverage

### Pi Session Persistence
- Module: `provider/pi/sessions.rs`
- Reads `~/.pi/agent/sessions/*.jsonl` files on remote host
- `SessionInfo.created_at` and `updated_at` are empty strings (Pi doesn't store timestamps in a standard format)

### Pi Transport Preference
- Module: `provider/pi/preference.rs`
- 4 modes: `PreferNative`, `PreferAcp`, `ForceNative`, `ForceAcp`
- Native preferred by default; falls back to ACP if native unavailable

### Test Coverage (ACP)
- 17 ACP transport tests, 4 detection tests, 11 session tests, 14 preference tests
- Full suite: 692 passed; 0 failed; 3 ignored

### Known Testing Gotchas
- Combined lifecycle test (init → auth → session/new → prompt → list in single test) hangs on Linux with `Arc<Mutex<AcpClient>>` mock pattern due to tokio scheduling. Split into focused individual tests.
- `cargo test` (default) fails on Linux due to V8 TLS relocations with cdylib crate type — use `cargo test --lib` instead
