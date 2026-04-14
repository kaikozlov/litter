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

// MARK: - Agent Capability Badge

/// A compact capability indicator badge for display in agent picker rows.
/// Shows a small icon and label for each capability the agent supports.
struct AgentCapabilityBadge: View {
    let capability: String
    var tint: Color = LitterTheme.accent

    var body: some View {
        HStack(spacing: 2) {
            Image(systemName: iconForCapability)
                .font(.system(size: 8, weight: .semibold))
            Text(labelForCapability)
                .font(.system(size: 9, weight: .medium))
        }
        .foregroundColor(tint)
        .padding(.horizontal, 4)
        .padding(.vertical, 1.5)
        .background(tint.opacity(0.12))
        .clipShape(Capsule())
    }

    private var iconForCapability: String {
        switch capability {
        case "streaming": return "waveform"
        case "tools": return "wrench.fill"
        case "plans": return "list.bullet.clipboard.fill"
        case "reasoning", "thinking-levels": return "brain.head.profile.fill"
        case "multimodal": return "photo.fill"
        case "approvals": return "hand.raised.fill"
        case "autonomy-levels": return "gauge.with.dots.needle.33percent"
        default: return "circle.fill"
        }
    }

    private var labelForCapability: String {
        switch capability {
        case "streaming": return "Stream"
        case "tools": return "Tools"
        case "plans": return "Plans"
        case "reasoning", "thinking-levels": return "Think"
        case "multimodal": return "Multi"
        case "approvals": return "Approve"
        case "autonomy-levels": return "Auto"
        default: return capability.prefix(1).uppercased() + capability.dropFirst(1)
        }
    }
}

/// A row of capability badges for an agent.
struct AgentCapabilitiesRow: View {
    let capabilities: [String]
    var tint: Color = LitterTheme.accent
    var maxBadges: Int = 4

    var body: some View {
        if !capabilities.isEmpty {
            HStack(spacing: 4) {
                ForEach(Array(capabilities.prefix(maxBadges).enumerated()), id: \.offset) { _, cap in
                    AgentCapabilityBadge(capability: cap, tint: tint)
                }
                if capabilities.count > maxBadges {
                    Text("+\(capabilities.count - maxBadges)")
                        .font(.system(size: 9, weight: .medium))
                        .foregroundColor(LitterTheme.textMuted)
                }
            }
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
