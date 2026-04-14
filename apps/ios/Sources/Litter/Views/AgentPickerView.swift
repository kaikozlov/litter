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

    // MARK: - ACP Profile & Remote Command Selection

    private static let selectedACPProfileIdKey = "litter.selectedACPProfileId"
    private static let selectedRemoteCommandKey = "litter.selectedRemoteCommand"

    /// Persists the selected ACP profile ID for a context (e.g., "lastSelection").
    func setSelectedACPProfileId(_ profileId: UUID?, for context: String) {
        let key = "\(Self.selectedACPProfileIdKey).\(context)"
        if let profileId {
            defaults.set(profileId.uuidString, forKey: key)
        } else {
            defaults.removeObject(forKey: key)
        }
    }

    /// Returns the previously selected ACP profile ID for a context.
    func selectedACPProfileId(for context: String) -> UUID? {
        guard let string = defaults.string(forKey: "\(Self.selectedACPProfileIdKey).\(context)") else {
            return nil
        }
        return UUID(uuidString: string)
    }

    /// Persists the selected/edited remote command for GenericAcp.
    func setSelectedRemoteCommand(_ command: String?) {
        if let command {
            defaults.set(command, forKey: Self.selectedRemoteCommandKey)
        } else {
            defaults.removeObject(forKey: Self.selectedRemoteCommandKey)
        }
    }

    /// Returns the persisted remote command for GenericAcp, or nil if none.
    func selectedRemoteCommand() -> String? {
        defaults.string(forKey: Self.selectedRemoteCommandKey)
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

                if compatible && agentType.usesACP {
                    Text("ACP")
                        .litterFont(.caption2, weight: .semibold)
                        .foregroundColor(AgentType.genericAcp.tintColor)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 2)
                        .background(AgentType.genericAcp.tintColor.opacity(0.15))
                        .clipShape(Capsule())
                }

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
/// Includes detected agents from the server plus ACP profiles for GenericAcp connections.
struct AgentPickerSheet: View {
    let agentInfos: [AgentInfo]
    /// The transports available on the active connection.
    /// If empty, all agents are considered compatible (backward compat).
    let availableTransports: [AgentType]
    @Binding var selectedAgentType: AgentType?
    @Environment(\.dismiss) private var dismiss

    /// Selected ACP profile when user picks a GenericAcp entry.
    @State private var selectedACPProfile: ACPProfile?
    /// Custom command override text (pre-populated from profile, editable).
    @State private var customCommand = ""
    /// Whether the custom command field is showing (after user picks an ACP profile).
    @State private var showingCustomCommand = false
    /// Validation error for the custom command field.
    @State private var commandError: String?
    /// ACP profiles loaded from the store.
    @State private var acpProfiles: [ACPProfile] = []

    /// Returns true if this agent has at least one transport matching the available transports.
    private func isCompatible(_ info: AgentInfo) -> Bool {
        guard !availableTransports.isEmpty else { return true }
        return !Set(info.detectedTransports).isDisjoint(with: Set(availableTransports))
    }

    /// Whether GenericAcp is available as a transport option.
    private var genericAcpAvailable: Bool {
        availableTransports.isEmpty || availableTransports.contains(.genericAcp)
    }

    var body: some View {
        NavigationStack {
            ZStack {
                LitterTheme.backgroundGradient.ignoresSafeArea()
                List {
                    // MARK: Detected agents section
                    ForEach(agentInfos, id: \.id) { info in
                        let compatible = isCompatible(info)
                        let agentType = info.detectedTransports.first ?? .codex
                        Button {
                            guard compatible else { return }
                            if agentType == .genericAcp {
                                // Show profile sub-options or configure prompt
                                handleGenericAcpSelection()
                            } else {
                                selectedAgentType = agentType
                                dismiss()
                            }
                        } label: {
                            agentPickerRow(info: info, agentType: agentType, compatible: compatible)
                        }
                        .listRowBackground(LitterTheme.surface.opacity(0.6))
                        .disabled(!compatible)
                    }

                    // MARK: ACP Profiles section (when GenericAcp available)
                    if genericAcpAvailable {
                        acpProfilesSection
                    }
                }
                .scrollContentBackground(.hidden)
            }
            .navigationTitle("Select Agent")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    if showingCustomCommand {
                        Button("Back") {
                            showingCustomCommand = false
                            selectedACPProfile = nil
                        }
                        .foregroundColor(LitterTheme.textSecondary)
                    } else {
                        Button("Cancel") { dismiss() }
                            .foregroundColor(LitterTheme.textSecondary)
                    }
                }
            }
            .onAppear {
                acpProfiles = ACPProfileStore.shared.profiles()
            }
        }
    }

    // MARK: - Agent Row

    private func agentPickerRow(info: AgentInfo, agentType: AgentType, compatible: Bool) -> some View {
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

            // Show ACP badge for ACP-type transports
            if compatible && agentType.usesACP {
                Text("ACP")
                    .litterFont(.caption2, weight: .semibold)
                    .foregroundColor(AgentType.genericAcp.tintColor)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 2)
                    .background(AgentType.genericAcp.tintColor.opacity(0.15))
                    .clipShape(Capsule())
            }
        }
        .padding(.vertical, 4)
    }

    // MARK: - ACP Profiles Section

    @ViewBuilder
    private var acpProfilesSection: some View {
        if showingCustomCommand, let profile = selectedACPProfile {
            // Show custom command editor for the selected profile
            customCommandSection(profile)
        } else if !acpProfiles.isEmpty {
            Section {
                ForEach(acpProfiles) { profile in
                    Button {
                        selectACPProfile(profile)
                    } label: {
                        acpProfileRow(profile)
                    }
                }

                if acpProfiles.count > 1 {
                    Button {
                        // "Other command" — show blank command field
                        selectedACPProfile = nil
                        customCommand = ""
                        showingCustomCommand = true
                    } label: {
                        HStack(spacing: 10) {
                            Image(systemName: "terminal")
                                .font(.system(size: 16, weight: .medium))
                                .foregroundColor(AgentType.genericAcp.tintColor)
                                .frame(width: 32)
                            Text("Enter custom command…")
                                .litterFont(.subheadline)
                                .foregroundColor(LitterTheme.textSecondary)
                            Spacer()
                        }
                        .padding(.vertical, 4)
                    }
                    .listRowBackground(LitterTheme.surface.opacity(0.6))
                }
            } header: {
                SectionHeader(label: "ACP Providers")
            }
        } else {
            // No profiles configured — offer a configure link
            Section {
                Button {
                    // Dismiss picker and navigate to settings
                    dismiss()
                } label: {
                    HStack(spacing: 10) {
                        Image(systemName: AgentType.genericAcp.icon)
                            .font(.system(size: 16, weight: .medium))
                            .foregroundColor(AgentType.genericAcp.tintColor)
                            .frame(width: 32)
                            .opacity(0.6)
                        Text("No ACP profiles configured")
                            .litterFont(.subheadline)
                            .foregroundColor(LitterTheme.textMuted)
                        Spacer()
                        Text("Settings →")
                            .litterFont(.caption)
                            .foregroundColor(LitterTheme.textSecondary)
                    }
                    .padding(.vertical, 4)
                }
                .listRowBackground(LitterTheme.surface.opacity(0.6))
            } header: {
                SectionHeader(label: "ACP Providers")
            }
        }
    }

    private func acpProfileRow(_ profile: ACPProfile) -> some View {
        HStack(spacing: 10) {
            Image(systemName: profile.icon ?? AgentType.genericAcp.icon)
                .font(.system(size: 16, weight: .medium))
                .foregroundColor(AgentType.genericAcp.tintColor)
                .frame(width: 32)

            VStack(alignment: .leading, spacing: 2) {
                Text(profile.displayName)
                    .litterFont(.subheadline)
                    .foregroundColor(LitterTheme.textPrimary)
                Text(profile.remoteCommand)
                    .litterFont(.caption)
                    .foregroundColor(LitterTheme.textSecondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }

            Spacer()

            // ACP badge
            Text("ACP")
                .litterFont(.caption2, weight: .semibold)
                .foregroundColor(AgentType.genericAcp.tintColor)
                .padding(.horizontal, 5)
                .padding(.vertical, 2)
                .background(AgentType.genericAcp.tintColor.opacity(0.15))
                .clipShape(Capsule())
        }
        .padding(.vertical, 4)
    }

    // MARK: - Custom Command Editor

    private func customCommandSection(_ profile: ACPProfile?) -> some View {
        Section {
            VStack(alignment: .leading, spacing: 8) {
                if let profile {
                    HStack(spacing: 8) {
                        Image(systemName: profile.icon ?? AgentType.genericAcp.icon)
                            .font(.system(size: 14, weight: .medium))
                            .foregroundColor(AgentType.genericAcp.tintColor)
                        Text(profile.displayName)
                            .litterFont(.subheadline, weight: .medium)
                            .foregroundColor(LitterTheme.textPrimary)
                    }
                }

                TextField("Remote command", text: $customCommand)
                    .litterFont(.footnote)
                    .foregroundColor(LitterTheme.textPrimary)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled(true)
                    .font(.system(.body, design: .monospaced))
                    .padding(8)
                    .background(LitterTheme.surfaceLight)
                    .clipShape(RoundedRectangle(cornerRadius: 6))

                if let error = commandError {
                    Text(error)
                        .litterFont(.caption2)
                        .foregroundColor(LitterTheme.danger)
                }

                Button {
                    confirmCustomCommand()
                } label: {
                    HStack {
                        Spacer()
                        Text("Connect")
                            .litterFont(.subheadline, weight: .medium)
                            .foregroundColor(LitterTheme.textOnAccent)
                        Spacer()
                    }
                    .padding(.vertical, 8)
                    .background(commandIsValid ? AgentType.genericAcp.tintColor : LitterTheme.surfaceLight)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                }
                .disabled(!commandIsValid)
                .padding(.top, 4)
            }
            .padding(.vertical, 4)
        } header: {
            if let profile {
                SectionHeader(label: "Custom Command for \(profile.displayName)")
            } else {
                SectionHeader(label: "Custom Command")
            }
        }
        .listRowBackground(LitterTheme.surface.opacity(0.6))
    }

    // MARK: - Actions

    private func handleGenericAcpSelection() {
        let profiles = ACPProfileStore.shared.profiles()
        if profiles.count == 1 {
            // Auto-select the only profile
            selectACPProfile(profiles[0])
        }
        // If multiple or zero profiles, the section handles it
    }

    private func selectACPProfile(_ profile: ACPProfile) {
        selectedACPProfile = profile
        customCommand = profile.remoteCommand
        commandError = nil
        showingCustomCommand = true
    }

    private var commandIsValid: Bool {
        let trimmed = customCommand.trimmingCharacters(in: .whitespaces)
        return !trimmed.isEmpty && !containsDangerousShellCharacters(trimmed)
    }

    private func confirmCustomCommand() {
        let trimmed = customCommand.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else {
            commandError = "Command cannot be empty"
            return
        }
        guard !containsDangerousShellCharacters(trimmed) else {
            commandError = "Command contains potentially dangerous characters"
            return
        }

        // Persist the selected profile and command
        if let profile = selectedACPProfile {
            AgentSelectionStore.shared.setSelectedACPProfileId(profile.id, for: "lastSelection")
        }
        AgentSelectionStore.shared.setSelectedRemoteCommand(trimmed)

        selectedAgentType = .genericAcp
        dismiss()
    }

    private func containsDangerousShellCharacters(_ command: String) -> Bool {
        // Basic validation: reject commands with shell injection risks
        let dangerous = ["&&", "||", ";", "`", "$(", ">", ">>", "<", "|"]
        return dangerous.contains(where: { command.contains($0) })
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

#Preview("Agent Picker Sheet") {
    AgentPickerSheet(
        agentInfos: [
            AgentInfo(id: "codex", displayName: "Codex", description: "Code assistant", detectedTransports: [.codex], capabilities: []),
            AgentInfo(id: "pi", displayName: "Pi", description: "AI coding agent", detectedTransports: [.piNative], capabilities: []),
        ],
        availableTransports: [.codex, .piNative, .droidNative, .genericAcp],
        selectedAgentType: .constant(nil)
    )
}
#endif
