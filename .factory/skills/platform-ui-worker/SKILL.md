---
name: platform-ui-worker
description: Implements iOS UI features for multi-agent connection routing — agent selection wiring, binding updates
---

# Platform UI Worker

NOTE: Startup and cleanup are handled by `worker-base`. This skill defines the WORK PROCEDURE.

## When to Use This Skill

Features that involve:
- iOS SwiftUI views for agent selection connection wiring
- UniFFI binding consumption in Swift
- Threading agent type parameters through iOS connect flow
- iOS build verification after Rust FFI changes

**This mission is iOS only — Android is deferred to a follow-up.**

## Required Skills

None. Work uses standard iOS tooling (xcodebuild, make ios-sim-fast).

## Work Procedure

### 1. Understand the Feature
Read the feature description. Identify:
- Which iOS views need modification (DiscoveryView, AppModel bridge)
- Which Rust UniFFI types are involved (AgentType, new FFI parameters)
- Whether bindings have been regenerated

### 2. Verify Bindings Are Current
The Rust backend worker may have already regenerated bindings. Verify:
```bash
# Check if bindings include the new agentType parameter
grep -n "agentType" shared/rust-bridge/generated/swift/*.swift || echo "Bindings not yet regenerated"
```
If not regenerated:
```bash
./shared/rust-bridge/generate-bindings.sh
```

### 3. Implement iOS Changes
- Modify views in `apps/ios/Sources/Litter/Views/` (primarily DiscoveryView.swift)
- Follow existing patterns: 4-space indent, UpperCamelCase types, lowerCamelCase properties
- Use `@Observable` / `@State` patterns matching existing code
- Dark theme: `Color.black` backgrounds, `#00FF9C` accent
- Keep bridge files thin — logic belongs in Rust

Key changes for this mission:
- `DiscoveryView.swift`: Read `AgentSelectionStore.shared.selectedAgentType(for: server.id)` in `connectToServer()`
- Add `agentType: AgentType?` parameter to `connectViaSSH()` and `sshConnectAndConnectServer()`
- Pass `agentType` to `appModel.ssh.sshStartRemoteServerConnect(... agentType:)` in both credential branches
- Update `maybeStartSimulatorAutoSSH` if it also calls the connect method

### 4. Verify

**iOS build:**
```bash
make ios-sim-fast
```

**Rust must still compile:**
```bash
cargo check --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml
```

### 5. Manual UI Verification
- Launch in simulator
- Navigate to discovery
- Tap a multi-agent server → agent picker should appear
- Select an agent → verify selection persists
- Connect → verify no crash (actual agent connection requires real server)
- Verify Codex-only server connects normally (regression)

Document verification steps and observations in the handoff.

## Example Handoff

```json
{
  "salientSummary": "Wired agent type selection through iOS connection flow. DiscoveryView now reads AgentSelectionStore and passes agentType to sshStartRemoteServerConnect. iOS builds via make ios-sim-fast. Codex regression verified.",
  "whatWasImplemented": "apps/ios/Sources/Litter/Views/DiscoveryView.swift — added agentType parameter threading in connectToServer(), connectViaSSH(), sshConnectAndConnectServer(). Both .password and .key credential branches updated.",
  "whatWasLeftUndone": "",
  "verification": {
    "commandsRun": [
      {"command": "make bindings", "exitCode": 0, "observation": "Bindings regenerated with agentType parameter"},
      {"command": "make ios-sim-fast", "exitCode": 0, "observation": "iOS build succeeded, installed to simulator"},
      {"command": "cargo check --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml", "exitCode": 0, "observation": "Rust compiles"}
    ],
    "interactiveChecks": [
      {"action": "Tapped multi-agent server in discovery", "observed": "Agent picker sheet appeared with Codex, Pi Native, Droid Native options"},
      {"action": "Selected Pi Native and connected", "observed": "Connection attempted with agentType parameter (confirmed via debug log)"},
      {"action": "Tapped Codex-only server", "observed": "Connected normally without agent picker, same as before"}
    ]
  },
  "tests": {
    "added": []
  },
  "discoveredIssues": []
}
```

## When to Return to Orchestrator

- Rust UniFFI types needed by the UI are not yet implemented (need a backend worker first)
- Build fails due to missing generated bindings
- The sshStartRemoteServerConnect signature in generated Swift doesn't include agentType (bindings not regenerated)
- Platform-specific build tool issues (Xcode not configured)
