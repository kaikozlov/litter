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
