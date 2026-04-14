# Architecture: Multi-Agent Connection Routing

## Overview

This mission wires the existing provider layer into the connection flow. The provider transports (PiNativeTransport, PiAcpTransport, DroidNativeTransport, DroidAcpTransport) are fully built. The gap is the routing bridge between the user's agent selection and the transport instantiation.

## Connection Flow (Current — Codex Only)

```
iOS DiscoveryView tap
  → connectToServer() → connectViaSSH()
    → appModel.ssh.sshStartRemoteServerConnect() (FFI)
      → Rust SshBridge::ssh_start_remote_server_connect()
        → run_guided_ssh_connect()
          → ssh_client.bootstrap_codex_server()
          → finish_connect_remote_over_ssh() (WebSocket tunnel)
            → ServerSession::connect_remote()
```

## Connection Flow (After — Agent-Type Aware)

```
iOS DiscoveryView tap
  → AgentSelectionStore.shared.selectedAgentType(for: serverId)
  → connectToServer(agentType) → connectViaSSH(agentType)
    → appModel.ssh.sshStartRemoteServerConnect(agentType) (FFI)
      → Rust SshBridge::ssh_start_remote_server_connect(agent_type)
        → if None/Codex: run_guided_ssh_connect() (unchanged)
        → if Pi/Droid:
          → connect_remote_over_ssh_with_agent_type()
            → ssh_client.exec_stream("pi --mode rpc") or equivalent
            → create_provider_over_ssh(agent_type, ssh_client, config)
            → MobileClient::connect_with_provider(config, provider)
              → ServerSession::from_provider()
```

## New Components

### SshClient::exec_stream()
- Location: `src/ssh.rs`
- Pattern: follows existing `exec_inner()` — opens SSH channel, drops handle lock, calls exec, returns `ChannelStream`
- Returns: `Pin<Box<dyn AsyncReadWrite>>` where `AsyncReadWrite` is a local trait alias for `AsyncRead + AsyncWrite + Send`
- **Type compatibility contract**: The returned `Pin<Box<dyn AsyncReadWrite>>` is directly compatible with all transport constructors that accept `T: AsyncRead + AsyncWrite + Send + 'static` (works via `Deref` on `Pin<Box<dyn X>>`). Future transport implementers can pass `exec_stream()` output directly to `Transport::new()`.
- **Stderr limitation**: russh's `ChannelStream` (from `into_stream()`) only reads stdout (`ChannelMsg::Data`) on its `AsyncRead` side. Stderr (`ExtendedData { ext: 1 }`) frames are handled internally by russh and are **not accessible** through the stream's read side. If stderr access is needed, a different approach (e.g., separate channel or `exec_inner()` with raw channel handling) would be required.
- Enables: spawning remote agent binaries with bidirectional I/O

### Provider Factory
- Location: `src/provider/factory.rs` (new)
- Function: `create_provider_over_ssh(agent_type, ssh_client, config) → Result<Box<dyn ProviderTransport>>`
- Spawns correct binary per agent type:
  - PiNative: `pi --mode rpc`
  - PiAcp: `npx pi-acp` (then ACP handshake)
  - DroidNative: `droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc`
  - DroidAcp: `droid exec --output-format stream-json --input-format stream-json`
  - GenericAcp: configurable command from ProviderConfig
  - Codex: error (must use standard path)
- **ACP initialization patterns differ by transport**:
  - PiAcp uses an explicit `handshake()` method (ACP `initialize` + `authenticate` protocol)
  - DroidAcp uses background task polling — the transport's IO loop reads `system/init` from the stream and sets internal state; the factory polls `transport.session_id()` until it becomes `Some(...)`
  - Future transport implementers need to choose the appropriate pattern based on the agent's protocol

### Agent-Type Routing
- Location: `src/mobile_client/mod.rs`
- Method: `connect_remote_over_ssh_with_agent_type()`
- Routes: Codex → existing path, Pi/Droid → provider factory → connect_with_provider()

### FFI Parameter
- Location: `src/ffi/ssh.rs`
- Added: `agent_type: Option<AgentType>` to `ssh_start_remote_server_connect()`
- Backward compatible: None = existing Codex behavior

### Codex Wire Method Mapping
- Location: Each transport's `send_request()` method
- Maps Codex wire methods (thread/start, turn/start, turn/interrupt, thread/list, etc.) to provider-native methods
- Ensures existing FFI methods (start_thread, list_threads, etc.) work transparently

#### Method Mapping Details
- **extract_text_from_params()** — Shared helper (duplicated in pi/transport.rs and droid/transport.rs) that extracts user text from Codex wire params with priority ordering: `items[].content` → `content` field → `text` field. ACP transports import from droid/transport.rs.
- **Thread ID formats** — PiNativeTransport generates `pi-thread-{uuid}` for synthetic responses; DroidNativeTransport uses the Droid session ID; DroidAcpTransport uses session ID from `system/init` or generates UUID fallback.
- **No-op methods** — `thread/archive`, `thread/rollback`, `thread/name/set` return `{"ok": true}` without checking connection state. `collaborationMode/list` returns `{"data": []}`. This is intentional per spec.
- **Unknown methods** — PiNativeTransport and DroidNativeTransport return `-32601` (method not found). DroidAcpTransport and PiAcpTransport rely on the error fallback.
- **ACP client lifecycle** — AcpClient checks its own internal `LifecycleState::Authenticated` before allowing session operations. The transport's `initialized` flag (Arc<Mutex<bool>>) is separate from AcpClient's internal state. Tests must perform full ACP handshake mocking; simply setting `initialized` is insufficient.
- **Parity gap** — DroidAcpTransport is missing a `thread/read` adapter (falls through to error fallback). PiAcpTransport correctly implements `thread/read` → `session/load`. This should be addressed in a follow-up.

## Request Routing Chain

```
AppClient::start_thread() (FFI)
  → MobileClient::request_typed_for_server(ClientRequest::ThreadStart)
    → ServerSession::request_client(request)
      → serialize ClientRequest → extract "method" + "params"
        → provider.send_request("thread/start", params)
          → [method mapping layer] → transport.native_method()
```

## What's NOT Changing

- Store/reducer logic — receives UiEvent regardless of provider
- Hydration pipeline — ProviderEvent → UiEvent → HydratedConversationItem
- Codex connection flow — completely unchanged
- IPC — provider sessions don't use IPC (no IPC socket, no tunnel)
- Reconnection — deferred (provider sessions won't auto-reconnect)
- Android — deferred to follow-up mission
