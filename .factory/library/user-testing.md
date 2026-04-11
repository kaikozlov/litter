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
