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
    private static let serverAgentTypesKey = "litter.serverAgentTypes"
    private static let serverSSHCapabilityKey = "litter.serverSSHCapability"

    /// In-memory cache of detected agent types per server, updated from discovery.
    private var serverAgentTypesCache: [String: [AgentType]] = [:]
    /// In-memory cache of agent info records per server, updated from discovery.
    private var serverAgentInfosCache: [String: [AgentInfo]] = [:]
    /// In-memory cache of SSH capability per server, updated from discovery.
    private var serverSSHCapabilityCache: [String: Bool] = [:]

    private init() {
        // Load persisted agent types cache
        if let data = defaults.data(forKey: Self.serverAgentTypesKey),
           let decoded = try? JSONDecoder().decode([String: [String]].self, from: data) {
            serverAgentTypesCache = decoded.mapValues { keys in
                keys.compactMap { AgentType.fromPersistentKey($0) == .codex && $0 != "codex" ? nil : AgentType.fromPersistentKey($0) }
            }
        }
    }

    /// Update the detected agent types for a server (called from discovery reconciliation).
    func updateAgentTypes(_ agentTypes: [AgentType], for serverId: String) {
        let current = serverAgentTypesCache[serverId] ?? []
        if current != agentTypes {
            serverAgentTypesCache[serverId] = agentTypes
            persistAgentTypesCache()
        }
    }

    /// Update the detected agent infos for a server (called from discovery reconciliation).
    func updateAgentInfos(_ agentInfos: [AgentInfo], for serverId: String) {
        serverAgentInfosCache[serverId] = agentInfos
        // Also update agent types from the infos if they carry data
        if !agentInfos.isEmpty {
            let types = agentInfos.map { $0.detectedTransports }.flatMap { $0 }.map { $0 }
            let uniqueTypes = Array(types)
            let current = serverAgentTypesCache[serverId] ?? []
            if current != uniqueTypes {
                serverAgentTypesCache[serverId] = uniqueTypes.isEmpty ? [.codex] : uniqueTypes
                persistAgentTypesCache()
            }
        }
    }

    /// Returns the detected agent types for a server, defaulting to [.codex].
    func agentTypes(for serverId: String) -> [AgentType] {
        serverAgentTypesCache[serverId] ?? [.codex]
    }

    /// Returns the detected agent infos for a server.
    func agentInfos(for serverId: String) -> [AgentInfo] {
        serverAgentInfosCache[serverId] ?? []
    }

    /// Update the SSH capability for a server (called from discovery reconciliation).
    func updateSSHCapability(_ hasSSH: Bool, for serverId: String) {
        serverSSHCapabilityCache[serverId] = hasSSH
    }

    /// Returns whether the server has SSH capability.
    func serverHasSSH(_ serverId: String) -> Bool {
        serverSSHCapabilityCache[serverId] ?? false
    }

    private func persistAgentTypesCache() {
        let encoded = serverAgentTypesCache.mapValues { types in
            types.map { $0.persistentKey }
        }
        if let data = try? JSONEncoder().encode(encoded) {
            defaults.set(data, forKey: Self.serverAgentTypesKey)
        }
    }

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
/// Agents with incompatible transports are grayed out and disabled.
struct InlineAgentSelectorView: View {
    let agentInfos: [AgentInfo]
    @Binding var selectedAgentType: AgentType
    /// The transports available on the active connection (e.g. SSH, WebSocket).
    /// If empty, all agents are considered compatible (backward compat).
    let availableTransports: [AgentType]
    @Environment(AppModel.self) private var appModel
    var onDismiss: () -> Void = {}

    /// Returns true if this agent has at least one transport matching the available transports.
    private func isCompatible(_ info: AgentInfo) -> Bool {
        guard !availableTransports.isEmpty else { return true }
        return !Set(info.detectedTransports).isDisjoint(with: Set(availableTransports))
    }

    var body: some View {
        VStack(spacing: 0) {
            if agentInfos.count > 1 {
                SectionHeader(label: "Agent")
                ForEach(agentInfos, id: \.id) { info in
                    agentRow(info)
                }
                Divider().background(LitterTheme.separator).padding(.leading, 16)
            }
        }
    }

    private func agentRow(_ info: AgentInfo) -> some View {
        let compatible = isCompatible(info)
        // Pick the best display agent type from detected transports
        let agentType = info.detectedTransports.first ?? .codex

        return Button {
            guard compatible else { return }
            selectedAgentType = agentType
            onDismiss()
        } label: {
            HStack(spacing: 10) {
                AgentBadgeView(agentType: agentType, size: 16, showsLabel: true)
                    .frame(width: 70, alignment: .leading)
                    .opacity(compatible ? 1.0 : 0.4)

                VStack(alignment: .leading, spacing: 2) {
                    Text(info.displayName)
                        .litterFont(.footnote)
                        .foregroundColor(compatible ? LitterTheme.textPrimary : LitterTheme.textMuted)
                    if compatible {
                        Text(info.detectedTransports.map { $0.transportLabel }.joined(separator: ", "))
                            .litterFont(.caption2)
                            .foregroundColor(LitterTheme.textSecondary)
                    } else {
                        Text("Transport unavailable")
                            .litterFont(.caption2)
                            .foregroundColor(LitterTheme.danger)
                    }
                }

                Spacer()

                if compatible && agentType == selectedAgentType {
                    Image(systemName: "checkmark")
                        .litterFont(size: 12, weight: .medium)
                        .foregroundColor(LitterTheme.accent)
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
        .disabled(!compatible)
    }
}

// MARK: - Agent Picker Sheet (for connection flow)

/// Full agent picker sheet shown during connection flow when server has multiple agents.
struct AgentPickerSheet: View {
    let agentInfos: [AgentInfo]
    /// The transports available on the active connection.
    /// If empty, all agents are considered compatible (backward compat).
    let availableTransports: [AgentType]
    @Binding var selectedAgentType: AgentType?
    @Environment(\.dismiss) private var dismiss

    /// Returns true if this agent has at least one transport matching the available transports.
    private func isCompatible(_ info: AgentInfo) -> Bool {
        guard !availableTransports.isEmpty else { return true }
        return !Set(info.detectedTransports).isDisjoint(with: Set(availableTransports))
    }

    var body: some View {
        NavigationStack {
            ZStack {
                LitterTheme.backgroundGradient.ignoresSafeArea()
                List {
                    ForEach(agentInfos, id: \.id) { info in
                        let compatible = isCompatible(info)
                        let agentType = info.detectedTransports.first ?? .codex
                        Button {
                            guard compatible else { return }
                            selectedAgentType = agentType
                            dismiss()
                        } label: {
                            HStack(spacing: 12) {
                                Image(systemName: agentType.icon)
                                    .font(.system(size: 20, weight: .medium))
                                    .foregroundColor(compatible ? agentType.tintColor : LitterTheme.textMuted)
                                    .frame(width: 32)
                                    .opacity(compatible ? 1.0 : 0.4)

                                VStack(alignment: .leading, spacing: 3) {
                                    Text(info.displayName)
                                        .litterFont(.subheadline)
                                        .foregroundColor(compatible ? LitterTheme.textPrimary : LitterTheme.textMuted)
                                    if compatible {
                                        Text(info.detectedTransports.map { $0.transportLabel }.joined(separator: ", ") + " transport")
                                            .litterFont(.caption)
                                            .foregroundColor(LitterTheme.textSecondary)
                                    } else {
                                        Text("Transport unavailable")
                                            .litterFont(.caption)
                                            .foregroundColor(LitterTheme.danger)
                                    }
                                }

                                Spacer()
                            }
                            .padding(.vertical, 4)
                        }
                        .listRowBackground(LitterTheme.surface.opacity(0.6))
                        .disabled(!compatible)
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
            agentInfos: [
                AgentInfo(id: "codex", displayName: "Codex", description: "Code assistant", detectedTransports: [.codex], capabilities: []),
                AgentInfo(id: "pi", displayName: "Pi", description: "AI coding agent", detectedTransports: [.piNative], capabilities: []),
                AgentInfo(id: "droid", displayName: "Droid", description: "Droid agent", detectedTransports: [.droidNative], capabilities: []),
            ],
            selectedAgentType: .constant(.codex),
            availableTransports: [.codex, .piNative, .droidNative]
        )
    }
    .background(Color.black)
}
#endif
