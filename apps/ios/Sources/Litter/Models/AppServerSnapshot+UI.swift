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
        case .findingCodex:
            return "finding codex"
        case .installingCodex:
            return "installing"
        case .startingAppServer:
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
