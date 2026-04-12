# Architecture: Multi-Agent Support

## Overview

Litter connects mobile devices (iOS/Android) to remote coding agents. The shared Rust layer (`codex-mobile-client`) handles all agent communication, state management, and protocol translation. Platform code (Swift/Kotlin) only handles UI rendering and platform-specific services.

## Provider Architecture

### Provider Transport Trait
All agent communication goes through a `ProviderTransport` trait that abstracts:
- Connection establishment (SSH+PTY, WebSocket, in-process)
- Request/response RPC
- Event streaming (server-sent notifications)
- Session lifecycle (list, load, resume)
- Disconnection and reconnection

### Provider Event Pipeline
```
Agent Protocol → ProviderTransport → ProviderEvent → UiEvent → AppStoreReducer → HydratedConversationItem → Platform UI
```

### Agent Types
- **Codex**: Existing Codex app-server protocol (JSON-RPC over WebSocket). Two modes: in-process and remote via SSH tunnel.
- **Pi (ACP)**: Via `pi-acp` adapter — ACP JSON-RPC over SSH PTY
- **Pi (Native)**: Direct Pi JSONL RPC over SSH PTY (`pi --mode rpc`)
- **Droid (ACP)**: Via `droid exec --output-format acp` — native ACP over SSH PTY
- **Droid (Native)**: Via `droid exec --stream-jsonrpc` — Factory API JSON-RPC over SSH PTY
- **Generic ACP**: Any ACP-compatible agent over SSH PTY

### Key Modules
- `src/provider/` — Provider trait, ProviderEvent enum, agent type definitions
- `src/provider/codex.rs` — Codex provider (wraps existing AppServerClient)
- `src/provider/mapping.rs` — ServerNotification/ServerRequest → ProviderEvent → UiEvent mapping
- `src/provider/error_handling.rs` — Error handling tests with ErrorMockProvider
- `src/provider/acp/` — (Future) Universal ACP client
- `src/provider/pi/` — (Future) Pi native RPC client
- `src/provider/droid/` — (Future) Droid native Factory API client
- `src/transport/` — Transport error types, connection state (unchanged)
- `src/session/` — ServerSession refactored to use ProviderTransport
- `src/store/` — AppStoreReducer (unchanged — receives UiEvent regardless of provider)

### Hydration Mapping
All providers map their protocol-specific events to the same HydratedConversationItem types:
- Text streaming → AssistantMessageData
- Thinking → ReasoningData
- Tool calls → CommandExecutionData / McpToolCallData / DynamicToolCallData
- Plans → ProposedPlanData
- File changes → FileChangeData
- Approvals → UserInputResponseData
