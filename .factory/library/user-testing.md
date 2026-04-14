# User Testing

## Validation Surface

### Rust Unit Tests
- **Tool:** `cargo test --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml --lib`
- **Coverage:** All Rust assertions (63 total)
- **Setup:** None — runs on host machine
- **Resource cost:** Low — ~200MB RAM, single process, <30 seconds

### iOS Build Verification
- **Tool:** `make ios-sim-fast`
- **Coverage:** iOS compilation after binding changes (VAL-IOS-004)
- **Setup:** Requires Xcode with iOS simulator SDK, Rust iOS targets
- **Resource cost:** Medium — Rust cross-compile + Xcode build, ~2GB RAM, 2-5 minutes

### E2E SSH Tests
- **Tool:** `cargo test --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml -- --ignored`
- **Coverage:** Real Pi/Droid agent connections via gvps (VAL-CROSS-001 through VAL-CROSS-007)
- **Setup:** SSH access to gvps required
- **Resource cost:** Low — SSH connections, ~50MB RAM per connection
- **Note:** Skip if gvps unavailable — manual testing sufficient

### Manual iOS Verification
- **Tool:** iOS Simulator + manual interaction
- **Coverage:** Agent picker selection → connection flow, Codex regression
- **Setup:** `make ios-sim-fast` + simulator launch
- **Resource cost:** Medium — simulator uses ~1GB RAM

## Validation Concurrency

- **Rust tests:** Max concurrent: 5 (low resource usage)
- **iOS build:** Max concurrent: 1 (Xcode is resource-heavy)
- **E2E SSH:** Max concurrent: 3 (limited by SSH connection capacity to gvps)
- **Manual iOS:** Max concurrent: 1 (single simulator interaction)

## Notes for Validators

- All Rust assertions can be verified via `cargo test` — no UI interaction needed
- iOS assertions require a successful `make ios-sim-fast` build
- Cross-area assertions (VAL-CROSS-*) require either E2E SSH tests or manual verification
- If gvps is unreachable, mark E2E assertions as "blocked" and note in synthesis
- VAL-CROSS-008 (IPC isolation) is a Rust-only assertion — verify via unit test

## Flow Validator Guidance: Rust Method Mapping Tests

### Isolation Rules
- Each test creates its own mock transport (MockPiChannel, MockAcpTransport, etc.)
- No shared state between tests — safe to run concurrently
- No services to start — all tests are pure unit tests against mock transports

### Boundaries
- Pi native tests: filter `pi_codex` in cargo test
- Pi ACP tests: filter `pi_acp_codex` in cargo test
- Droid native tests: filter `droid_codex` in cargo test
- Droid ACP tests: filter `droid_acp_codex` in cargo test
- Do NOT run the full `--lib` suite per subagent — use filtered runs to avoid overlap

### Resource Cost
- Very low: ~200MB RAM total for all 4 groups
- Max concurrent validators: 5 (for method mapping tests specifically)
- Typical runtime: <1 second per filtered group

## Flow Validator Guidance: iOS Wiring Code Review

### Isolation Rules
- Code review is read-only — no shared mutable state
- File reads are safe to parallelize
- Do NOT modify any source files

### Boundaries
- VAL-IOS-001: Review `connectToServer()` in DiscoveryView.swift — verify `AgentSelectionStore.shared.selectedAgentType(for: server.id)` is read
- VAL-IOS-002: Review `connectViaSSH()` — verify agent type parameter threading
- VAL-IOS-003: Review `sshConnectAndConnectServer()` — verify agent type reaches `sshStartRemoteServerConnect` call
- Do NOT overlap with Rust test groups or iOS build verification

### Resource Cost
- Very low: ~50MB RAM, read-only file access
- Max concurrent validators: 5

## Flow Validator Guidance: iOS Build Verification

### Isolation Rules
- Only ONE build validator at a time (Xcode/rustc resource-heavy)
- Do NOT run any other cargo or Xcode commands concurrently

### Boundaries
- VAL-IOS-004: Run `make bindings && make ios-sim-fast` — must exit 0
- VAL-IOS-005: Run `make rust-test` — verify all existing tests pass (Codex regression)
- Check generated Swift binding for `agentType: AgentType?` parameter

### Resource Cost
- High: ~2-4GB RAM for Rust cross-compile + Xcode build
- Max concurrent validators: 1
- Typical runtime: 3-8 minutes

## Flow Validator Guidance: Cross-Area Rust Tests

### Isolation Rules
- Each test uses mock transports — no shared state
- Safe to run multiple filtered test groups concurrently
- Do NOT run full `--lib` suite — use filtered runs

### Boundaries
- VAL-CROSS-004: Filter `codex_regression` or `connect_remote_over_ssh` in cargo test
- VAL-CROSS-005: Manual-only — cannot test multiple connections without real server
- VAL-CROSS-006: Filter `connection_failed` or `factory` in cargo test for error handling
- VAL-CROSS-008: Filter `ipc` or `provider` in cargo test for IPC isolation

### Resource Cost
- Low: ~200MB RAM, <10 seconds per filtered group
- Max concurrent validators: 3

## Flow Validator Guidance: E2E SSH Cross-Area

### Isolation Rules
- E2E tests create real SSH connections to gvps — serialize or limit concurrency
- Each test spawns a remote process on gvps
- Clean up: verify no zombie processes after tests

### Boundaries
- VAL-CROSS-001: Pi native connection — `cargo test ... -- --ignored` with `pi_native` filter
- VAL-CROSS-002: Droid native connection — `cargo test ... -- --ignored` with `droid_native` filter
- VAL-CROSS-003: Pi ACP connection — `cargo test ... -- --ignored` with `pi_acp` filter
- VAL-CROSS-007: Provider disconnect — verify cleanup after connection tests
- If gvps is unreachable, mark assertions as blocked

### Resource Cost
- Medium: SSH connections, ~50MB RAM per connection
- Max concurrent validators: 2 (limited by gvps capacity)
- Typical runtime: 10-30 seconds per test
