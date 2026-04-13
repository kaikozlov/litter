---
name: rust-backend-worker
description: Implements Rust backend features in shared/rust-bridge/codex-mobile-client/ — SSH streaming, provider factory, agent routing, method mapping, FFI exposure
---

# Rust Backend Worker

NOTE: Startup and cleanup are handled by `worker-base`. This skill defines the WORK PROCEDURE.

## When to Use This Skill

Features that involve:
- SshClient modifications (exec_stream, new methods)
- Provider factory and transport construction
- MobileClient agent-type routing
- FFI SSH connect method changes
- Codex wire method mapping on Pi/Droid transports
- Any Rust code in `shared/rust-bridge/codex-mobile-client/src/`

## Required Skills

None. All work is done with standard Rust tooling (cargo test, cargo check, cargo clippy).

## Work Procedure

### 1. Understand the Feature
Read the feature description carefully. Identify:
- Which module(s) in `src/` are affected
- What existing types/methods need extension vs new creation
- What the protocol mapping looks like (Codex wire methods → provider-native methods)
- What backward compatibility constraints exist (existing Codex flow MUST NOT change)

### 2. Write Tests First (Red)
Before ANY implementation:
- Create test cases or extend existing `#[cfg(test)] mod tests` blocks
- Write failing tests that define expected behavior
- Cover: happy path, error cases, edge cases (disconnect, timeout, malformed data)
- For provider transports: use mock transports that exchange in-memory messages
- For SSH: mock the russh channel behavior
- For method mapping: test each Codex wire method → native method translation
- For E2E against gvps: mark tests with `#[ignore]` and note SSH requirement

### 3. Implement (Green)
- Follow the existing code patterns in each module
- For SshClient: follow the `open_streamlocal()` pattern for `exec_stream()`
- For provider factory: use `Arc<SshClient>` and `exec_stream()` to spawn remote binaries
- For method mapping: add match arms BEFORE the existing `_` fallback in `send_request()`
- For FFI: add `Option<AgentType>` parameter for backward compatibility
- All new UniFFI types must use proper derive macros
- Reuse existing `TransportError`/`RpcError` for error handling

### 4. Verify
Run these commands and ensure all pass:
```bash
cargo check --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml
cargo test --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml --lib
cargo clippy --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml -- -D warnings
```

### 5. If UniFFI Types Changed
If you added or modified any `#[derive(uniffi::Record)]` or `#[derive(uniffi::Enum)]` types, or changed FFI method signatures:
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
  "salientSummary": "Implemented SshClient::exec_stream() following open_streamlocal pattern. Returns Pin<Box<dyn AsyncRead + AsyncWrite + Send>>. 6 unit tests pass covering type bounds, round-trip I/O, stderr capture, cleanup on drop, and exec() regression.",
  "whatWasImplemented": "src/ssh.rs — added exec_stream() method (30 lines) and ExecStream wrapper type. 6 new tests in ssh tests module.",
  "whatWasLeftUndone": "",
  "verification": {
    "commandsRun": [
      {"command": "cargo test --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml --lib", "exitCode": 0, "observation": "892 tests passed (6 new + 886 existing)"},
      {"command": "cargo clippy --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml -- -D warnings", "exitCode": 0, "observation": "No warnings"}
    ],
    "interactiveChecks": []
  },
  "tests": {
    "added": [
      {"file": "src/ssh.rs", "cases": [
        {"name": "test_exec_stream_type_bounds", "verifies": "AsyncRead + AsyncWrite + Send bounds on return type"},
        {"name": "test_exec_stream_round_trip", "verifies": "Write to stdin appears on stdout"},
        {"name": "test_exec_stream_stderr", "verifies": "Stderr bytes are captured"},
        {"name": "test_exec_stream_cleanup_on_drop", "verifies": "Channel closed when stream dropped"},
        {"name": "test_exec_regression", "verifies": "Existing exec() still works"}
      ]}
    ]
  },
  "discoveredIssues": []
}
```

## When to Return to Orchestrator

- Feature requires new UniFFI types that would break existing platform code (coordinate with platform workers)
- SSH to gvps is unavailable and E2E tests cannot run
- Existing Codex tests break — stop and report immediately
- The SshClient changes require architectural decisions beyond the feature scope
- russh ChannelStream has limitations that affect the provider factory design
