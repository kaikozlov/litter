import SwiftUI

// MARK: - AgentType + Serialization

extension AgentType: @retroactive Codable {
    private static var rawValueMap: [(AgentType, String)] {
        [
            (.codex, "codex"),
            (.piAcp, "piAcp"),
            (.piNative, "piNative"),
            (.droidAcp, "droidAcp"),
            (.droidNative, "droidNative"),
            (.genericAcp, "genericAcp"),
        ]
    }

    /// Stable string identifier for persistence.
    var persistentKey: String {
        Self.rawValueMap.first(where: { $0.0 == self })?.1 ?? "codex"
    }

    /// Initialize from a persisted key string.
    static func fromPersistentKey(_ key: String) -> AgentType {
        rawValueMap.first(where: { $0.1 == key })?.0 ?? .codex
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        let key = try container.decode(String.self)
        self = Self.fromPersistentKey(key)
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(persistentKey)
    }
}

// MARK: - AgentPermissionPolicy + Serialization

extension AgentPermissionPolicy: @retroactive Codable {
    private static var rawValueMap: [(AgentPermissionPolicy, String)] {
        [
            (.autoApproveAll, "autoApproveAll"),
            (.autoRejectHighRisk, "autoRejectHighRisk"),
            (.promptAlways, "promptAlways"),
        ]
    }

    var persistentKey: String {
        Self.rawValueMap.first(where: { $0.0 == self })?.1 ?? "promptAlways"
    }

    static func fromPersistentKey(_ key: String) -> AgentPermissionPolicy {
        rawValueMap.first(where: { $0.1 == key })?.0 ?? .promptAlways
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        let key = try container.decode(String.self)
        self = Self.fromPersistentKey(key)
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(persistentKey)
    }
}

// MARK: - AgentType Identifiable for ForEach

extension AgentType: @retroactive Identifiable {
    public var id: String { persistentKey }
}

// MARK: - Agent Selection State

/// Manages per-server agent selection state, persisted to UserDefaults.
@MainActor
final class AgentSelectionStore {
    static let shared = AgentSelectionStore()

    private let defaults = UserDefaults.standard
    private static let selectedAgentKey = "litter.selectedAgent"

    private init() {}

    /// Returns the selected `AgentType` for the given server, or nil if no selection has been made.
    func selectedAgentType(for serverId: String) -> AgentType? {
        guard let key = defaults.string(forKey: "\(Self.selectedAgentKey).\(serverId)") else {
            return nil
        }
        return .fromPersistentKey(key)
    }

    /// Persists the selected `AgentType` for a server.
    func setSelectedAgentType(_ agentType: AgentType?, for serverId: String) {
        let key = "\(Self.selectedAgentKey).\(serverId)"
        if let agentType {
            defaults.set(agentType.persistentKey, forKey: key)
        } else {
            defaults.removeObject(forKey: key)
        }
    }

    /// Returns the effective agent type for a server: the user's selection if set,
    /// otherwise auto-selects if only one agent is available, or nil if multiple agents
    /// are available and none is selected.
    func effectiveAgentType(for serverId: String, availableAgents: [AgentType]) -> AgentType? {
        // If user has made a selection, use it
        if let selected = selectedAgentType(for: serverId),
           availableAgents.contains(selected) {
            return selected
        }
        // Auto-select if only one agent available
        if availableAgents.count == 1 {
            let auto = availableAgents[0]
            setSelectedAgentType(auto, for: serverId)
            return auto
        }
        return nil
    }

    /// Whether the agent picker should be shown (i.e., more than one agent is available).
    func shouldShowPicker(availableAgents: [AgentType]) -> Bool {
        availableAgents.count > 1
    }
}

/// Manages per-agent permission policy preferences, persisted to UserDefaults.
@MainActor
final class AgentPermissionStore {
    static let shared = AgentPermissionStore()

    private let defaults = UserDefaults.standard
    private static let permissionPolicyKey = "litter.agentPermissionPolicy"
    private static let transportPreferenceKey = "litter.agentTransportPreference"

    private init() {}

    func permissionPolicy(for agentTypeId: String) -> AgentPermissionPolicy {
        guard let key = defaults.string(forKey: "\(Self.permissionPolicyKey).\(agentTypeId)") else {
            return .promptAlways
        }
        return .fromPersistentKey(key)
    }

    func setPermissionPolicy(_ policy: AgentPermissionPolicy, for agentTypeId: String) {
        defaults.set(policy.persistentKey, forKey: "\(Self.permissionPolicyKey).\(agentTypeId)")
    }

    func transportPreference(for agentTypeId: String) -> AgentType? {
        guard let key = defaults.string(forKey: "\(Self.transportPreferenceKey).\(agentTypeId)") else {
            return nil
        }
        return .fromPersistentKey(key)
    }

    func setTransportPreference(_ agentType: AgentType?, for agentTypeId: String) {
        let key = "\(Self.transportPreferenceKey).\(agentTypeId)"
        if let agentType {
            defaults.set(agentType.persistentKey, forKey: key)
        } else {
            defaults.removeObject(forKey: key)
        }
    }
}

// MARK: - Agent Picker View

/// Inline agent selector shown inside the model picker popover in HeaderView.
/// Lists available agents for the current server and allows selection.
struct InlineAgentSelectorView: View {
    let agentTypes: [AgentType]
    @Binding var selectedAgentType: AgentType
    @Environment(AppModel.self) private var appModel
    var onDismiss: () -> Void = {}

    var body: some View {
        VStack(spacing: 0) {
            if agentTypes.count > 1 {
                SectionHeader(label: "Agent")
                ForEach(agentTypes) { agentType in
                    agentRow(agentType)
                }
                Divider().background(LitterTheme.separator).padding(.leading, 16)
            }
        }
    }

    private func agentRow(_ agentType: AgentType) -> some View {
        Button {
            selectedAgentType = agentType
            onDismiss()
        } label: {
            HStack(spacing: 10) {
                AgentBadgeView(agentType: agentType, size: 16, showsLabel: true)
                    .frame(width: 70, alignment: .leading)

                VStack(alignment: .leading, spacing: 2) {
                    Text(agentType.displayName)
                        .litterFont(.footnote)
                        .foregroundColor(LitterTheme.textPrimary)
                    Text(agentType.transportLabel)
                        .litterFont(.caption2)
                        .foregroundColor(LitterTheme.textSecondary)
                }

                Spacer()

                if agentType == selectedAgentType {
                    Image(systemName: "checkmark")
                        .litterFont(size: 12, weight: .medium)
                        .foregroundColor(LitterTheme.accent)
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
    }
}

// MARK: - Agent Picker Sheet (for connection flow)

/// Full agent picker sheet shown during connection flow when server has multiple agents.
struct AgentPickerSheet: View {
    let agentTypes: [AgentType]
    @Binding var selectedAgentType: AgentType?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ZStack {
                LitterTheme.backgroundGradient.ignoresSafeArea()
                List {
                    ForEach(agentTypes) { agentType in
                        Button {
                            selectedAgentType = agentType
                            dismiss()
                        } label: {
                            HStack(spacing: 12) {
                                Image(systemName: agentType.icon)
                                    .font(.system(size: 20, weight: .medium))
                                    .foregroundColor(agentType.tintColor)
                                    .frame(width: 32)

                                VStack(alignment: .leading, spacing: 3) {
                                    Text(agentType.displayName)
                                        .litterFont(.subheadline)
                                        .foregroundColor(LitterTheme.textPrimary)
                                    Text(agentType.transportLabel + " transport")
                                        .litterFont(.caption)
                                        .foregroundColor(LitterTheme.textSecondary)
                                }

                                Spacer()
                            }
                            .padding(.vertical, 4)
                        }
                        .listRowBackground(LitterTheme.surface.opacity(0.6))
                    }
                }
                .scrollContentBackground(.hidden)
            }
            .navigationTitle("Select Agent")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Cancel") { dismiss() }
                        .foregroundColor(LitterTheme.textSecondary)
                }
            }
        }
    }
}

// MARK: - Section Header Helper

struct SectionHeader: View {
    let label: String

    var body: some View {
        HStack {
            Text(label.uppercased())
                .litterFont(.caption2, weight: .semibold)
                .foregroundColor(LitterTheme.textMuted)
            Spacer()
        }
        .padding(.horizontal, 16)
        .padding(.top, 8)
        .padding(.bottom, 4)
    }
}

// MARK: - Pi Thinking Level Control

/// Segmented picker for Pi thinking levels, shown when active agent is Pi.
struct PiThinkingLevelPicker: View {
    @Binding var thinkingLevel: String
    let onLevelChanged: (String) -> Void

    private let levels = [
        ("Quick", "off"),
        ("Balanced", "medium"),
        ("Deep", "high"),
    ]

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: "brain")
                .font(.system(size: 9, weight: .semibold))
                .foregroundColor(.purple)
            ForEach(levels, id: \.1) { label, value in
                Button {
                    thinkingLevel = value
                    onLevelChanged(value)
                } label: {
                    Text(label)
                        .font(.system(size: 10, weight: .medium))
                        .foregroundColor(thinkingLevel == value ? LitterTheme.textOnAccent : LitterTheme.textPrimary)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(thinkingLevel == value ? Color.purple : LitterTheme.surfaceLight)
                        .clipShape(Capsule())
                }
            }
        }
    }
}

// MARK: - Droid Autonomy Control

/// Slider for Droid autonomy levels, shown when active agent is Droid.
struct DroidAutonomyControl: View {
    @Binding var autonomyLevel: String
    let onLevelChanged: (String) -> Void

    private let levels = [
        ("Cautious", "suggest"),
        ("Normal", "normal"),
        ("Aggressive", "full"),
    ]

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: "robot")
                .font(.system(size: 9, weight: .semibold))
                .foregroundColor(.orange)
            ForEach(levels, id: \.1) { label, value in
                Button {
                    autonomyLevel = value
                    onLevelChanged(value)
                } label: {
                    Text(label)
                        .font(.system(size: 10, weight: .medium))
                        .foregroundColor(autonomyLevel == value ? LitterTheme.textOnAccent : LitterTheme.textPrimary)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(autonomyLevel == value ? .orange : LitterTheme.surfaceLight)
                        .clipShape(Capsule())
                }
            }
        }
    }
}

// MARK: - Preview

#if DEBUG
#Preview("Agent Picker") {
    VStack(spacing: 16) {
        InlineAgentSelectorView(
            agentTypes: [.codex, .piNative, .droidNative],
            selectedAgentType: .constant(.codex)
        )
    }
    .background(Color.black)
}
#endif
