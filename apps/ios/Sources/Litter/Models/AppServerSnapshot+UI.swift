import Foundation
import SwiftUI

extension AppServerSnapshot {
    var isConnected: Bool {
        transportState == .connected
    }

    var isIpcConnected: Bool {
        ipcState == .ready
    }

    var canUseTransportActions: Bool {
        capabilities.canUseTransportActions
    }

    var canBrowseDirectories: Bool {
        capabilities.canBrowseDirectories
    }

    var canResumeViaIpc: Bool {
        capabilities.canResumeViaIpc
    }

    // MARK: - Agent Capabilities (VAL-ACP-100)

    /// Whether the agent supports streaming output.
    var supportsStreaming: Bool {
        agentCapabilities.contains("streaming")
    }

    /// Whether the agent supports tool calls.
    var supportsTools: Bool {
        agentCapabilities.contains("tools")
    }

    /// Whether the agent supports plan generation and display.
    var supportsPlans: Bool {
        agentCapabilities.contains("plans")
    }

    /// Whether the agent supports reasoning/thinking display.
    var supportsReasoning: Bool {
        agentCapabilities.contains("reasoning") || agentCapabilities.contains("thinking-levels")
    }

    /// Whether the agent supports multimodal input (images, files).
    var supportsMultimodal: Bool {
        agentCapabilities.contains("multimodal")
    }

    /// Whether the agent supports approval flows.
    var supportsApprovals: Bool {
        agentCapabilities.contains("approvals")
    }

    /// Whether the agent supports autonomy levels.
    var supportsAutonomyLevels: Bool {
        agentCapabilities.contains("autonomy-levels")
    }

    /// Whether the agent has any capabilities reported (i.e., connected via ACP).
    var hasAgentCapabilities: Bool {
        !agentCapabilities.isEmpty
    }

    /// Human-readable summary of capabilities for display.
    var capabilitiesSummary: String {
        guard !agentCapabilities.isEmpty else { return "" }
        return agentCapabilities.map { cap in
            switch cap {
            case "streaming": return "Streaming"
            case "tools": return "Tools"
            case "plans": return "Plans"
            case "reasoning", "thinking-levels": return "Reasoning"
            case "multimodal": return "Multimodal"
            case "approvals": return "Approvals"
            case "autonomy-levels": return "Autonomy"
            default: return cap.prefix(1).uppercased() + cap.dropFirst()
            }
        }.joined(separator: " · ")
    }

    var connectionModeLabel: String {
        guard !isLocal else { return "local" }
        guard ExperimentalFeatures.shared.isEnabled(.ipc) else { return "remote" }
        switch ipcState {
        case .ready:
            return "remote · ipc"
        case .disconnected:
            return "remote · no ipc"
        case .unsupported:
            return "remote"
        }
    }

    var currentConnectionStep: AppConnectionStepSnapshot? {
        guard let progress = connectionProgress else { return nil }
        return progress.steps.first(where: {
            $0.state == .awaitingUserInput || $0.state == .inProgress
        }) ?? progress.steps.last(where: {
            $0.state == .failed || $0.state == .completed
        })
    }

    var connectionProgressLabel: String? {
        guard let step = currentConnectionStep else { return nil }
        switch step.kind {
        case .connectingToSsh:
            return "connecting"
        case .detectingAgents:
            return "detecting agents"
        case .findingAgent:
            return "finding agent"
        case .installingAgent:
            return "installing"
        case .startingAgent:
            return "starting"
        case .openingTunnel:
            return "tunneling"
        case .connected:
            return "connected"
        }
    }

    var connectionProgressDetail: String? {
        currentConnectionStep?.detail ?? connectionProgress?.terminalMessage
    }

    var statusLabel: String {
        if let connectionProgressLabel {
            return connectionProgressLabel
        }
        if transportState == .connected, !isLocal, account == nil {
            return "Sign in required"
        }
        if transportState == .connected, ipcState == .disconnected,
           ExperimentalFeatures.shared.isEnabled(.ipc) {
            return "Connected, IPC unavailable"
        }
        return transportState.displayLabel
    }

    var statusColor: Color {
        if currentConnectionStep?.state == .failed {
            return .red
        }
        if currentConnectionStep?.state == .awaitingUserInput {
            return .orange
        }
        if connectionProgressLabel != nil {
            return LitterTheme.accent
        }
        if transportState == .connected, !isLocal, account == nil {
            return .orange
        }
        if transportState == .connected, ipcState == .disconnected,
           ExperimentalFeatures.shared.isEnabled(.ipc) {
            return .orange
        }
        return transportState.accentColor
    }
}
