# iOS Connection & Conversation Parity

## Overview
The iOS connection flow and conversation rendering work identically for all provider types (Codex, Pi, Droid, GenericAcp). The infrastructure is fully provider-agnostic.

## Key Files

### Connection Flow
- `DiscoveryView.swift` — Main discovery/connection UI with agent picker, SSH flow, and runtime agent detection
- `DiscoveredServer.swift` — Server model with `agentTypes`, `agentInfos`, `agentPorts`, and `PreferredConnectionMode`
- `AgentPickerView.swift` — `AgentSelectionStore` (selection persistence), `AgentPickerSheet` (picker UI), `AgentType+Codable`
- `ConnectionTarget.swift` — Connection target enum (local, remote, remoteURL, sshThenRemote)
- `AppServerSnapshot+UI.swift` — Connection step labels, status labels, agent capabilities

### Conversation Rendering
- `ConversationView.swift` — Main conversation view, `TypingIndicator` ("Thinking"), `SubagentBreadcrumbBar` (uses `agentDisplayLabel`)
- `ConversationTimelineView.swift` — Timeline rendering with subagent cards
- `MessageBubbleView.swift` — Message rendering (no agent-type branching)
- `HeaderView.swift` — `InlineModelSelectorView` with per-agent controls (Pi thinking levels, Droid autonomy)

### Voice
- `InlineVoiceStatusStrip.swift` — Provider-generic voice status strip (accepts `agentLabel` parameter)
- `VoiceCallView.swift` — Voice transcript entries use `AgentLabelFormatter.format()` with "Agent" fallback

## Provider-Agnostic Patterns

1. **Connection steps**: `AppConnectionStepKind` uses generic names (`.findingAgent`, `.startingAgent`, `.detectingAgents`)
2. **Error messages**: Never reference specific providers
3. **Conversation rendering**: No branching on agent type — all providers produce the same UI
4. **Agent selection**: `AgentSelectionStore` persists selection per server ID, auto-selects single agent, shows picker for multiple
5. **Runtime detection**: SSH connect can detect agents at runtime and pause for picker selection
6. **Connection choice**: Dialog uses "Use Agent (port)" and "Connect via SSH" labels

## Tests
- `ConnectionConversationParityTests.swift` — 28 tests covering VAL-IOS-028 through VAL-IOS-038
- `AgentBadgePickerTests.swift` — Badge/picker/persistence tests
- `SessionFilterAndSettingsTests.swift` — Session filter and per-agent settings tests
