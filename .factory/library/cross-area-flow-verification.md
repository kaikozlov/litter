# Cross-Area Flow Verification

## What Was Done
End-to-end verification of cross-area flows spanning all four milestones (provider abstraction → ACP → discovery → iOS parity).

## Test Coverage

### Rust Tests (cross_provider.rs)
- VAL-CROSS-001: Full Codex session lifecycle through provider trait
- VAL-CROSS-002: Full Codex approval flow through provider trait
- VAL-CROSS-006: Multi-provider concurrent sessions with isolated events
- VAL-CROSS-007: Reconnection health transitions after disconnect
- VAL-CROSS-008: Reconnection after transport disconnect
- VAL-CROSS-009: Provider switch mid-session (Pi → Droid clean handoff)
- VAL-CROSS-010: Session history aggregation across providers
- VAL-CROSS-011: Agent binary validation (pi, droid binaries; no auto-install)
- VAL-CROSS-012: Discovery port registry includes Pi (9234) and Codex (8390) ports

### iOS Tests (CrossAreaFlowTests.swift - 23 tests)
- VAL-CROSS-002: Pi agent type full flow attributes and selection persistence
- VAL-CROSS-003: Droid agent type full ACP flow and transport preference
- VAL-CROSS-004: GenericAcp flow from profile config to connection parameters
- VAL-CROSS-005: ACP profile, permission policy, and transport preference changes are immediately effective
- VAL-CROSS-006: Multi-provider session filtering by agent type with correct badge types
- VAL-CROSS-008: Saved server migration (legacy hasCodexServer/codexPorts/directCodex → new fields)

## Build Verification
- `make rust-test`: 1291 passed, 0 failed
- `make ios-sim-fast`: BUILD SUCCEEDED
- iOS unit tests: 240 passed, 0 failed
- Xcode project regenerated via `make xcgen` after adding new test file
