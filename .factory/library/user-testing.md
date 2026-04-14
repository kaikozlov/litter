# User Testing

## Validation Surface

### Rust Unit Tests
- **Tool:** `make rust-test`
- **Coverage:** All Rust assertions (VAL-ABS-*, VAL-ACP-*, VAL-DISC-*)
- **Setup:** None — runs on host machine
- **Resource cost:** Low — ~200MB RAM, single process, 30-90 seconds
- **Max concurrent:** 5

### iOS Build Verification
- **Tool:** `make ios-sim-fast`
- **Coverage:** iOS compilation after binding/FFI changes
- **Setup:** Xcode with iOS simulator SDK, Rust iOS targets
- **Resource cost:** Medium-High — Rust cross-compile + Xcode build, ~3GB RAM, 3-8 minutes
- **Max concurrent:** 1

### iOS Unit Tests
- **Tool:** `make test-ios`
- **Coverage:** Codable migration, model renames, iOS unit tests
- **Setup:** Requires successful `make ios-sim-fast` build
- **Resource cost:** Medium — ~1GB RAM, 1-3 minutes
- **Max concurrent:** 1

### E2E SSH Tests
- **Tool:** `cargo test --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml -- --ignored`
- **Coverage:** Real Pi/Droid/ACP agent connections via gvps
- **Setup:** SSH access to gvps (100.82.102.84:5132) required
- **Resource cost:** Low — SSH connections, ~50MB RAM per connection
- **Max concurrent:** 2 (limited by gvps capacity)
- **Note:** Skip if gvps unavailable — mark assertions as "blocked"

### Manual iOS Verification
- **Tool:** iOS Simulator + tuistory
- **Coverage:** Agent picker, discovery flow, session list, conversation rendering, settings
- **Setup:** `make ios-sim-fast` + simulator launch
- **Resource cost:** Medium — simulator uses ~1GB RAM
- **Max concurrent:** 1 (single simulator)

## Validation Concurrency

- **Rust tests:** Max concurrent: 5 (low resource usage)
- **iOS build:** Max concurrent: 1 (Xcode/rustc resource-heavy)
- **iOS tests:** Max concurrent: 1 (requires simulator)
- **E2E SSH:** Max concurrent: 2 (limited by gvps)
- **Manual iOS:** Max concurrent: 1 (single simulator)
- **Overall:** Expect sequential validation for iOS surface, parallel for Rust-only

## Notes for Validators

- Rust assertions can be verified via `make rust-test` — no UI interaction needed
- iOS assertions require successful `make ios-sim-fast` build first
- E2E assertions requiring remote servers (gvps) may be marked "blocked" if unreachable
- Cross-area assertions involving real agent connections require manual testing or E2E SSH tests
- Android source files are text-updated only (not compiled) — verify via `rg` not build
- For field rename assertions, use `rg` to verify old names are gone and new names present
- Codable migration tests MUST verify backward compatibility with old key names

## Flow Validator Guidance: Rust Unit Tests

- **Surface:** `make rust-test` — cargo test for codex-mobile-client
- **Isolation:** Tests run in-process, no shared external state, no ports needed
- **Boundaries:** Do NOT start any services. Do NOT access the network. Only run `cargo test`.
- **Concurrency:** Safe to run up to 5 validators concurrently (low resource, isolated test processes)
- **Shared state concerns:** None — each test process gets its own tokio runtime and mock transports
- **Verification pattern:** Run the test command, grep for specific test names, check exit code is 0

## Flow Validator Guidance: iOS Build and Tests

- **Surface:** `make ios-sim-fast` (build), `make test-ios` (unit tests)
- **Isolation:** Single Xcode process, single simulator — MUST run alone
- **Boundaries:** Do NOT run any other iOS validator concurrently. Do NOT start simulator manually.
- **Concurrency:** Max concurrent: 1. No other iOS or resource-heavy processes should run alongside.
- **Shared state concerns:** Xcode build locks, DerivedData, simulator state — single validator only
- **Verification pattern:** Run `make ios-sim-fast`, check exit code 0. Then `make test-ios`, check exit code 0.

## Flow Validator Guidance: iOS UI (tuistory)

- **Surface:** iOS Simulator via tuistory skill
- **Isolation:** Single simulator instance, single session
- **Boundaries:** Do NOT run alongside other iOS validators or simulators
- **Concurrency:** Max concurrent: 1
- **Shared state concerns:** Simulator app data (UserDefaults, keychain), app state
- **Prerequisites:** Successful `make ios-sim-fast` build must complete before UI testing
- **Verification pattern:** Install app on simulator, use tuistory for interaction, take screenshots for evidence
- **Note:** For generic-acp milestone, iOS UI assertions may be partially testable via code review (rg) since real ACP agents require remote servers
