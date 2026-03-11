import SwiftUI
import Inject

struct HeaderView: View {
    @ObserveInjection var inject
    @EnvironmentObject var serverManager: ServerManager
    @EnvironmentObject var appState: AppState
    @State private var isReloading = false
    @State private var showOAuth = false

    var topInset: CGFloat = 0

    private var activeConn: ServerConnection? {
        serverManager.activeConnection
    }

    var body: some View {
        VStack(spacing: 4) {
            HStack(alignment: .center, spacing: 10) {
                Button {
                    withAnimation(.easeInOut(duration: 0.25)) {
                        appState.sidebarOpen.toggle()
                    }
                } label: {
                    Image(systemName: "line.3.horizontal")
                        .font(.system(size: 16, weight: .medium))
                        .foregroundColor(LitterTheme.textSecondary)
                        .frame(width: 44, height: 44)
                        .modifier(GlassCircleModifier())
                }
                .accessibilityIdentifier("header.sidebarButton")

                Spacer(minLength: 0)

                Button {
                    withAnimation(.spring(response: 0.3, dampingFraction: 0.85)) {
                        appState.showModelSelector.toggle()
                    }
                } label: {
                    VStack(spacing: 2) {
                        HStack(spacing: 6) {
                            Circle()
                                .fill(authDotColor)
                                .frame(width: 6, height: 6)
                            Text(sessionModelLabel)
                                .foregroundColor(LitterTheme.textPrimary)
                            Text(sessionReasoningLabel)
                                .foregroundColor(LitterTheme.textSecondary)
                            Image(systemName: "chevron.down")
                                .font(.system(size: 10, weight: .semibold))
                                .foregroundColor(LitterTheme.textSecondary)
                                .rotationEffect(.degrees(appState.showModelSelector ? 180 : 0))
                        }
                        .font(LitterFont.styled(.subheadline, weight: .semibold))
                        .lineLimit(1)
                        .minimumScaleFactor(0.75)

                        Text(sessionDirectoryLabel)
                            .font(LitterFont.styled(.caption2, weight: .semibold))
                            .foregroundColor(LitterTheme.textSecondary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .modifier(GlassRectModifier(cornerRadius: 16))
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("header.modelPickerButton")

                Spacer(minLength: 0)

                reloadButton
            }
            .padding(.horizontal, 16)
            .padding(.top, topInset)
            .padding(.bottom, 4)

            if appState.showModelSelector {
                InlineModelSelectorView(onDismiss: {
                    withAnimation(.spring(response: 0.3, dampingFraction: 0.85)) {
                        appState.showModelSelector = false
                    }
                })
                .padding(.horizontal, 16)
                .transition(.opacity.combined(with: .scale(scale: 0.95, anchor: .top)))
            }
        }
        .background(
            LinearGradient(
                colors: LitterTheme.headerScrim,
                startPoint: .top,
                endPoint: .bottom
            )
            .padding(.bottom, -30)
            .ignoresSafeArea(.container, edges: .top)
            .allowsHitTesting(false)
        )
        .onChange(of: serverManager.activeThreadKey) { _, _ in
            syncSelectionFromActiveThread()
            Task { await loadModelsIfNeeded() }
        }
        .onChange(of: serverManager.activeThread?.model) { _, _ in
            syncSelectionFromActiveThread()
        }
        .onChange(of: serverManager.activeThread?.reasoningEffort) { _, _ in
            syncSelectionFromActiveThread()
        }
        .onChange(of: serverManager.activeThread?.cwd) { _, _ in
            syncSelectionFromActiveThread()
        }
        .task {
            syncSelectionFromActiveThread()
            await loadModelsIfNeeded()
        }
        .onChange(of: activeConn?.oauthURL) { _, url in
            showOAuth = url != nil
        }
        .onChange(of: activeConn?.loginCompleted) { _, completed in
            if completed == true {
                showOAuth = false
                activeConn?.loginCompleted = false
                Task {
                    await serverManager.refreshAllSessions()
                    await serverManager.syncActiveThreadFromServer()
                    syncSelectionFromActiveThread()
                }
            }
        }
        .sheet(isPresented: $showOAuth) {
            if let conn = activeConn, let url = conn.oauthURL {
                NavigationStack {
                    OAuthWebView(url: url, onCallbackIntercepted: { callbackURL in
                        conn.forwardOAuthCallback(callbackURL)
                    }) {
                        Task { await conn.cancelLogin() }
                    }
                    .ignoresSafeArea()
                    .navigationTitle("Login with ChatGPT")
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar {
                        ToolbarItem(placement: .topBarLeading) {
                            Button("Cancel") {
                                Task { await conn.cancelLogin() }
                                showOAuth = false
                            }
                            .foregroundColor(LitterTheme.danger)
                        }
                    }
                }
            }
        }
        .enableInjection()
    }

    private var authDotColor: Color {
        let conn = activeConn ?? serverManager.connections.values.first(where: { $0.isConnected })
        switch conn?.authStatus {
        case .chatgpt, .apiKey: return LitterTheme.accentStrong
        case .notLoggedIn: return LitterTheme.danger
        case .unknown, .none: return LitterTheme.textMuted
        }
    }

    private var sessionModelLabel: String {
        let selected = appState.selectedModel.trimmingCharacters(in: .whitespacesAndNewlines)
        if !selected.isEmpty { return selected }

        let threadModel = serverManager.activeThread?.model.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !threadModel.isEmpty { return threadModel }

        return "litter"
    }

    private var sessionReasoningLabel: String {
        let selected = appState.reasoningEffort.trimmingCharacters(in: .whitespacesAndNewlines)
        if !selected.isEmpty { return selected }

        let threadReasoning = serverManager.activeThread?.reasoningEffort?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !threadReasoning.isEmpty { return threadReasoning }

        return "default"
    }

    private var sessionDirectoryLabel: String {
        let currentDirectory = serverManager.activeThread?.cwd.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !currentDirectory.isEmpty {
            return abbreviateHomePath(currentDirectory)
        }

        let appDirectory = appState.currentCwd.trimmingCharacters(in: .whitespacesAndNewlines)
        if !appDirectory.isEmpty {
            return abbreviateHomePath(appDirectory)
        }

        return "~"
    }

    private func loadModelsIfNeeded() async {
        syncSelectionFromActiveThread()

        guard let conn = activeConn, conn.isConnected, !conn.modelsLoaded else { return }
        do {
            let resp = try await conn.listModels()
            conn.models = resp.data
            conn.modelsLoaded = true
            if appState.selectedModel.isEmpty {
                if let defaultModel = resp.data.first(where: { $0.isDefault }) {
                    appState.selectedModel = defaultModel.id
                    appState.reasoningEffort = defaultModel.defaultReasoningEffort
                } else if let first = resp.data.first {
                    appState.selectedModel = first.id
                    appState.reasoningEffort = first.defaultReasoningEffort
                }
            }
        } catch {}
    }

    private func syncSelectionFromActiveThread() {
        let threadModel = serverManager.activeThread?.model.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !threadModel.isEmpty && appState.selectedModel != threadModel {
            appState.selectedModel = threadModel
        }

        let threadReasoning = serverManager.activeThread?.reasoningEffort?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !threadReasoning.isEmpty && appState.reasoningEffort != threadReasoning {
            appState.reasoningEffort = threadReasoning
        }

        let threadCwd = serverManager.activeThread?.cwd.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !threadCwd.isEmpty && appState.currentCwd != threadCwd {
            appState.currentCwd = threadCwd
        }
    }

    private var reloadButton: some View {
        Button {
            Task {
                isReloading = true
                let conn = activeConn ?? serverManager.connections.values.first(where: { $0.isConnected })
                if conn?.authStatus == .notLoggedIn {
                    await conn?.logout()
                    await conn?.loginWithChatGPT()
                } else {
                    await serverManager.refreshAllSessions()
                    await serverManager.syncActiveThreadFromServer()
                    syncSelectionFromActiveThread()
                }
                isReloading = false
            }
        } label: {
            Group {
                if isReloading {
                    ProgressView()
                        .scaleEffect(0.7)
                        .tint(LitterTheme.accent)
                } else {
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundColor(serverManager.hasAnyConnection ? LitterTheme.accent : LitterTheme.textMuted)
                }
            }
            .frame(width: 44, height: 44)
            .modifier(GlassCircleModifier())
        }
        .accessibilityIdentifier("header.reloadButton")
        .disabled(isReloading || !serverManager.hasAnyConnection)
    }

}

struct InlineModelSelectorView: View {
    @EnvironmentObject var serverManager: ServerManager
    @EnvironmentObject var appState: AppState
    var onDismiss: () -> Void

    private var models: [CodexModel] {
        serverManager.activeConnection?.models ?? []
    }

    private var currentModel: CodexModel? {
        models.first { $0.id == appState.selectedModel }
    }

    var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(spacing: 0) {
                    ForEach(models) { model in
                        Button {
                            appState.selectedModel = model.id
                            appState.reasoningEffort = model.defaultReasoningEffort
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    HStack(spacing: 6) {
                                        Text(model.displayName)
                                            .font(LitterFont.styled(.footnote))
                                            .foregroundColor(LitterTheme.textPrimary)
                                        if model.isDefault {
                                            Text("default")
                                                .font(LitterFont.styled(.caption2, weight: .medium))
                                                .foregroundColor(LitterTheme.accent)
                                                .padding(.horizontal, 6)
                                                .padding(.vertical, 1)
                                                .background(LitterTheme.accent.opacity(0.15))
                                                .clipShape(Capsule())
                                        }
                                    }
                                    Text(model.description)
                                        .font(LitterFont.styled(.caption2))
                                        .foregroundColor(LitterTheme.textSecondary)
                                }
                                Spacer()
                                if model.id == appState.selectedModel {
                                    Image(systemName: "checkmark")
                                        .font(.system(size: 12, weight: .medium))
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
                                appState.reasoningEffort = effort.reasoningEffort
                            } label: {
                                Text(effort.reasoningEffort)
                                    .font(LitterFont.styled(.caption2, weight: .medium))
                                    .foregroundColor(effort.reasoningEffort == appState.reasoningEffort ? LitterTheme.textOnAccent : LitterTheme.textPrimary)
                                    .padding(.horizontal, 10)
                                    .padding(.vertical, 5)
                                    .background(effort.reasoningEffort == appState.reasoningEffort ? LitterTheme.accent : LitterTheme.surfaceLight)
                                    .clipShape(Capsule())
                            }
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 8)
                }
            }
        }
        .padding(.vertical, 4)
        .fixedSize(horizontal: false, vertical: true)
        .modifier(GlassRectModifier(cornerRadius: 16))
    }
}

struct ModelSelectorSheet: View {
    @EnvironmentObject var serverManager: ServerManager
    @EnvironmentObject var appState: AppState

    private var models: [CodexModel] {
        serverManager.activeConnection?.models ?? []
    }

    private var currentModel: CodexModel? {
        models.first { $0.id == appState.selectedModel }
    }

    var body: some View {
        VStack(spacing: 0) {
            ForEach(models) { model in
                Button {
                    appState.selectedModel = model.id
                    appState.reasoningEffort = model.defaultReasoningEffort
                } label: {
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(model.displayName)
                                    .font(LitterFont.styled(.footnote))
                                    .foregroundColor(LitterTheme.textPrimary)
                                if model.isDefault {
                                    Text("default")
                                        .font(LitterFont.styled(.caption2, weight: .medium))
                                        .foregroundColor(LitterTheme.accent)
                                        .padding(.horizontal, 6)
                                        .padding(.vertical, 1)
                                        .background(LitterTheme.accent.opacity(0.15))
                                        .clipShape(Capsule())
                                }
                            }
                            Text(model.description)
                                .font(LitterFont.styled(.caption2))
                                .foregroundColor(LitterTheme.textSecondary)
                        }
                        Spacer()
                        if model.id == appState.selectedModel {
                            Image(systemName: "checkmark")
                                .font(.system(size: 12, weight: .medium))
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
                                appState.reasoningEffort = effort.reasoningEffort
                            } label: {
                                Text(effort.reasoningEffort)
                                    .font(LitterFont.styled(.caption2, weight: .medium))
                                    .foregroundColor(effort.reasoningEffort == appState.reasoningEffort ? LitterTheme.textOnAccent : LitterTheme.textPrimary)
                                    .padding(.horizontal, 10)
                                    .padding(.vertical, 5)
                                    .background(effort.reasoningEffort == appState.reasoningEffort ? LitterTheme.accent : LitterTheme.surfaceLight)
                                    .clipShape(Capsule())
                            }
                        }
                    }
                    .padding(.horizontal, 20)
                    .padding(.vertical, 12)
                }
            }

            Spacer()
        }
        .padding(.top, 20)
        .background(.ultraThinMaterial)
    }
}

#if DEBUG
#Preview("Header") {
    LitterPreviewScene {
        HeaderView()
    }
}
#endif
