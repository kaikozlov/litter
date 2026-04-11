# Environment

Environment variables, external dependencies, and setup notes.

**What belongs here:** Required env vars, external API keys/services, dependency quirks, platform-specific notes.
**What does NOT belong here:** Service ports/commands (use `.factory/services.yaml`).

---

## SSH Test Target
- **Host**: gvps (100.82.102.84:5132, user: ubuntu)
- **Pi**: v0.66.1 at `~/.bun/bin/pi` (supports `--mode rpc`)
- **Droid**: v0.99.0 at `~/.bun/bin/droid` (supports `exec --stream-jsonrpc`)
- **Node**: v25.3.0, npm 11.6.2

## Required Environment Variables
- `FACTORY_API_KEY` — Required for Droid authentication (set on remote host or passed via SSH)

## ACP Protocol
- Uses `agent-client-protocol-schema` crate v0.11.5 (Apache-2.0)
- Transport: NDJSON over bidirectional stream (SSH PTY for remote agents)
- Protocol version: V1

## Build Dependencies
- Rust toolchain via rustup (not Homebrew)
- `cargo` must resolve to rustup proxy
- No new system dependencies needed (ACP uses existing russh + serde_json)
