import Foundation
import SwiftUI

extension AppServerSnapshot {
    var isConnected: Bool {
        health == .connected
    }

    var isIpcConnected: Bool {
        hasIpc && !isLocal && isConnected
    }

    var connectionModeLabel: String {
        guard !isLocal else { return "local" }
        return isIpcConnected ? "remote · ipc" : "remote"
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
        if health == .connected, !isLocal, account == nil {
            return "Sign in required"
        }
        return health.displayLabel
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
        if health == .connected, !isLocal, account == nil {
            return .orange
        }
        return health.accentColor
    }
}
