---
name: rust-backend-worker
description: Implements Rust backend features in shared/rust-bridge/codex-mobile-client/ — provider traits, ACP client, Pi/Droid providers, protocol logic
---

# Rust Backend Worker

NOTE: Startup and cleanup are handled by `worker-base`. This skill defines the WORK PROCEDURE.

## When to Use This Skill

Features that involve:
- Provider trait definition and implementation
- ACP client transport and protocol logic
- Pi or Droid native protocol clients
- Event mapping and hydration
- UniFFI type definitions for new provider types
- SSH transport for remote agent spawning
- Any Rust code in `shared/rust-bridge/codex-mobile-client/src/`

## Required Skills

None. All work is done with standard Rust tooling (cargo test, cargo check).

## Work Procedure

### 1. Understand the Feature
Read the feature description carefully. Identify:
- Which module(s) in `src/` are affected
- What new types need to be created
- What existing types need extension
- What the protocol mapping looks like (provider events → ProviderEvent → UiEvent)

### 2. Write Tests First (Red)
Before ANY implementation:
- Create test module or extend existing tests
- Write failing tests that define expected behavior
- Cover: happy path, error cases, edge cases (disconnect, timeout, malformed data)
- For providers: mock transport that exchanges in-memory messages
- For hydration: test each event type maps to correct HydratedConversationItem variant
- For E2E against gvps: mark tests with `#[ignore]` and note SSH requirement

### 3. Implement (Green)
- Follow the provider architecture: trait in `src/provider/mod.rs`, implementations in submodules
- Use `agent-client-protocol-schema` types for ACP serialization
- Map protocol events through `ProviderEvent` → `UiEvent` pipeline
- All new UniFFI types must use proper derive macros
- Reuse existing `TransportError`/`RpcError` for error handling
- Reuse existing `SshClient` for SSH connections (do not create a new SSH layer)

### 4. Verify
Run these commands and ensure all pass:
```bash
cargo check --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml
cargo test --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml
cargo clippy --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml -- -D warnings
```

### 5. If UniFFI Types Changed
If you added or modified any `#[derive(uniffi::Record)]` or `#[derive(uniffi::Enum)]` types:
```bash
./shared/rust-bridge/generate-bindings.sh
```
Report this in the handoff — platform workers will need updated bindings.

### 6. E2E Verification (if applicable)
For features involving actual agent connections, test against gvps:
```bash
ssh gvps "pi --version && droid --version"
cargo test --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml -- --ignored
```

## Example Handoff

```json
{
  "salientSummary": "Implemented AcpProviderTransport trait with initialize/session/new/prompt lifecycle. Added NDJSON framing over bidirectional streams with mock transport for testing. All 12 unit tests pass, 2 E2E tests pass against gvps.",
  "whatWasImplemented": "src/provider/acp/mod.rs (transport trait), src/provider/acp/framing.rs (NDJSON codec), src/provider/acp/client.rs (ACP lifecycle), src/provider/acp/hydration.rs (SessionUpdate → ProviderEvent mapping), src/provider/acp/mock.rs (test transport). 12 unit tests + 2 ignored E2E tests.",
  "whatWasLeftUndone": "Terminal/* handlers return MethodNotFound — full implementation deferred to follow-up feature.",
  "verification": {
    "commandsRun": [
      {"command": "cargo test --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml", "exitCode": 0, "observation": "12 tests passed, 0 failed"},
      {"command": "cargo clippy --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml -- -D warnings", "exitCode": 0, "observation": "No warnings"},
      {"command": "cargo test --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml -- --ignored", "exitCode": 0, "observation": "E2E tests passed against gvps"}
    ],
    "interactiveChecks": []
  },
  "tests": {
    "added": [
      {"file": "src/provider/acp/client.rs", "cases": [{"name": "test_acp_initialize_handshake", "verifies": "ACP initialize with version negotiation"}, {"name": "test_acp_session_lifecycle", "verifies": "session/new → prompt → stream → cancel"}, {"name": "test_acp_ndjson_framing", "verifies": "NDJSON serialization and deserialization"}]}
    ]
  },
  "discoveredIssues": []
}
```

## When to Return to Orchestrator

- Feature requires new UniFFI types that would break existing platform code (coordinate with platform workers)
- SSH to gvps is unavailable and E2E tests cannot run
- Existing Codex tests break — stop and report immediately
- The provider trait design needs architectural decisions beyond the feature scope
- ACP protocol version incompatibility discovered
