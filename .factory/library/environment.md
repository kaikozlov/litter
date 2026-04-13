# Environment

Environment variables, external dependencies, and setup notes.

**What belongs here:** Required env vars, external API keys/services, dependency quirks, platform-specific notes.
**What does NOT belong here:** Service ports/commands (use `.factory/services.yaml`).

---

## Rust Toolchain
- `cargo` and `rustc` must come from rustup, not Homebrew. `.factory/init.sh` handles PATH setup.
- Cross-compilation targets: `aarch64-apple-ios`, `aarch64-apple-ios-sim`, Android NDK targets

## SSH Test Target
- **gvps**: 100.82.102.84:5132, user `ubuntu`
- Pi 0.66.1 at `~/.bun/bin/pi`
- Droid 0.99.0 at `~/.bun/bin/droid`
- `ssh gvps` should work directly (configured in ~/.ssh/config)
- E2E tests marked `#[ignore]` require this target

## UniFFI Binding Regeneration
- After any Rust UniFFI type changes: `./shared/rust-bridge/generate-bindings.sh`
- Swift bindings go to `shared/rust-bridge/generated/swift/`
- Kotlin bindings go to `shared/rust-bridge/generated/kotlin/`
- iOS build picks up generated Swift directly
- Android build picks up generated Kotlin from the shared location

## Key Dependencies
- `russh` — SSH client library. Supports ChannelStream for bidirectional I/O.
- `agent-client-protocol-schema` v0.11.5 — ACP protocol types
- `uniffi` — Rust↔Swift/Kotlin binding generation
- `tokio` — async runtime
