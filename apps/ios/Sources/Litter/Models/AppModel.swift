import Foundation
import Observation

@MainActor
@Observable
final class AppModel {
    private struct PendingStreamingDeltaEvent: Sendable {
        let key: ThreadKey
        let itemId: String
        let kind: ThreadStreamingDeltaKind
        var text: String
    }

    /// Pre-built Rust objects initialized off the main thread to avoid
    /// priority inversion (tokio runtime init blocks at default QoS).
    private struct RustBridges: @unchecked Sendable {
        let store: AppStore
        let client: AppClient
        let discovery: DiscoveryBridge
        let serverBridge: ServerBridge
        let ssh: SshBridge
    }

    /// Kick off Rust bridge construction on a background thread.
    /// Call from `AppDelegate.didFinishLaunching` before SwiftUI touches `shared`.
    nonisolated static func prewarmRustBridges() {
        _ = _prewarmResult
    }

    private nonisolated static let _prewarmResult: RustBridges = {
        RustBridges(
            store: AppStore(),
            client: AppClient(),
            discovery: DiscoveryBridge(),
            serverBridge: ServerBridge(),
            ssh: SshBridge()
        )
    }()

    static let shared = AppModel()

    struct ComposerPrefillRequest: Identifiable, Equatable {
        let id = UUID()
        let threadKey: ThreadKey
        let text: String
    }

    let store: AppStore
    let client: AppClient
    let discovery: DiscoveryBridge
    let serverBridge: ServerBridge
    let ssh: SshBridge

    private(set) var snapshot: AppSnapshotRecord?
    private(set) var lastError: String?
    private(set) var composerPrefillRequest: ComposerPrefillRequest?

    @ObservationIgnored private var subscription: AppStoreSubscription?
    @ObservationIgnored private var updateTask: Task<Void, Never>?
    @ObservationIgnored private var loadingModelServerIds: Set<String> = []
    @ObservationIgnored private var loadingRateLimitServerIds: Set<String> = []
    @ObservationIgnored private var recentConversationMetadataLoads: [String: Date] = [:]
    @ObservationIgnored private var pendingThreadRefreshKeys: Set<ThreadKey> = []
    @ObservationIgnored private var pendingThreadRefreshTask: Task<Void, Never>?
    @ObservationIgnored private var pendingActiveThreadHydrationKey: ThreadKey?
    @ObservationIgnored private var pendingActiveThreadHydrationTask: Task<Void, Never>?
    @ObservationIgnored private var pendingSnapshotRefreshTask: Task<Void, Never>?
    @ObservationIgnored private var pendingStreamingDeltaEvents: [PendingStreamingDeltaEvent] = []
    @ObservationIgnored private var pendingStreamingDeltaTask: Task<Void, Never>?
    @ObservationIgnored private var cachedThreadSnapshots: [ThreadKey: AppThreadSnapshot] = [:]

    init(
        store: AppStore? = nil,
        client: AppClient? = nil,
        discovery: DiscoveryBridge? = nil,
        serverBridge: ServerBridge? = nil,
        ssh: SshBridge? = nil
    ) {
        let bridges = Self._prewarmResult
        self.store = store ?? bridges.store
        self.client = client ?? bridges.client
        self.discovery = discovery ?? bridges.discovery
        self.serverBridge = serverBridge ?? bridges.serverBridge
        self.ssh = ssh ?? bridges.ssh
    }

    deinit {
        updateTask?.cancel()
        pendingThreadRefreshTask?.cancel()
        pendingActiveThreadHydrationTask?.cancel()
        pendingSnapshotRefreshTask?.cancel()
        pendingStreamingDeltaTask?.cancel()
    }

    func start() {
        guard updateTask == nil else { return }
        let subscription = store.subscribeUpdates()
        self.subscription = subscription
        updateTask = Task.detached(priority: .userInitiated) { [weak self, subscription] in
            guard let self else { return }
            await self.refreshSnapshot()
            while !Task.isCancelled {
                do {
                    let update = try await subscription.nextUpdate()
                    await self.handleStoreUpdate(update)
                } catch {
                    if Task.isCancelled { break }
                    await self.recordStoreSubscriptionError(error)
                    break
                }
            }
        }
    }

    func stop() {
        updateTask?.cancel()
        updateTask = nil
        pendingThreadRefreshTask?.cancel()
        pendingThreadRefreshTask = nil
        pendingThreadRefreshKeys.removeAll()
        pendingActiveThreadHydrationTask?.cancel()
        pendingActiveThreadHydrationTask = nil
        pendingActiveThreadHydrationKey = nil
        pendingSnapshotRefreshTask?.cancel()
        pendingSnapshotRefreshTask = nil
        pendingStreamingDeltaTask?.cancel()
        pendingStreamingDeltaTask = nil
        pendingStreamingDeltaEvents.removeAll()
        subscription = nil
    }

    func refreshSnapshot() async {
        pendingSnapshotRefreshTask?.cancel()
        pendingSnapshotRefreshTask = nil
        await performSnapshotRefresh()
    }

    private func performSnapshotRefresh() async {
        do {
            applySnapshot(try await store.snapshot())
        } catch {
            lastError = error.localizedDescription
        }
    }

    private func recordStoreSubscriptionError(_ error: Error) {
        lastError = error.localizedDescription
    }

    private func scheduleSnapshotRefreshDebounced() {
        guard pendingSnapshotRefreshTask == nil else { return }
        pendingSnapshotRefreshTask = Task { [weak self] in
            do {
                try await Task.sleep(nanoseconds: 75_000_000)
            } catch {
                return
            }
            guard let self else { return }
            self.pendingSnapshotRefreshTask = nil
            await self.performSnapshotRefresh()
        }
    }

    func activateThread(_ key: ThreadKey?) {
        restoreCachedThreadSnapshotIfNeeded(for: key)
        updateActiveThread(key)
        store.setActiveThread(key: key)
        scheduleDeferredActiveThreadHydrationIfNeeded(for: key)
    }

    func resumeThreadPreferringIPC(
        key: ThreadKey,
        launchConfig: AppThreadLaunchConfig,
        cwdOverride: String?
    ) async throws -> ThreadKey {
        if shouldUsePassiveIpcOpen(
            key: key,
            launchConfig: launchConfig,
            cwdOverride: cwdOverride
        ) {
            // Treat ordinary opens the same as reloads: perform an explicit
            // IPC-backed thread/read before we claim success. That keeps stale
            // "connected" snapshot state from turning a dead IPC tunnel into a
            // ghost-success open followed by downstream activation crashes.
            try await store.externalResumeThread(
                key: key,
                hostId: nil
            )
            return key
        }

        let trimmedCwdOverride = cwdOverride?.trimmingCharacters(in: .whitespacesAndNewlines)
        let requiresDistinctCwdOverride = requiresResumeCwdOverride(
            for: key,
            cwdOverride: trimmedCwdOverride
        )

        return try await client.resumeThread(
            serverId: key.serverId,
            params: launchConfig.threadResumeRequest(
                threadId: key.threadId,
                cwdOverride: requiresDistinctCwdOverride ? trimmedCwdOverride : nil
            )
        )
    }

    func reloadThreadPreferringIPC(
        key: ThreadKey,
        launchConfig: AppThreadLaunchConfig,
        cwdOverride: String?
    ) async throws -> ThreadKey {
        if shouldUsePassiveIpcOpen(
            key: key,
            launchConfig: launchConfig,
            cwdOverride: cwdOverride
        ) {
            try await store.externalResumeThread(
                key: key,
                hostId: nil
            )
            return key
        }

        let trimmedCwdOverride = cwdOverride?.trimmingCharacters(in: .whitespacesAndNewlines)
        let requiresDistinctCwdOverride = requiresResumeCwdOverride(
            for: key,
            cwdOverride: trimmedCwdOverride
        )

        return try await client.resumeThread(
            serverId: key.serverId,
            params: launchConfig.threadResumeRequest(
                threadId: key.threadId,
                cwdOverride: requiresDistinctCwdOverride ? trimmedCwdOverride : nil
            )
        )
    }

    private func shouldUsePassiveIpcOpen(
        key: ThreadKey,
        launchConfig: AppThreadLaunchConfig,
        cwdOverride: String?
    ) -> Bool {
        let trimmedCwdOverride = cwdOverride?.trimmingCharacters(in: .whitespacesAndNewlines)
        let requiresDistinctCwdOverride = requiresResumeCwdOverride(
            for: key,
            cwdOverride: trimmedCwdOverride
        )
        let requiresResumeOverrides =
            launchConfig.model != nil ||
            launchConfig.approvalPolicy != nil ||
            launchConfig.sandbox != nil ||
            !(launchConfig.developerInstructions?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true) ||
            requiresDistinctCwdOverride ||
            !launchConfig.persistExtendedHistory

        return !requiresResumeOverrides &&
            snapshot?.serverSnapshot(for: key.serverId)?.canResumeViaIpc == true
    }

    private func requiresResumeCwdOverride(
        for key: ThreadKey,
        cwdOverride: String?
    ) -> Bool {
        guard let normalizedOverride = cwdOverride, !normalizedOverride.isEmpty else {
            return false
        }

        let existingCwd =
            threadSnapshot(for: key)?.info.cwd?.trimmingCharacters(in: .whitespacesAndNewlines)
            ?? snapshot?.sessionSummary(for: key)?.cwd.trimmingCharacters(in: .whitespacesAndNewlines)

        guard let existingCwd, !existingCwd.isEmpty else {
            return true
        }
        return existingCwd != normalizedOverride
    }

    func restartLocalServer() async throws {
        let currentLocal = snapshot?.servers.first(where: \.isLocal)
        let serverId = currentLocal?.serverId ?? "local"
        let displayName = currentLocal?.displayName ?? "This Device"
        serverBridge.disconnectServer(serverId: serverId)
        _ = try await serverBridge.connectLocalServer(
            serverId: serverId,
            displayName: displayName,
            host: "127.0.0.1",
            port: 0
        )
        await restoreStoredLocalChatGPTAuth(serverId: serverId)
        await refreshSnapshot()
    }

    func restoreStoredLocalChatGPTAuth(serverId: String) async {
        guard let tokens = (try? ChatGPTOAuthTokenStore.shared.load()) ?? nil else {
            return
        }

        do {
            _ = try await client.loginAccount(
                serverId: serverId,
                params: .chatgptAuthTokens(
                    accessToken: tokens.accessToken,
                    chatgptAccountId: tokens.accountID,
                    chatgptPlanType: tokens.planType
                )
            )
        } catch {
            lastError = error.localizedDescription
        }
    }

    func applySnapshot(_ snapshot: AppSnapshotRecord?) {
        let mergedSnapshot = snapshot.map(mergingCachedThreadSnapshots)
        self.snapshot = mergedSnapshot
        if let mergedSnapshot {
            mergedSnapshot.threads.forEach(cacheThreadSnapshot)
            lastError = nil
        }
    }

    private func handleStoreUpdate(_ update: AppStoreUpdateRecord) async {
        switch update {
        case .threadUpserted(let thread, let sessionSummary, let agentDirectoryVersion):
            applyThreadUpsert(
                thread,
                sessionSummary: sessionSummary,
                agentDirectoryVersion: agentDirectoryVersion
            )
        case .threadStateUpdated(let state, let sessionSummary, let agentDirectoryVersion):
            applyThreadStateUpdated(
                state,
                sessionSummary: sessionSummary,
                agentDirectoryVersion: agentDirectoryVersion
            )
        case .threadItemUpserted(let key, _):
            scheduleThreadSnapshotRefresh(for: key)
        case .threadCommandExecutionUpdated(
            let key,
            _,
            _,
            _,
            _,
            _
        ):
            scheduleThreadSnapshotRefresh(for: key)
        case .threadStreamingDelta(let key, let itemId, let kind, let text):
            enqueueThreadStreamingDelta(key: key, itemId: itemId, kind: kind, text: text)
        case .threadRemoved(let key, let agentDirectoryVersion):
            removeThreadSnapshot(for: key, agentDirectoryVersion: agentDirectoryVersion)
        case .activeThreadChanged(let key):
            updateActiveThread(key)
            if let key, threadSnapshot(for: key) == nil {
                await refreshThreadSnapshot(key: key)
            }
            scheduleDeferredActiveThreadHydrationIfNeeded(for: key)
        case .pendingApprovalsChanged:
            await refreshSnapshot()
        case .pendingUserInputsChanged:
            await refreshSnapshot()
        case .serverChanged:
            scheduleSnapshotRefreshDebounced()
        case .serverRemoved:
            await refreshSnapshot()
        case .fullResync:
            await refreshSnapshot()
        case .voiceSessionChanged:
            await refreshSnapshot()
        case .realtimeTranscriptUpdated:
            break
        case .realtimeHandoffRequested:
            break
        case .realtimeSpeechStarted:
            break
        case .realtimeStarted:
            await refreshSnapshot()
        case .realtimeOutputAudioDelta:
            break
        case .realtimeError:
            await refreshSnapshot()
        case .realtimeClosed:
            await refreshSnapshot()
        }
    }

    private func refreshThreadSnapshot(key: ThreadKey) async {
        guard snapshot != nil else {
            await refreshSnapshot()
            return
        }

        do {
            guard let threadSnapshot = try await store.threadSnapshot(key: key) else {
                if cachedThreadSnapshots[key] == nil {
                    removeThreadSnapshot(for: key, clearCache: false)
                }
                return
            }
            applyThreadSnapshot(threadSnapshot)
        } catch {
            lastError = error.localizedDescription
            await refreshSnapshot()
        }
    }

    private func scheduleThreadSnapshotRefresh(for key: ThreadKey) {
        pendingThreadRefreshKeys.insert(key)
        guard pendingThreadRefreshTask == nil else { return }
        pendingThreadRefreshTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 33_000_000)
            guard let self else { return }
            let keys = self.pendingThreadRefreshKeys
            self.pendingThreadRefreshKeys.removeAll()
            self.pendingThreadRefreshTask = nil
            for key in keys {
                await self.refreshThreadSnapshot(key: key)
            }
        }
    }

    private func enqueueThreadStreamingDelta(
        key: ThreadKey,
        itemId: String,
        kind: ThreadStreamingDeltaKind,
        text: String
    ) {
        guard !text.isEmpty else { return }

        if let lastIndex = pendingStreamingDeltaEvents.indices.last,
           pendingStreamingDeltaEvents[lastIndex].key == key,
           pendingStreamingDeltaEvents[lastIndex].itemId == itemId,
           pendingStreamingDeltaEvents[lastIndex].kind == kind {
            pendingStreamingDeltaEvents[lastIndex].text += text
        } else {
            pendingStreamingDeltaEvents.append(
                PendingStreamingDeltaEvent(
                    key: key,
                    itemId: itemId,
                    kind: kind,
                    text: text
                )
            )
        }

        guard pendingStreamingDeltaTask == nil else { return }
        pendingStreamingDeltaTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 33_000_000)
            guard let self else { return }
            await self.flushPendingStreamingDeltas()
        }
    }

    private func flushPendingStreamingDeltas() async {
        let events = pendingStreamingDeltaEvents
        pendingStreamingDeltaEvents.removeAll()
        pendingStreamingDeltaTask = nil
        guard !events.isEmpty else { return }

        let refreshKeys = applyStreamingDeltaBatch(events)
        for key in refreshKeys {
            await refreshThreadSnapshot(key: key)
        }
    }

    private func applyStreamingDeltaBatch(
        _ events: [PendingStreamingDeltaEvent]
    ) -> Set<ThreadKey> {
        guard var snapshot else {
            return Set(events.map(\.key))
        }

        var mutated = false
        var refreshKeys: Set<ThreadKey> = []

        for event in events {
            guard let threadIndex = snapshot.threads.firstIndex(where: { $0.key == event.key }) else {
                refreshKeys.insert(event.key)
                continue
            }
            guard let itemIndex = snapshot.threads[threadIndex].hydratedConversationItems.firstIndex(where: { $0.id == event.itemId }) else {
                refreshKeys.insert(event.key)
                continue
            }

            var item = snapshot.threads[threadIndex].hydratedConversationItems[itemIndex]
            guard let updatedContent = Self.applyingStreamingDelta(
                kind: event.kind,
                text: event.text,
                to: item.content
            ) else {
                refreshKeys.insert(event.key)
                continue
            }

            item.content = updatedContent
            snapshot.threads[threadIndex].hydratedConversationItems[itemIndex] = item
            mutated = true
        }

        if mutated {
            self.snapshot = snapshot
            lastError = nil
        }

        return refreshKeys
    }

    private func scheduleDeferredActiveThreadHydrationIfNeeded(for key: ThreadKey?) {
        guard let key else {
            pendingActiveThreadHydrationTask?.cancel()
            pendingActiveThreadHydrationTask = nil
            pendingActiveThreadHydrationKey = nil
            return
        }

        guard let thread = threadSnapshot(for: key),
              shouldAttemptDeferredHydration(for: thread) else {
            if pendingActiveThreadHydrationKey == key {
                pendingActiveThreadHydrationTask?.cancel()
                pendingActiveThreadHydrationTask = nil
                pendingActiveThreadHydrationKey = nil
            }
            return
        }

        guard pendingActiveThreadHydrationKey != key || pendingActiveThreadHydrationTask == nil else {
            return
        }

        pendingActiveThreadHydrationTask?.cancel()
        pendingActiveThreadHydrationKey = key
        pendingActiveThreadHydrationTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 300_000_000)
            guard let self else { return }
            await self.hydrateActiveThreadIfNeeded(key: key)
        }
    }

    private func hydrateActiveThreadIfNeeded(key: ThreadKey) async {
        defer {
            if pendingActiveThreadHydrationKey == key {
                pendingActiveThreadHydrationTask = nil
                pendingActiveThreadHydrationKey = nil
            }
        }

        guard snapshot?.activeThread == key,
              let thread = threadSnapshot(for: key),
              shouldAttemptDeferredHydration(for: thread) else {
            return
        }

        do {
            let nextKey = try await client.readThread(
                serverId: key.serverId,
                params: AppReadThreadRequest(
                    threadId: key.threadId,
                    includeTurns: true
                )
            )
            if let threadSnapshot = try await store.threadSnapshot(key: nextKey) {
                applyThreadSnapshot(threadSnapshot)
            } else {
                await refreshThreadSnapshot(key: nextKey)
            }
        } catch {
            lastError = error.localizedDescription
        }
    }

    private func shouldAttemptDeferredHydration(for thread: AppThreadSnapshot) -> Bool {
        guard thread.hydratedConversationItems.isEmpty else { return false }
        let preview = thread.info.preview?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let title = thread.info.title?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return !preview.isEmpty || !title.isEmpty || thread.hasActiveTurn
    }

    private func applyThreadSnapshot(_ thread: AppThreadSnapshot) {
        let thread = mergedThreadSnapshotPreservingHydratedItems(thread)
        guard var snapshot else {
            cacheThreadSnapshot(thread)
            applySnapshot(nil)
            return
        }

        if let index = snapshot.threads.firstIndex(where: { $0.key == thread.key }) {
            snapshot.threads[index] = thread
        } else {
            snapshot.threads.append(thread)
        }
        self.snapshot = snapshot
        cacheThreadSnapshot(thread)
        lastError = nil
    }

    private func applyThreadUpsert(
        _ thread: AppThreadSnapshot,
        sessionSummary: AppSessionSummary,
        agentDirectoryVersion: UInt64
    ) {
        let thread = mergedThreadSnapshotPreservingHydratedItems(thread)
        guard var snapshot else { return }

        if let index = snapshot.threads.firstIndex(where: { $0.key == thread.key }) {
            snapshot.threads[index] = thread
        } else {
            snapshot.threads.append(thread)
        }

        if let index = snapshot.sessionSummaries.firstIndex(where: { $0.key == sessionSummary.key }) {
            snapshot.sessionSummaries[index] = sessionSummary
        } else {
            snapshot.sessionSummaries.append(sessionSummary)
        }
        snapshot.sessionSummaries.sort(by: Self.sessionSummarySort(lhs:rhs:))
        snapshot.agentDirectoryVersion = agentDirectoryVersion
        self.snapshot = snapshot
        cacheThreadSnapshot(thread)
        lastError = nil
    }

    private func applyThreadStateUpdated(
        _ state: AppThreadStateRecord,
        sessionSummary: AppSessionSummary,
        agentDirectoryVersion: UInt64
    ) {
        guard var snapshot else { return }
        guard let threadIndex = snapshot.threads.firstIndex(where: { $0.key == state.key }) else {
            return
        }

        var thread = snapshot.threads[threadIndex]
        thread.info = state.info
        thread.collaborationMode = state.collaborationMode
        thread.model = state.model
        thread.reasoningEffort = state.reasoningEffort
        thread.effectiveApprovalPolicy = state.effectiveApprovalPolicy
        thread.effectiveSandboxPolicy = state.effectiveSandboxPolicy
        thread.activeTurnId = state.activeTurnId
        thread.activePlanProgress = state.activePlanProgress
        thread.pendingPlanImplementationPrompt = state.pendingPlanImplementationPrompt
        thread.contextTokensUsed = state.contextTokensUsed
        thread.modelContextWindow = state.modelContextWindow
        thread.rateLimits = state.rateLimits
        thread.realtimeSessionId = state.realtimeSessionId
        snapshot.threads[threadIndex] = thread

        if let index = snapshot.sessionSummaries.firstIndex(where: { $0.key == sessionSummary.key }) {
            snapshot.sessionSummaries[index] = sessionSummary
        } else {
            snapshot.sessionSummaries.append(sessionSummary)
        }
        snapshot.sessionSummaries.sort(by: Self.sessionSummarySort(lhs:rhs:))
        snapshot.agentDirectoryVersion = agentDirectoryVersion
        self.snapshot = snapshot
        cacheThreadSnapshot(thread)
        lastError = nil
    }

    private func applyThreadItemUpsert(
        key: ThreadKey,
        item: HydratedConversationItem
    ) -> Bool {
        guard var snapshot else { return false }
        guard let threadIndex = snapshot.threads.firstIndex(where: { $0.key == key }) else {
            return false
        }

        var thread = snapshot.threads[threadIndex]
        if let itemIndex = thread.hydratedConversationItems.firstIndex(where: { $0.id == item.id }) {
            thread.hydratedConversationItems[itemIndex] = item
        } else {
            let insertionIndex = Self.insertionIndex(for: item, in: thread.hydratedConversationItems)
            thread.hydratedConversationItems.insert(item, at: insertionIndex)
        }
        snapshot.threads[threadIndex] = thread
        self.snapshot = snapshot
        cacheThreadSnapshot(thread)
        lastError = nil
        return true
    }

    private func applyThreadCommandExecutionUpdated(
        key: ThreadKey,
        itemId: String,
        status: AppOperationStatus,
        exitCode: Int32?,
        durationMs: Int64?,
        processId: String?
    ) -> Bool {
        guard var snapshot else { return false }
        guard let threadIndex = snapshot.threads.firstIndex(where: { $0.key == key }) else {
            return false
        }
        guard let itemIndex = snapshot.threads[threadIndex].hydratedConversationItems.firstIndex(where: { $0.id == itemId }) else {
            return false
        }

        var item = snapshot.threads[threadIndex].hydratedConversationItems[itemIndex]
        guard case .commandExecution(var data) = item.content else {
            return false
        }
        data.status = status
        data.exitCode = exitCode
        data.durationMs = durationMs
        data.processId = processId
        item.content = .commandExecution(data)
        snapshot.threads[threadIndex].hydratedConversationItems[itemIndex] = item
        self.snapshot = snapshot
        cacheThreadSnapshot(snapshot.threads[threadIndex])
        lastError = nil
        return true
    }

    private func applyThreadStreamingDelta(
        key: ThreadKey,
        itemId: String,
        kind: ThreadStreamingDeltaKind,
        text: String
    ) -> Bool {
        guard var snapshot else { return false }
        guard let threadIndex = snapshot.threads.firstIndex(where: { $0.key == key }) else {
            return false
        }
        guard let itemIndex = snapshot.threads[threadIndex].hydratedConversationItems.firstIndex(where: { $0.id == itemId }) else {
            return false
        }

        var item = snapshot.threads[threadIndex].hydratedConversationItems[itemIndex]
        guard let updatedContent = Self.applyingStreamingDelta(kind: kind, text: text, to: item.content) else {
            return false
        }
        item.content = updatedContent
        snapshot.threads[threadIndex].hydratedConversationItems[itemIndex] = item
        self.snapshot = snapshot
        cacheThreadSnapshot(snapshot.threads[threadIndex])
        lastError = nil
        return true
    }

    private func removeThreadSnapshot(
        for key: ThreadKey,
        agentDirectoryVersion: UInt64? = nil,
        clearCache: Bool = true
    ) {
        guard var snapshot else { return }
        snapshot.threads.removeAll { $0.key == key }
        snapshot.sessionSummaries.removeAll { $0.key == key }
        if snapshot.activeThread == key {
            snapshot.activeThread = nil
        }
        if let agentDirectoryVersion {
            snapshot.agentDirectoryVersion = agentDirectoryVersion
        }
        self.snapshot = snapshot
        if clearCache {
            cachedThreadSnapshots.removeValue(forKey: key)
        }
    }

    private func updateActiveThread(_ key: ThreadKey?) {
        guard var snapshot else { return }
        snapshot.activeThread = key
        self.snapshot = snapshot
    }

    private static func applyingStreamingDelta(
        kind: ThreadStreamingDeltaKind,
        text: String,
        to content: HydratedConversationItemContent
    ) -> HydratedConversationItemContent? {
        switch (kind, content) {
        case (.assistantText, .assistant(var data)):
            data.text += text
            return .assistant(data)
        case (.reasoningText, .reasoning(var data)):
            if data.content.isEmpty {
                data.content.append(text)
            } else {
                data.content[data.content.count - 1] += text
            }
            return .reasoning(data)
        case (.planText, .proposedPlan(var data)):
            data.content += text
            return .proposedPlan(data)
        case (.commandOutput, .commandExecution(var data)):
            data.output = (data.output ?? "") + text
            return .commandExecution(data)
        case (.mcpProgress, .mcpToolCall(var data)):
            if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                data.progressMessages.append(text)
            }
            return .mcpToolCall(data)
        default:
            return nil
        }
    }

    private static func sessionSummarySort(lhs: AppSessionSummary, rhs: AppSessionSummary) -> Bool {
        let lhsUpdatedAt = lhs.updatedAt ?? Int64.min
        let rhsUpdatedAt = rhs.updatedAt ?? Int64.min
        if lhsUpdatedAt != rhsUpdatedAt {
            return lhsUpdatedAt > rhsUpdatedAt
        }
        if lhs.key.serverId != rhs.key.serverId {
            return lhs.key.serverId < rhs.key.serverId
        }
        return lhs.key.threadId < rhs.key.threadId
    }

    private static func insertionIndex(
        for item: HydratedConversationItem,
        in items: [HydratedConversationItem]
    ) -> Int {
        guard let targetTurnIndex = item.sourceTurnIndex.map(Int.init) else {
            return items.count
        }
        if let lastSameTurnIndex = items.lastIndex(where: { $0.sourceTurnIndex.map(Int.init) == targetTurnIndex }) {
            return lastSameTurnIndex + 1
        }
        if let nextTurnIndex = items.firstIndex(where: {
            guard let sourceTurnIndex = $0.sourceTurnIndex.map(Int.init) else { return false }
            return sourceTurnIndex > targetTurnIndex
        }) {
            return nextTurnIndex
        }
        return items.count
    }

    func queueComposerPrefill(threadKey: ThreadKey, text: String) {
        composerPrefillRequest = ComposerPrefillRequest(threadKey: threadKey, text: text)
    }

    func clearComposerPrefill(id: UUID) {
        guard composerPrefillRequest?.id == id else { return }
        composerPrefillRequest = nil
    }

    func availableModels(for serverId: String) -> [ModelInfo] {
        snapshot?.serverSnapshot(for: serverId)?.availableModels ?? []
    }

    func rateLimits(for serverId: String) -> RateLimitSnapshot? {
        snapshot?.serverSnapshot(for: serverId)?.rateLimits
    }

    func loadConversationMetadataIfNeeded(serverId: String) async {
        if hasFreshConversationMetadata(for: serverId) {
            return
        }
        await loadAvailableModelsIfNeeded(serverId: serverId)
        await loadRateLimitsIfNeeded(serverId: serverId)
        recentConversationMetadataLoads[serverId] = Date()
    }

    func loadAvailableModelsIfNeeded(serverId: String) async {
        guard let server = snapshot?.serverSnapshot(for: serverId), server.isConnected else { return }
        guard server.availableModels == nil else { return }
        guard !loadingModelServerIds.contains(serverId) else { return }
        loadingModelServerIds.insert(serverId)
        defer { loadingModelServerIds.remove(serverId) }
        do {
            _ = try await client.refreshModels(
                serverId: serverId,
                params: AppRefreshModelsRequest(cursor: nil, limit: nil, includeHidden: false)
            )
            await refreshSnapshot()
        } catch {
            lastError = error.localizedDescription
        }
    }

    func loadRateLimitsIfNeeded(serverId: String) async {
        guard let server = snapshot?.serverSnapshot(for: serverId), server.isConnected else { return }
        guard server.rateLimits == nil else { return }
        guard server.account != nil else { return }
        guard !loadingRateLimitServerIds.contains(serverId) else { return }
        loadingRateLimitServerIds.insert(serverId)
        defer { loadingRateLimitServerIds.remove(serverId) }
        do {
            _ = try await client.refreshRateLimits(serverId: serverId)
        } catch {
            lastError = error.localizedDescription
        }
    }

    func startTurn(key: ThreadKey, payload: AppComposerPayload) async throws {
        do {
            try await store.startTurn(
                key: key,
                params: payload.turnStartRequest(threadId: key.threadId)
            )
        } catch {
            lastError = error.localizedDescription
            throw error
        }
    }

    func hydrateThreadPermissions(for key: ThreadKey, appState: AppState) async -> ThreadKey? {
        if let existing = threadSnapshot(for: key) {
            appState.hydratePermissions(from: existing)
            if !hasAuthoritativePermissions(existing) {
                scheduleBackgroundThreadPermissionHydration(for: key, appState: appState)
            }
            return key
        }

        if snapshot?.sessionSummary(for: key) != nil {
            scheduleBackgroundThreadPermissionHydration(for: key, appState: appState)
            return key
        }

        do {
            let nextKey = try await client.readThread(
                serverId: key.serverId,
                params: AppReadThreadRequest(
                    threadId: key.threadId,
                    includeTurns: false
                )
            )
            if let threadSnapshot = try await store.threadSnapshot(key: nextKey) {
                applyThreadSnapshot(threadSnapshot)
                appState.hydratePermissions(from: threadSnapshot)
            } else {
                await refreshSnapshot()
                appState.hydratePermissions(from: snapshot?.threadSnapshot(for: nextKey))
            }
            return nextKey
        } catch {
            lastError = error.localizedDescription
            return nil
        }
    }

    private func scheduleBackgroundThreadPermissionHydration(
        for key: ThreadKey,
        appState: AppState
    ) {
        Task { [weak self] in
            guard let self else { return }
            do {
                let nextKey = try await client.readThread(
                    serverId: key.serverId,
                    params: AppReadThreadRequest(
                        threadId: key.threadId,
                        includeTurns: false
                    )
                )
                if let threadSnapshot = try await store.threadSnapshot(key: nextKey) {
                    applyThreadSnapshot(threadSnapshot)
                    appState.hydratePermissions(from: threadSnapshot)
                } else {
                    await refreshSnapshot()
                    appState.hydratePermissions(from: snapshot?.threadSnapshot(for: nextKey))
                }
            } catch {
                lastError = error.localizedDescription
            }
        }
    }

    func ensureThreadLoaded(
        key: ThreadKey,
        maxAttempts: Int = 5
    ) async -> ThreadKey? {
        if threadSnapshot(for: key) != nil {
            return key
        }

        var currentKey = key
        for attempt in 0..<maxAttempts {
            var readSucceeded = false
            do {
                let nextKey = try await client.readThread(
                    serverId: currentKey.serverId,
                    params: AppReadThreadRequest(
                        threadId: currentKey.threadId,
                        includeTurns: true
                    )
                )
                currentKey = nextKey
                store.setActiveThread(key: currentKey)
                readSucceeded = true
            } catch {
                lastError = error.localizedDescription
            }

            await refreshSnapshot()
            if threadSnapshot(for: currentKey) != nil {
                return currentKey
            }

            if !readSucceeded {
                do {
                    _ = try await client.listThreads(
                        serverId: currentKey.serverId,
                        params: AppListThreadsRequest(
                            cursor: nil,
                            limit: nil,
                            archived: nil,
                            cwd: nil,
                            searchTerm: nil
                        )
                    )
                } catch {
                    lastError = error.localizedDescription
                }

                await refreshSnapshot()
                if threadSnapshot(for: currentKey) != nil {
                    return currentKey
                }
            }

            if attempt + 1 < maxAttempts {
                try? await Task.sleep(nanoseconds: 250_000_000)
            }
        }

        if let activeKey = snapshot?.activeThread,
           activeKey.serverId == currentKey.serverId,
           threadSnapshot(for: activeKey) != nil {
            return activeKey
        }

        return nil
    }

    func threadSnapshot(for key: ThreadKey) -> AppThreadSnapshot? {
        snapshot?.threadSnapshot(for: key) ?? cachedThreadSnapshots[key]
    }

    private func hasAuthoritativePermissions(_ thread: AppThreadSnapshot) -> Bool {
        threadPermissionsAreAuthoritative(
            approvalPolicy: thread.effectiveApprovalPolicy,
            sandboxPolicy: thread.effectiveSandboxPolicy
        )
    }

    private func hasFreshConversationMetadata(for serverId: String) -> Bool {
        guard let server = snapshot?.serverSnapshot(for: serverId) else { return false }
        let hasModels = server.availableModels != nil
        let hasRateLimits = server.account == nil || server.rateLimits != nil
        if hasModels && hasRateLimits {
            return true
        }

        guard let lastLoad = recentConversationMetadataLoads[serverId] else { return false }
        return Date().timeIntervalSince(lastLoad) < 10
    }

    private func restoreCachedThreadSnapshotIfNeeded(for key: ThreadKey?) {
        guard let key,
              snapshot?.threadSnapshot(for: key) == nil,
              let cached = cachedThreadSnapshots[key] else {
            return
        }
        applyThreadSnapshot(cached)
    }

    private func cacheThreadSnapshot(_ thread: AppThreadSnapshot) {
        cachedThreadSnapshots[thread.key] = thread
    }

    private func mergedThreadSnapshotPreservingHydratedItems(_ thread: AppThreadSnapshot) -> AppThreadSnapshot {
        guard thread.hydratedConversationItems.isEmpty,
              let cached = cachedThreadSnapshots[thread.key],
              !cached.hydratedConversationItems.isEmpty else {
            return thread
        }

        var merged = thread
        merged.hydratedConversationItems = cached.hydratedConversationItems
        return merged
    }

    private func mergingCachedThreadSnapshots(_ snapshot: AppSnapshotRecord) -> AppSnapshotRecord {
        var snapshot = snapshot

        for index in snapshot.threads.indices {
            let thread = snapshot.threads[index]
            snapshot.threads[index] = mergedThreadSnapshotPreservingHydratedItems(thread)
        }

        for (key, cached) in cachedThreadSnapshots {
            guard snapshot.threads.contains(where: { $0.key == key }) == false else { continue }
            guard snapshot.activeThread == key ||
                  snapshot.sessionSummaries.contains(where: { $0.key == key }) else {
                continue
            }
            snapshot.threads.append(cached)
        }

        return snapshot
    }
}

extension AppSnapshotRecord {
    func threadSnapshot(for key: ThreadKey) -> AppThreadSnapshot? {
        threads.first { $0.key == key }
    }

    func serverSnapshot(for serverId: String) -> AppServerSnapshot? {
        servers.first { $0.serverId == serverId }
    }

    func sessionSummary(for key: ThreadKey) -> AppSessionSummary? {
        sessionSummaries.first { $0.key == key }
    }

    func resolvedThreadKey(for receiverId: String, serverId: String) -> ThreadKey? {
        guard let normalized = AgentLabelFormatter.sanitized(receiverId) else { return nil }
        if let summary = sessionSummaries.first(where: {
            $0.key.serverId == serverId && $0.key.threadId == normalized
        }) {
            return summary.key
        }
        return ThreadKey(serverId: serverId, threadId: normalized)
    }

    func resolvedAgentTargetLabel(for target: String, serverId: String) -> String? {
        if AgentLabelFormatter.looksLikeDisplayLabel(target) {
            return AgentLabelFormatter.sanitized(target)
        }
        guard let normalized = AgentLabelFormatter.sanitized(target) else { return nil }
        if let summary = sessionSummaries.first(where: {
            $0.key.serverId == serverId && $0.key.threadId == normalized
        }) {
            return summary.agentDisplayLabel ?? AgentLabelFormatter.sanitized(target)
        }
        return nil
    }
}
