---
name: platform-ui-worker
description: Implements iOS and Android UI features for multi-agent support — agent picker, session badges, per-agent settings
---

# Platform UI Worker

NOTE: Startup and cleanup are handled by `worker-base`. This skill defines the WORK PROCEDURE.

## When to Use This Skill

Features that involve:
- iOS SwiftUI views for agent selection, badges, settings
- Android Compose screens for agent selection, badges, settings
- UniFFI type consumption in Swift/Kotlin
- Platform-specific state management for multi-agent

## Required Skills

None. Work uses standard platform tooling (Xcode/xcodebuild, Gradle).

## Work Procedure

### 1. Understand the Feature
Read the feature description. Identify:
- Which platform views/screens need changes (iOS: Views/, Android: ui/)
- Which Rust UniFFI types are involved (AgentType, AgentInfo, etc.)
- Whether bindings need regeneration

### 2. Verify Bindings Are Current
If the feature depends on new Rust types, verify bindings are generated:
```bash
./shared/rust-bridge/generate-bindings.sh
```

### 3. Implement iOS (if in scope)
- Modify views in `apps/ios/Sources/Litter/Views/`
- Follow existing patterns: 4-space indent, UpperCamelCase types, lowerCamelCase properties
- Use `@Observable` / `@State` patterns matching existing code
- Dark theme: `Color.black` backgrounds, `#00FF9C` accent
- Keep bridge files thin — logic belongs in Rust

### 4. Implement Android (if in scope)
- Modify screens in `apps/android/app/src/main/java/com/litter/android/ui/`
- Follow existing patterns: Compose Material3, 4-space indent
- Use state from `AppModel.kt` / `AppLaunchState.kt`
- Keep Kotlin thin — business logic in Rust

### 5. Verify

**iOS:**
```bash
make ios-sim-fast
```

**Android:**
```bash
make android-emulator-fast
```

**Rust (bindings must still compile):**
```bash
cargo check --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml
```

### 6. Manual UI Verification
Since mobile UI cannot be fully automated:
- Launch in simulator/emulator
- Navigate to affected screens
- Verify agent picker appears, shows correct agents
- Verify badges display correctly
- Verify connection flow works end-to-end

Document verification steps and observations in the handoff.

## Example Handoff

```json
{
  "salientSummary": "Added agent picker to iOS HeaderView (InlineAgentSelectorView alongside model picker) and Android HeaderBar (AgentSelectorDropdown). Agent type badges added to SessionsScreen on both platforms. iOS builds via make ios-sim-fast, Android via make android-emulator-fast.",
  "whatWasImplemented": "iOS: Views/InlineAgentSelectorView.swift (new), Views/HeaderView.swift (agent picker section), Views/SessionsScreen.swift (agent badges). Android: ui/conversation/AgentSelectorDropdown.kt (new), ui/conversation/HeaderBar.kt (agent selector), ui/sessions/SessionsScreen.kt (agent badges).",
  "whatWasLeftUndone": "Per-agent settings sheet not yet implemented — deferred to follow-up feature.",
  "verification": {
    "commandsRun": [
      {"command": "make ios-sim-fast", "exitCode": 0, "observation": "iOS build succeeded, installed to simulator"},
      {"command": "make android-emulator-fast", "exitCode": 0, "observation": "Android build succeeded"},
      {"command": "cargo check --manifest-path shared/rust-bridge/codex-mobile-client/Cargo.toml", "exitCode": 0, "observation": "Rust compiles with new UniFFI types"}
    ],
    "interactiveChecks": [
      {"action": "Opened agent picker in HeaderView", "observed": "Shows Codex, Pi Native, Droid Native options with correct icons"},
      {"action": "Selected Pi agent", "observed": "Agent badge appears on new session in SessionsScreen"},
      {"action": "Opened Android HeaderBar agent selector", "observed": "Same agents listed, selection persists"}
    ]
  },
  "tests": {
    "added": []
  },
  "discoveredIssues": []
}
```

## When to Return to Orchestrator

- Rust UniFFI types needed by the UI are not yet implemented (need a backend worker to create them first)
- Build fails due to missing generated bindings
- Design ambiguity in how agent picker should work (e.g., where it appears, what happens with single-agent servers)
- Platform-specific build tool issues (Xcode not configured, Android SDK missing)
