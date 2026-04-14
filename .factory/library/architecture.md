# Architecture: Multi-Provider Pluggable Architecture

## Overview

This mission refactors Litter's provider layer so all AI agents are first-class citizens through the same `ProviderTransport` abstraction. Currently the main session lifecycle bypasses the provider trait and talks directly to Codex's `AppServerClient`. After this mission, every provider (Codex, Pi, Droid, generic ACP) routes through `ProviderTransport` → `ServerSession` → `MobileClient`.

## Provider Abstraction Layer

### Core Trait: `ProviderTransport`
- Location: `src/provider/mod.rs`
- Object-safe, `Send + Sync + 'static`
- Methods: `connect`, `disconnect`, `send_request`, `send_notification`, `next_event`, `event_receiver`, `list_sessions`, `is_connected`
- Used as `Box<dyn ProviderTransport>` throughout session layer

### Agent Types (6 variants)
- `Codex` — Codex app-server (WebSocket/IPC/in-process/SSH tunnel)
- `PiNative` — Pi native JSONL RPC (`pi --mode rpc`)
- `PiAcp` — Pi over standard ACP (`npx pi-acp`)
- `DroidNative` — Droid Factory API JSON-RPC
- `DroidAcp` — Droid over standard ACP (`droid exec --output-format acp`) — **replacing proprietary stream-json**
- `GenericAcp` — Any ACP-compatible agent with configurable command

### Event Pipeline
```
Upstream Protocol (Codex JSON-RPC / ACP JSON-RPC / Pi JSONL)
  → ProviderTransport::event_receiver()
    → ProviderEvent (34 variants, normalized)
      → provider_event_to_ui_event()
        → UiEvent (consumed by AppStore reducer)
```

## Session Lifecycle (Target Architecture)

### Codex Path (refactored through trait)
```
MobileClient::connect_remote_over_ssh()
  → SSH bootstrap → tunnel → AppServerClient
    → CodexProvider::new(client)  // wraps as ProviderTransport
      → ServerSession::from_provider(config, Box::new(provider))
        → MobileClient::connect_with_provider()  // wires event/health readers
```

### Non-Codex Path (through trait)
```
MobileClient::connect_remote_over_ssh_with_agent_type(agent_type)
  → SSH connect → validate agent binary
    → create_provider_over_ssh(agent_type, ssh_client, config)
      → ServerSession::from_provider(config, Box::new(provider))
        → MobileClient::connect_with_provider()
```

### Key Point: Both paths converge at `ServerSession::from_provider`

## New Components

### CodexProvider (`src/provider/codex.rs`)
- Wraps existing `AppServerClient` as `ProviderTransport`
- Worker task forwards `AppServerEvent` → `ProviderEvent` via broadcast channel
- No behavior change — all existing Codex flows preserved

### GenericAcpTransport (`src/provider/generic_acp.rs` — new)
- Thin wrapper around `AcpClient` (like PiAcpTransport)
- Command comes from `ProviderConfig.remote_command`
- Full ACP lifecycle: initialize → authenticate → session/new → prompt → cancel → list → load
- Codex wire method adapters: thread/start → session/new + session/prompt, etc.

### AcpClient (`src/provider/acp/client.rs`)
- Already implements full ACP protocol lifecycle
- NDJSON framing over any `AsyncRead + AsyncWrite` stream
- Used by PiAcp, GenericAcp, and now DroidAcp (standard ACP)

## DroidAcp Migration

### Before (proprietary stream-json)
```
droid exec --output-format stream-json --input-format stream-json
  → Custom DroidAcpTransport with proprietary I/O loop
```

### After (standard ACP)
```
droid exec --output-format acp
  → Standard AcpClient (same as PiAcp)
  → Same event pipeline, same ProviderEvent variants
```

## Discovery Generalization

### Field Renames
- `codex_port` → `agent_port`, `codex_ports` → `agent_ports`
- `has_codex_server` → `has_agent_server`
- `preferred_codex_port` → `preferred_agent_port`
- `PreferredConnectionMode.directCodex` → `.direct`
- `AppConnectionStepKind::FindingCodex` → `FindingAgent`, etc.

### Port Registry
- Replace hardcoded `DEFAULT_SCAN_PORTS` with per-agent-type port mapping
- Codex: [8390], Pi: [9234], SSH: [22]
- Default scan = union of all registered ports

## iOS Changes

### New UI Components
- ACP profile management in Settings (ACPProfile, ACPProfileStore)
- Custom command text field in agent picker
- GenericAcp badge (gearshape.fill, gray)

### String Cleanup
- All hardcoded "Codex" → provider-agnostic text
- ActivityKit: CodexTurnAttributes → TurnAttributes
- Voice: "Codex" speaker label → agent display name

### Codable Migration
- SavedServer decodes both old keys (hasCodexServer, codexPorts, directCodex) and new keys
- Encodes only new keys
- Zero data loss on upgrade

## What's NOT Changing
- Store/reducer logic — already processes UiEvent regardless of provider
- Hydration pipeline — ProviderEvent → UiEvent → HydratedConversationItem
- Android — deferred (text-only field renames, no build)
- shared/third_party/codex submodule — read-only
