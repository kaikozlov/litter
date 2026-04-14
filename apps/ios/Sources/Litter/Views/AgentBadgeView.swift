import SwiftUI

// MARK: - Agent Type Visual Configuration

extension AgentType {
    /// SF Symbol name for this agent type.
    var icon: String {
        switch self {
        case .codex:
            return "terminal.fill"
        case .piAcp, .piNative:
            return "brain"
        case .droidAcp, .droidNative:
            return "robot"
        case .genericAcp:
            return "gearshape.fill"
        }
    }

    /// Accent tint color for this agent type.
    var tintColor: Color {
        switch self {
        case .codex:
            return LitterTheme.accent
        case .piAcp, .piNative:
            return Color.purple
        case .droidAcp, .droidNative:
            return Color.orange
        case .genericAcp:
            return Color.gray
        }
    }

    /// Short display label for this agent type.
    var displayName: String {
        switch self {
        case .codex:
            return "Codex"
        case .piAcp, .piNative:
            return "Pi"
        case .droidAcp, .droidNative:
            return "Droid"
        case .genericAcp:
            return "ACP"
        }
    }

    /// Whether this agent type is a Pi variant.
    var isPi: Bool {
        switch self {
        case .piAcp, .piNative:
            return true
        default:
            return false
        }
    }

    /// Whether this agent type is a Droid variant.
    var isDroid: Bool {
        switch self {
        case .droidAcp, .droidNative:
            return true
        default:
            return false
        }
    }

    /// Transport description for display.
    var transportLabel: String {
        switch self {
        case .codex:
            return "WebSocket"
        case .piAcp:
            return "ACP"
        case .piNative:
            return "Native"
        case .droidAcp:
            return "ACP"
        case .droidNative:
            return "Native"
        case .genericAcp:
            return "ACP"
        }
    }

    /// Whether this agent type uses the ACP transport protocol.
    var usesACP: Bool {
        switch self {
        case .piAcp, .droidAcp, .genericAcp:
            return true
        default:
            return false
        }
    }
}

// MARK: - Agent Badge View

/// A small badge showing an agent type icon with colored tint.
/// Used on session rows, discovery cards, and in the agent picker.
struct AgentBadgeView: View {
    let agentType: AgentType
    var size: CGFloat = 14
    var showsLabel: Bool = false

    var body: some View {
        HStack(spacing: 3) {
            Image(systemName: agentType.icon)
                .font(.system(size: size, weight: .semibold))
                .foregroundColor(agentType.tintColor)
            if showsLabel {
                Text(agentType.displayName)
                    .font(.system(size: size - 2, weight: .medium))
                    .foregroundColor(agentType.tintColor)
            }
        }
    }
}

/// A compact pill badge for agent type, suitable for inline use.
struct AgentTypePill: View {
    let agentType: AgentType

    var body: some View {
        HStack(spacing: 3) {
            Image(systemName: agentType.icon)
                .font(.system(size: 9, weight: .semibold))
            Text(agentType.displayName)
                .font(.system(size: 10, weight: .medium))
        }
        .foregroundColor(agentType.tintColor)
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(agentType.tintColor.opacity(0.15))
        .clipShape(Capsule())
    }
}

// MARK: - Agent Permission Policy Display

extension AgentPermissionPolicy {
    var displayName: String {
        switch self {
        case .autoApproveAll:
            return "Auto Approve All"
        case .autoRejectHighRisk:
            return "Auto Reject High Risk"
        case .promptAlways:
            return "Prompt Always"
        }
    }

    var shortName: String {
        switch self {
        case .autoApproveAll:
            return "Auto"
        case .autoRejectHighRisk:
            return "Safe"
        case .promptAlways:
            return "Prompt"
        }
    }

    var description: String {
        switch self {
        case .autoApproveAll:
            return "All permission requests are automatically approved."
        case .autoRejectHighRisk:
            return "High-risk operations are automatically rejected; low-risk ones approved."
        case .promptAlways:
            return "Every permission request requires your explicit approval."
        }
    }

    var icon: String {
        switch self {
        case .autoApproveAll:
            return "checkmark.circle.fill"
        case .autoRejectHighRisk:
            return "shield.fill"
        case .promptAlways:
            return "questionmark.shield.fill"
        }
    }
}

// MARK: - Preview

#if DEBUG
#Preview("Agent Badges") {
    VStack(spacing: 16) {
        ForEach([
            AgentType.codex,
            .piNative,
            .piAcp,
            .droidNative,
            .droidAcp,
            .genericAcp
        ], id: \.displayName) { agentType in
            HStack(spacing: 12) {
                AgentBadgeView(agentType: agentType, size: 16)
                AgentTypePill(agentType: agentType)
                Spacer()
            }
        }
    }
    .padding()
    .background(Color.black)
}
#endif
