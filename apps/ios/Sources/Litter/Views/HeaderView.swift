import SafariServices
import SwiftUI

struct HeaderView: View {
    @Environment(AppState.self) private var appState
    @Environment(AppModel.self) private var appModel
    let thread: AppThreadSnapshot
    @State private var pulsing = false
    @AppStorage("fastMode") private var fastMode = false
    @AppStorage("piThinkingLevel") private var piThinkingLevel = "medium"
    @AppStorage("droidAutonomyLevel") private var droidAutonomyLevel = "normal"

    private var server: AppServerSnapshot? {
        appModel.snapshot?.serverSnapshot(for: thread.key.serverId)
    }

    private var availableModels: [ModelInfo] {
        appModel.availableModels(for: thread.key.serverId)
    }

    /// The available agent types for the current server from discovery data.
    private var availableAgentTypes: [AgentType] {
        AgentSelectionStore.shared.agentTypes(for: thread.key.serverId)
    }

    /// The currently selected agent type for this server.
    private var currentAgentType: AgentType {
        AgentSelectionStore.shared.selectedAgentType(for: thread.key.serverId)
            ?? availableAgentTypes.first
            ?? .codex
    }

    var body: some View {
        Button {
            appState.showModelSelector.toggle()
        } label: {
            VStack(spacing: 2) {
                HStack(spacing: 6) {
                    Circle()
                        .fill(statusDotColor)
                        .frame(width: 6, height: 6)
                        .opacity(shouldPulse ? (pulsing ? 0.3 : 1.0) : 1.0)
                        .animation(shouldPulse ? .easeInOut(duration: 0.8).repeatForever(autoreverses: true) : .default, value: pulsing)
                        .onChange(of: shouldPulse) { _, pulse in
                            pulsing = pulse
                        }
                    if fastMode {
                        Image(systemName: "bolt.fill")
                            .font(LitterFont.styled(size: 10, weight: .semibold))
                            .foregroundColor(LitterTheme.warning)
                    }
                    Text(sessionModelLabel)
                        .foregroundColor(LitterTheme.textPrimary)
                    Text(sessionReasoningLabel)
                        .foregroundColor(LitterTheme.textSecondary)
                    Image(systemName: "chevron.down")
                        .font(LitterFont.styled(size: 10, weight: .semibold))
                        .foregroundColor(LitterTheme.textSecondary)
                        .rotationEffect(.degrees(appState.showModelSelector ? 180 : 0))
                }
                .font(LitterFont.styled(size: 14, weight: .semibold))
                .lineLimit(1)
                .minimumScaleFactor(0.75)

                HStack(spacing: 6) {
                    Text(sessionDirectoryLabel)
                        .font(LitterFont.styled(size: 11, weight: .semibold))
                        .foregroundColor(LitterTheme.textSecondary)
                        .lineLimit(1)
                        .truncationMode(.middle)

                    // Agent type badge — show when non-Codex or multi-agent
                    if showAgentBadge {
                        AgentTypePill(agentType: currentAgentType)
                    }

                    if thread.collaborationMode == .plan {
                        Text("plan")
                            .font(LitterFont.styled(size: 11, weight: .bold))
                            .foregroundColor(.black)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(LitterTheme.accent)
                            .clipShape(Capsule())
                    }

                    if server?.isIpcConnected == true, ExperimentalFeatures.shared.isEnabled(.ipc) {
                        Text("IPC")
                            .font(LitterFont.styled(size: 11, weight: .bold))
                            .foregroundColor(LitterTheme.accentStrong)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(LitterTheme.accentStrong.opacity(0.14))
                            .clipShape(Capsule())
                    }
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .frame(maxWidth: 240)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("header.modelPickerButton")
        .popover(
            isPresented: Binding(
                get: { appState.showModelSelector },
                set: { appState.showModelSelector = $0 }
            ),
            attachmentAnchor: .rect(.bounds),
            arrowEdge: .top
        ) {
            ConversationModelPickerPanel(thread: thread)
                .presentationCompactAdaptation(.popover)
        }
        .task(id: thread.key) {
            await loadModelsIfNeeded()
        }
    }

    /// Whether to show the agent type badge in the header subtitle row.
    /// Shown when there are multiple agent types or the current agent is non-Codex.
    private var showAgentBadge: Bool {
        availableAgentTypes.count > 1 || currentAgentType != .codex
    }

    private var shouldPulse: Bool {
        guard let transportState = server?.transportState else { return false }
        return transportState == .connecting || transportState == .unresponsive
    }

    private var statusDotColor: Color {
        guard let server else {
            return LitterTheme.textMuted
        }
        switch server.transportState {
        case .connecting, .unresponsive:
            return .orange
        case .connected:
            if server.hasIpc && server.ipcState == .disconnected && ExperimentalFeatures.shared.isEnabled(.ipc) {
                return .orange
            }
            if server.isLocal {
                switch server.account {
                case .chatgpt?, .apiKey?:
                    return LitterTheme.success
                case nil:
                    return LitterTheme.danger
                }
            }
            return server.account == nil ? .orange : LitterTheme.success
        case .disconnected:
            return LitterTheme.danger
        case .unknown:
            return LitterTheme.textMuted
        }
    }

    private var sessionModelLabel: String {
        let pendingModel = appState.selectedModel.trimmingCharacters(in: .whitespacesAndNewlines)
        if !pendingModel.isEmpty { return pendingModel }

        let threadModel = (thread.model ?? thread.info.model ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        if !threadModel.isEmpty { return threadModel }

        return "litter"
    }

    private var sessionReasoningLabel: String {
        let pendingReasoning = appState.reasoningEffort.trimmingCharacters(in: .whitespacesAndNewlines)
        if !pendingReasoning.isEmpty { return pendingReasoning }

        let threadReasoning = thread.reasoningEffort?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !threadReasoning.isEmpty { return threadReasoning }

        // Fall back to the model's default reasoning effort from the loaded model list.
        let currentModel = (thread.model ?? thread.info.model ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        if let model = availableModels.first(where: { $0.model == currentModel }),
           !model.defaultReasoningEffort.wireValue.isEmpty {
            return model.defaultReasoningEffort.wireValue
        }

        return "default"
    }

    private var sessionDirectoryLabel: String {
        let currentDirectory = (thread.info.cwd ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        if !currentDirectory.isEmpty {
            return abbreviateHomePath(currentDirectory)
        }

        return "~"
    }

    private var selectedModelBinding: Binding<String> {
        Binding(
            get: {
                let pending = appState.selectedModel.trimmingCharacters(in: .whitespacesAndNewlines)
                if !pending.isEmpty { return pending }
                return (thread.model ?? thread.info.model ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            },
            set: { appState.selectedModel = $0 }
        )
    }

    private var reasoningEffortBinding: Binding<String> {
        Binding(
            get: {
                let pending = appState.reasoningEffort.trimmingCharacters(in: .whitespacesAndNewlines)
                if !pending.isEmpty { return pending }
                return thread.reasoningEffort?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            },
            set: { appState.reasoningEffort = $0 }
        )
    }

    private func loadModelsIfNeeded() async {
        await appModel.loadConversationMetadataIfNeeded(serverId: thread.key.serverId)
    }
}

struct ConversationModelPickerPanel: View {
    @Environment(AppState.self) private var appState
    @Environment(AppModel.self) private var appModel
    let thread: AppThreadSnapshot
    @State private var selectedAgentType: AgentType = .codex

    private var availableModels: [ModelInfo] {
        appModel.availableModels(for: thread.key.serverId)
    }

    /// Available agent types for the current server.
    private var availableAgentTypes: [AgentType] {
        AgentSelectionStore.shared.agentTypes(for: thread.key.serverId)
    }

    var body: some View {
        VStack(spacing: 0) {
            // Agent selector (only shown when multiple agents available)
            if availableAgentTypes.count > 1 {
                InlineAgentSelectorView(
                    agentTypes: availableAgentTypes,
                    selectedAgentType: $selectedAgentType,
                    onDismiss: {
                        appState.showModelSelector = false
                    }
                )
                Divider().background(LitterTheme.separator)
            }

            InlineModelSelectorView(
                models: availableModels,
                selectedModel: selectedModelBinding,
                reasoningEffort: reasoningEffortBinding,
                threadKey: thread.key,
                collaborationMode: thread.collaborationMode,
                activeAgentType: selectedAgentType,
                onDismiss: {
                    appState.showModelSelector = false
                }
            )
        }
        .padding(.horizontal, 16)
        .padding(.top, 8)
        .padding(.bottom, 8)
        .task(id: thread.key) {
            selectedAgentType = AgentSelectionStore.shared.effectiveAgentType(
                for: thread.key.serverId,
                availableAgents: availableAgentTypes
            ) ?? .codex
            await appModel.loadConversationMetadataIfNeeded(serverId: thread.key.serverId)
        }
        .onChange(of: selectedAgentType) { _, newValue in
            AgentSelectionStore.shared.setSelectedAgentType(newValue, for: thread.key.serverId)
        }
    }

    private var selectedModelBinding: Binding<String> {
        Binding(
            get: {
                let pending = appState.selectedModel.trimmingCharacters(in: .whitespacesAndNewlines)
                if !pending.isEmpty { return pending }
                return (thread.model ?? thread.info.model ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            },
            set: { appState.selectedModel = $0 }
        )
    }

    private var reasoningEffortBinding: Binding<String> {
        Binding(
            get: {
                let pending = appState.reasoningEffort.trimmingCharacters(in: .whitespacesAndNewlines)
                if !pending.isEmpty { return pending }
                return thread.reasoningEffort?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            },
            set: { appState.reasoningEffort = $0 }
        )
    }
}

struct ConversationToolbarControls: View {
    enum Control {
        case reload
        case info
    }

    @Environment(AppState.self) private var appState
    @Environment(AppModel.self) private var appModel
    let thread: AppThreadSnapshot
    let control: Control
    var onInfo: (() -> Void)?
    @State private var isReloading = false
    @State private var remoteAuthSession: RemoteAuthSession?

    private var server: AppServerSnapshot? {
        appModel.snapshot?.serverSnapshot(for: thread.key.serverId)
    }

    var body: some View {
        Group {
            switch control {
            case .reload:
                reloadButton
            case .info:
                infoButton
            }
        }
        .frame(width: 28, height: 28)
        .contentShape(Rectangle())
        .buttonStyle(.plain)
        .sheet(item: $remoteAuthSession) { session in
            InAppSafariView(url: session.url)
                .ignoresSafeArea()
        }
        .onChange(of: server?.account != nil) { _, isLoggedIn in
            if isLoggedIn {
                remoteAuthSession = nil
            }
        }
    }

    private var reloadButton: some View {
        Button {
            Task {
                isReloading = true
                defer { isReloading = false }
                if await handleRemoteLoginIfNeeded() {
                    return
                }
                if server?.account == nil {
                    appState.showSettings = true
                } else {
                    let nextKey = try? await appModel.reloadThread(
                        key: thread.key,
                        launchConfig: reloadLaunchConfig(),
                        cwdOverride: thread.info.cwd
                    )
                    if let nextKey {
                        appModel.store.setActiveThread(
                            key: nextKey
                        )
                    }
                }
            }
        } label: {
            reloadButtonLabel
        }
        .accessibilityIdentifier("header.reloadButton")
        .disabled(isReloading || server?.isConnected != true)
    }

    @ViewBuilder
    private var reloadButtonLabel: some View {
        if isReloading {
            ProgressView()
                .scaleEffect(0.7)
                .tint(LitterTheme.accent)
        } else {
            Image(systemName: "arrow.clockwise")
                .font(LitterFont.styled(size: 16, weight: .semibold))
                .foregroundColor(server?.isConnected == true ? LitterTheme.accent : LitterTheme.textMuted)
        }
    }

    private var infoButton: some View {
        Button {
            onInfo?()
        } label: {
            Image(systemName: "info.circle")
                .font(LitterFont.styled(size: 16, weight: .semibold))
                .foregroundColor(LitterTheme.accent)
        }
        .accessibilityIdentifier("header.infoButton")
    }

    private func handleRemoteLoginIfNeeded() async -> Bool {
        guard let server, !server.isLocal else {
            return false
        }
        guard server.account == nil else {
            return false
        }
        do {
            let authURL = try await appModel.client.startRemoteSshOauthLogin(
                serverId: server.serverId
            )
            if let url = URL(string: authURL) {
                await MainActor.run {
                    remoteAuthSession = RemoteAuthSession(url: url)
                }
            }
        } catch {}
        return true
    }

    private func reloadLaunchConfig() -> AppThreadLaunchConfig {
        let pendingModel = appState.selectedModel.trimmingCharacters(in: .whitespacesAndNewlines)
        let resolvedModel = pendingModel.isEmpty ? nil : pendingModel
        return AppThreadLaunchConfig(
            model: resolvedModel,
            approvalPolicy: appState.launchApprovalPolicy(for: thread.key),
            sandbox: appState.launchSandboxMode(for: thread.key),
            developerInstructions: nil,
            persistExtendedHistory: true
        )
    }
}

private struct RemoteAuthSession: Identifiable {
    let id = UUID()
    let url: URL
}

struct InlineModelSelectorView: View {
    let models: [ModelInfo]
    @Binding var selectedModel: String
    @Binding var reasoningEffort: String
    var threadKey: ThreadKey
    var collaborationMode: AppModeKind = .default
    var activeAgentType: AgentType = .codex
    @Environment(AppModel.self) private var appModel
    @AppStorage("fastMode") private var fastMode = false
    @AppStorage("piThinkingLevel") private var piThinkingLevel = "medium"
    @AppStorage("droidAutonomyLevel") private var droidAutonomyLevel = "normal"
    var onDismiss: () -> Void

    private var currentModel: ModelInfo? {
        models.first { $0.id == selectedModel }
    }

    var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(spacing: 0) {
                    ForEach(models) { model in
                        Button {
                            selectedModel = model.id
                            reasoningEffort = model.defaultReasoningEffort.wireValue
                            onDismiss()
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    HStack(spacing: 6) {
                                        Text(model.displayName)
                                            .litterFont(.footnote)
                                            .foregroundColor(LitterTheme.textPrimary)
                                        if model.isDefault {
                                            Text("default")
                                                .litterFont(.caption2, weight: .medium)
                                                .foregroundColor(LitterTheme.accent)
                                                .padding(.horizontal, 6)
                                                .padding(.vertical, 1)
                                                .background(LitterTheme.accent.opacity(0.15))
                                                .clipShape(Capsule())
                                        }
                                    }
                                    Text(model.description)
                                        .litterFont(.caption2)
                                        .foregroundColor(LitterTheme.textSecondary)
                                }
                                Spacer()
                                if model.id == selectedModel {
                                    Image(systemName: "checkmark")
                                        .litterFont(size: 12, weight: .medium)
                                        .foregroundColor(LitterTheme.accent)
                                }
                            }
                            .padding(.horizontal, 16)
                            .padding(.vertical, 8)
                        }
                        if model.id != models.last?.id {
                            Divider().background(LitterTheme.separator).padding(.leading, 16)
                        }
                    }
                }
            }
            .frame(maxHeight: 320)

            if let info = currentModel, !info.supportedReasoningEfforts.isEmpty {
                Divider().background(LitterTheme.separator).padding(.horizontal, 12)

                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(info.supportedReasoningEfforts) { effort in
                            Button {
                                reasoningEffort = effort.reasoningEffort.wireValue
                                onDismiss()
                            } label: {
                                Text(effort.reasoningEffort.wireValue)
                                    .litterFont(.caption2, weight: .medium)
                                    .foregroundColor(effort.reasoningEffort.wireValue == reasoningEffort ? LitterTheme.textOnAccent : LitterTheme.textPrimary)
                                    .padding(.horizontal, 10)
                                    .padding(.vertical, 5)
                                    .background(effort.reasoningEffort.wireValue == reasoningEffort ? LitterTheme.accent : LitterTheme.surfaceLight)
                                    .clipShape(Capsule())
                            }
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 8)
                }
            }

            Divider().background(LitterTheme.separator).padding(.horizontal, 12)

            // Agent-specific controls
            if activeAgentType.isPi {
                ScrollView(.horizontal, showsIndicators: false) {
                    PiThinkingLevelPicker(
                        thinkingLevel: $piThinkingLevel,
                        onLevelChanged: { _ in }
                    )
                    .padding(.horizontal, 16)
                }
                .padding(.vertical, 4)
            } else if activeAgentType.isDroid {
                ScrollView(.horizontal, showsIndicators: false) {
                    DroidAutonomyControl(
                        autonomyLevel: $droidAutonomyLevel,
                        onLevelChanged: { _ in }
                    )
                    .padding(.horizontal, 16)
                }
                .padding(.vertical, 4)
            }

            HStack(spacing: 6) {
                Button {
                    let next: AppModeKind = collaborationMode == .plan ? .default : .plan
                    Task {
                        try? await appModel.store.setThreadCollaborationMode(
                            key: threadKey, mode: next
                        )
                    }
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "doc.text")
                            .litterFont(size: 9, weight: .semibold)
                        Text("Plan")
                            .litterFont(.caption2, weight: .medium)
                    }
                    .foregroundColor(collaborationMode == .plan ? .black : LitterTheme.textPrimary)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .background(collaborationMode == .plan ? LitterTheme.accent : LitterTheme.surfaceLight)
                    .clipShape(Capsule())
                }

                Button {
                    fastMode.toggle()
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "bolt.fill")
                            .litterFont(size: 9, weight: .semibold)
                        Text("Fast")
                            .litterFont(.caption2, weight: .medium)
                    }
                    .foregroundColor(fastMode ? LitterTheme.textOnAccent : LitterTheme.textPrimary)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .background(fastMode ? LitterTheme.warning : LitterTheme.surfaceLight)
                    .clipShape(Capsule())
                }
                Spacer()
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
        .padding(.vertical, 4)
        .fixedSize(horizontal: false, vertical: true)
    }
}

private struct InAppSafariView: UIViewControllerRepresentable {
    let url: URL

    func makeUIViewController(context: Context) -> SFSafariViewController {
        let controller = SFSafariViewController(url: url)
        controller.dismissButtonStyle = .close
        return controller
    }

    func updateUIViewController(_ uiViewController: SFSafariViewController, context: Context) {}
}

struct ModelSelectorSheet: View {
    let models: [ModelInfo]
    @Binding var selectedModel: String
    @Binding var reasoningEffort: String
    @AppStorage("fastMode") private var fastMode = false

    private var currentModel: ModelInfo? {
        models.first { $0.id == selectedModel }
    }

    var body: some View {
        VStack(spacing: 0) {
            ForEach(models) { model in
                Button {
                    selectedModel = model.id
                    reasoningEffort = model.defaultReasoningEffort.wireValue
                } label: {
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(model.displayName)
                                    .litterFont(.footnote)
                                    .foregroundColor(LitterTheme.textPrimary)
                                if model.isDefault {
                                    Text("default")
                                        .litterFont(.caption2, weight: .medium)
                                        .foregroundColor(LitterTheme.accent)
                                        .padding(.horizontal, 6)
                                        .padding(.vertical, 1)
                                        .background(LitterTheme.accent.opacity(0.15))
                                        .clipShape(Capsule())
                                }
                            }
                            Text(model.description)
                                .litterFont(.caption2)
                                .foregroundColor(LitterTheme.textSecondary)
                        }
                        Spacer()
                        if model.id == selectedModel {
                            Image(systemName: "checkmark")
                                .litterFont(size: 12, weight: .medium)
                                .foregroundColor(LitterTheme.accent)
                        }
                    }
                    .padding(.horizontal, 20)
                    .padding(.vertical, 12)
                }
                Divider().background(LitterTheme.separator).padding(.leading, 20)
            }

            if let info = currentModel, !info.supportedReasoningEfforts.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(info.supportedReasoningEfforts) { effort in
                            Button {
                                reasoningEffort = effort.reasoningEffort.wireValue
                            } label: {
                                Text(effort.reasoningEffort.wireValue)
                                    .litterFont(.caption2, weight: .medium)
                                    .foregroundColor(effort.reasoningEffort.wireValue == reasoningEffort ? LitterTheme.textOnAccent : LitterTheme.textPrimary)
                                    .padding(.horizontal, 10)
                                    .padding(.vertical, 5)
                                    .background(effort.reasoningEffort.wireValue == reasoningEffort ? LitterTheme.accent : LitterTheme.surfaceLight)
                                    .clipShape(Capsule())
                            }
                        }
                    }
                    .padding(.horizontal, 20)
                    .padding(.vertical, 12)
                }
            }

            Divider().background(LitterTheme.separator).padding(.leading, 20)

            HStack(spacing: 6) {
                Button {
                    fastMode.toggle()
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "bolt.fill")
                            .litterFont(size: 9, weight: .semibold)
                        Text("Fast")
                            .litterFont(.caption2, weight: .medium)
                    }
                    .foregroundColor(fastMode ? LitterTheme.textOnAccent : LitterTheme.textPrimary)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .background(fastMode ? LitterTheme.warning : LitterTheme.surfaceLight)
                    .clipShape(Capsule())
                }
                Spacer()
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 12)

            Spacer()
        }
        .padding(.top, 20)
        .background(.ultraThinMaterial)
    }
}

#if DEBUG
#Preview("Header") {
    let appModel = LitterPreviewData.makeConversationAppModel()
    LitterPreviewScene(appModel: appModel) {
        HeaderView(thread: appModel.snapshot!.threads[0])
    }
}
#endif
