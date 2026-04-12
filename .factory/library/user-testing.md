# User Testing

## Validation Surface

### Rust Unit Tests
- **Tool**: `cargo test` in `shared/rust-bridge/codex-mobile-client/`
- **Coverage**: Provider trait, ACP protocol, Pi native, Droid native, hydration mappings, permission policies
- **Pattern**: Mock transports with in-memory message exchange

### E2E Provider Tests (Manual)
- **Tool**: SSH to gvps (100.82.102.84:5132)
- **Coverage**: Real Pi and Droid sessions over SSH
- **Pattern**: Workers SSH to gvps, spawn agent, verify session lifecycle

### iOS UI (Manual)
- **Tool**: iOS Simulator via `make ios-sim-fast`
- **Coverage**: Agent picker, session badges, connection flow, permissions
- **Pattern**: Manual verification in simulator

### Android UI (Manual)
- **Tool**: Android Emulator via `make android-emulator-fast`
- **Coverage**: Agent selector, session badges, connection flow, permissions
- **Pattern**: Manual verification in emulator

## Validation Concurrency

### Rust Tests
- **Max concurrent**: 4 (cargo test default parallelism on this machine)
- **Resource cost**: Low — CPU-bound, no network access for mock tests

### E2E SSH Tests
- **Max concurrent**: 1 (single SSH connection to gvps, sequential test execution)
- **Resource cost**: Medium — network latency, agent process spawning

### iOS/Android UI
- **Max concurrent**: 1 per platform (simulator/emulator resource-heavy)
- **Resource cost**: High — full platform build + simulator/emulator

## Flow Validator Guidance: Rust Tests (cargo test)

### Isolation Rules
- All Pi unit tests use mock transports (in-memory channels). No real SSH connections.
- Tests are independent — each test creates its own mock and provider instance.
- No shared mutable state between tests.
- Safe to run multiple subagents concurrently — they each run `cargo test` with different filters.

### Boundaries
- Each subagent runs `cargo test --lib -p codex-mobile-client -- <filter>` from `shared/rust-bridge/` directory.
- Subagents should NOT modify source files — only read code and run tests.
- If a test fails, capture the test name, expected vs actual, and the full error output.

### Test Naming Convention
- Pi native transport tests: `pi_native_*`, `pi_binary_*`, `pi_process_*`, `pi_ssh_*`, `pi_malformed_*`, `pi_full_lifecycle_*`, `pi_text_delta_*`, `pi_thinking_delta_*`, `pi_tool_execution_*`, `pi_abort_*`, `pi_error_*`
- Pi ACP transport tests: `pi_acp_*`
- Pi detection tests: `pi_detection_*`
- Pi session persistence tests: `session_entry_*`, `session_info_*`, `parse_*`
- Pi protocol tests: `pi_event_*`, `pi_thinking_level_*`, `serialize_command_*`
- Pi mock tests: `mock_channel_*`

## Flow Validator Guidance: E2E SSH Tests

### Isolation Rules
- Only ONE subagent may SSH to gvps at a time (single SSH connection constraint).
- The E2E subagent runs last after all unit test subagents complete.
- SSH target: `gvps` (100.82.102.84:5132, user ubuntu).
- Pi binary: `~/.bun/bin/pi` (v0.66.1, supports `--mode rpc`).
- `npx pi-acp` is available but does not print a version string.

### Boundaries
- E2E tests connect to a real Pi instance — tests are sequential and non-destructive.
- After E2E test, clean up any test sessions created on the remote host.
- SSH timeout should be 60s for each prompt-response round trip.
- Do NOT modify any files on gvps beyond creating temporary session files.
