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
- Fields: `websocket_url`, `ssh_host`, `ssh_port`, `remote_port`, `working_dir`, `agent_type`, `client_name`, `client_version`, `remote_command`
- `remote_command: Option<String>` — shell command for launching the agent's ACP server over SSH (used by `GenericAcp` and `DroidAcp` factory paths). Defaults to `None`.
- The FFI bridge (`SshBridge`) passes `remoteCommand: String?` through `ssh_connect_remote_server` and `ssh_start_remote_server_connect` to `MobileClient::connect_remote_over_ssh_with_agent_type`

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

## Droid ACP Transport (Stream-JSON)

### Architecture
- `DroidAcpTransport` implements `ProviderTransport` over Droid's `--output-format stream-json` protocol
- Connects via SSH → spawns `droid exec --output-format stream-json --input-format stream-json` → streaming JSON over stdin/stdout
- NOT the same as the standard ACP JSON-RPC protocol — uses Droid's own streaming JSON format

### Module Structure
- `provider/droid/acp_transport.rs` — `DroidAcpTransport` implementing `ProviderTransport`
- `provider/droid/stream_json.rs` — Droid stream-json protocol types (`StreamMessage` enum)

### Stream-JSON Protocol
Droid's `--output-format stream-json` emits newline-delimited JSON messages:
- `{"type":"system","subtype":"init","session_id":"...","model":"...","tools":[...]}` — Session init
- `{"type":"message","role":"user/assistant","text":"..."}` — Messages
- `{"type":"tool_call","toolName":"...","parameters":{...}}` — Tool invocations
- `{"type":"tool_result","value":"...","isError":false}` — Tool results
- `{"type":"completion","finalText":"...","usage":{...}}` — Session completion

Input: `{"type":"user_message","text":"..."}\n` via stdin

### Event Mapping
| Droid StreamMessage | ProviderEvent |
|---|---|
| `system/init` | `StreamingStarted` |
| `message` (role=assistant) | `ItemStarted` → `MessageDelta` → `ItemCompleted` |
| `tool_call` | `ToolCallStarted` + `CommandOutputDelta` |
| `tool_result` | `CommandOutputDelta` + `ToolCallUpdate` |
| `completion` | `StreamingCompleted` |

### Permission Auto-Handling
- Maps Droid autonomy levels to `AgentPermissionPolicy`:
  - `Suggest` → `PromptAlways`
  - `Normal` → `PromptAlways`
  - `Full` → `AutoApproveAll`

### MCP Config
- `generate_mcp_config()` generates a session-scoped `.factory/mcp.json` string
- `build_spawn_command()` constructs the `droid exec` command with appropriate flags

### Test Coverage
- 24 unit tests in `acp_transport::tests` (all pass)
- 10 protocol tests in `stream_json::tests` (all pass)
- 3 E2E tests against gvps (marked `#[ignore]`, all pass):
  - `e2e_droid_acp_stream_json_init` — connect, init, streaming response
  - `e2e_droid_acp_stream_json_tool_use` — tool call with file read
  - `e2e_droid_acp_full_pipeline` — full ProviderEvent pipeline verification
- Full suite: 800 passed; 0 failed; 6 ignored

## Droid Native Transport (Factory API JSON-RPC 2.0)

### Architecture
- `DroidNativeTransport` implements `ProviderTransport` over Droid's native Factory API
- Connects via SSH → spawns `droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc` → JSON-RPC 2.0 over NDJSON
- Distinct from the stream-json protocol (used by `DroidAcpTransport`) — this is full JSON-RPC 2.0 with methods and notifications

### Module Structure
- `provider/droid/protocol.rs` — Droid JSON-RPC 2.0 protocol types: `JsonRpcRequest`, `JsonRpcResponse`, `JsonRpcNotification`, Droid-specific notification variants (`WorkingStateChanged`, `CreateMessage`, `ToolResult`, `Error`, `Complete`)
- `provider/droid/transport.rs` — `DroidNativeTransport` implementing `ProviderTransport`, manages bidirectional stream lifecycle, JSON-RPC request/response correlation, and Droid notification → ProviderEvent mapping
- `provider/droid/mock.rs` — `MockDroidChannel` implementing `AsyncRead + AsyncWrite` for in-memory testing with response queuing and write capture
- `provider/droid/detection.rs` — Auto-detection of `droid` binary on SSH host

### Droid JSON-RPC 2.0 Protocol
- **Request**: `droid.initialize_session` — returns session ID, model info, available models
- **Request**: `droid.add_user_message` — sends prompt, streams response via notifications
- **Notification**: `droid_working_state_changed` — idle/streaming status transitions
- **Notification**: `create_message` — incremental assistant text deltas
- **Notification**: `tool_result` — tool call results with name, input, output
- **Notification**: `error` — error events with message and optional code
- **Notification**: `complete` — session turn completion

### Idle-Before-Complete Quirk
Droid sends `droid_working_state_changed(idle)` *before* the `complete` notification. The transport handles this with deferred completion logic: when `idle` is received during active streaming, it waits for the subsequent `complete` notification before emitting `StreamingCompleted`.

### Event Mapping
| Droid Notification | ProviderEvent |
|---|---|
| `droid_working_state_changed(streaming)` | `WorkingStateChanged(Streaming)` |
| `droid_working_state_changed(idle)` | Deferred: waits for `complete` |
| `create_message` (text deltas) | `MessageDelta` |
| `tool_result` | `ToolCallStarted` → `CommandOutputDelta` → `ToolCallUpdate` |
| `error` | `ProviderEvent::Error` |
| `complete` (with deferred idle) | `WorkingStateChanged(Idle)` + `StreamingCompleted` |

### Droid-Specific Features
- **Autonomy levels**: `Suggest`/`Normal`/`Full` → mapped to approval policies
- **Model selection**: passed through to `droid.initialize_session`, not validated locally
- **Reasoning effort**: `Low`/`Medium`/`High` → forwarded to Droid API
- **Session fork/resume**: supported via session ID tracking
- **FACTORY_API_KEY**: read from remote SSH host environment, passed in API requests, never logged or exposed in error messages

### Auto-Detection
- Probes SSH host for `droid` binary at: `$HOME/.bun/bin/droid`, `/usr/local/bin/droid`, `$HOME/.cargo/bin/droid`, and via `command -v droid`
- Returns `Some(path)` when found, `None` when absent
- Does not block Codex binary detection — both probes run independently

### Error Handling
- **Process crash**: Stream EOF → `ProviderEvent::Disconnected`, partial assistant messages discarded with interrupted-message error
- **Transient errors** (429): Retry up to 3 times with exponential backoff
- **Permanent errors** (400/401/403/404): Surface immediately with correct code and message
- **API errors**: Mapped to `ProviderEvent::Error` with parsed code and message
- **Post-disconnect requests**: Return `Err(TransportError::Disconnected)`
- **Disconnect idempotent**: Multiple disconnect calls are no-ops

### Test Coverage
- 75 unit tests in `transport::tests`, `protocol::tests`, `detection::tests` (all pass)
- Full suite: 800 passed; 0 failed; 6 ignored
- VAL-DROID-002 through VAL-DROID-012, VAL-DROID-016 covered

### Known Non-Blocking Issues
- `is_connected()` returns true on Mutex contention (conservative default)
- `blocking_lock()` used as fallback in subscribe/event_receiver
- Transient retry test doesn't fully validate same-request retry semantics
- `$HOME` shell expansion in detection probe may fail on non-standard shells

## Cross-Provider Integration Tests (provider/cross_provider.rs)

### Test Coverage (22 tests)
- **Codex regression**: connect-through-prompt + approval flow through provider trait (VAL-CROSS-001, VAL-CROSS-002)
- **Multi-provider concurrent sessions**: three providers streaming simultaneously with isolated event attribution (VAL-CROSS-006, VAL-CROSS-016)
- **Session history aggregation**: sessions from all providers collected with unique IDs (VAL-CROSS-010)
- **Store/reducer parity**: identical ProviderEvent produces identical hydrated output regardless of source (VAL-CROSS-012)
- **Unicode content parity**: emoji, CJK, RTL preserved identically across Codex/Pi/Droid providers (VAL-CROSS-017)
- **Transport fallback**: native → ACP and ACP → native fallback flows (VAL-CROSS-013)
- **Large response handling**: 1000+ command output deltas + 500+ Pi text chunks (VAL-PROV-017)
- **App background simulation**: state preserved across background/foreground cycle (VAL-CROSS-019)
- **Network change simulation**: disconnect → reconnect → resume streaming (VAL-CROSS-020)
- **SSH keepalive**: idle connection still functional after delay (VAL-PROV-020)
- **Multi-agent SSH channel isolation**: Pi and Droid channels fully independent (VAL-CROSS-018)
- **Disconnect isolation**: one provider crash doesn't affect others (VAL-CROSS-008)
- **Session ID collision**: ThreadKey distinguishes by server_id (VAL-CROSS-021)
- **ProviderTransport object safety**: all transport types work as `Box<dyn ProviderTransport>`

### Test Infrastructure
- `SequencedMockProvider`: lightweight mock with broadcast event channel (4096 capacity)
- `emit_streaming_sequence()`: helper for standard message streaming
- `emit_command_sequence()`: helper for tool call lifecycle
- `emit_approval_sequence()`: helper for approval request/resolution
- `collect_events()`: async helper with 200ms timeout for event collection (unit tests use 200ms, E2E mock transport tests use 500ms due to async overhead)
- `ErrorMockProvider`: mock that simulates transport errors on specific methods

### Pi Native Transport Notes
- `PiNativeTransport::list_sessions()` returns an empty `Vec` because Pi session listing requires SSH file I/O (reading `~/.pi/agent/sessions/*.jsonl`), not the native RPC protocol. Session listing is handled at a higher level via the SSH bridge, not the transport layer.

### Ignored E2E Placeholder Tests
- Several `#[ignore]` tests (e.g., `droid_e2e_gvps_*`, `pi_e2e_gvps_*`) are placeholder functions with `println!` statements that document expected real E2E flows against gvps. Running `cargo test -- --ignored` will show these pass without testing anything. These should be replaced with real SSH-based E2E tests or removed.

### Test Count
- Full suite: 868 passed; 0 failed; 9 ignored
