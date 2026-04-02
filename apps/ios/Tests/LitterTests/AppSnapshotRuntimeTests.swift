import XCTest
@testable import Litter

final class AppSnapshotRuntimeTests: XCTestCase {
    func testThreadHasTrackedTurnWhenThreadHasActiveTurn() {
        let key = ThreadKey(serverId: "srv", threadId: "thread-1")
        let snapshot = makeSnapshot(
            threads: [makeThreadSnapshot(key: key, status: .active, activeTurnId: "turn-1")]
        )

        XCTAssertTrue(snapshot.threadHasTrackedTurn(for: key))
        XCTAssertEqual(snapshot.threadsWithTrackedTurns.map(\.key), [key])
    }

    func testThreadHasTrackedTurnWhenApprovalIsPending() {
        let key = ThreadKey(serverId: "srv", threadId: "thread-1")
        var snapshot = makeSnapshot(
            threads: [makeThreadSnapshot(key: key)]
        )
        snapshot.pendingApprovals = [
            PendingApproval(
                id: "approval-1",
                serverId: key.serverId,
                kind: .command,
                threadId: key.threadId,
                turnId: "turn-1",
                itemId: "item-1",
                command: "ls",
                path: nil,
                grantRoot: nil,
                cwd: "/tmp",
                reason: nil
            )
        ]

        XCTAssertTrue(snapshot.threadHasTrackedTurn(for: key))
        XCTAssertEqual(snapshot.threadsWithTrackedTurns.map(\.key), [key])
    }

    func testThreadHasTrackedTurnWhenUserInputIsPending() {
        let key = ThreadKey(serverId: "srv", threadId: "thread-1")
        var snapshot = makeSnapshot(
            threads: [makeThreadSnapshot(key: key)]
        )
        snapshot.pendingUserInputs = [
            PendingUserInputRequest(
                id: "input-1",
                serverId: key.serverId,
                threadId: key.threadId,
                turnId: "turn-1",
                itemId: "item-1",
                questions: [],
                requesterAgentNickname: nil,
                requesterAgentRole: nil
            )
        ]

        XCTAssertTrue(snapshot.threadHasTrackedTurn(for: key))
        XCTAssertEqual(snapshot.threadsWithTrackedTurns.map(\.key), [key])
    }

    func testThreadHasTrackedTurnIgnoresOtherThreadsPendingState() {
        let trackedKey = ThreadKey(serverId: "srv", threadId: "thread-1")
        let otherKey = ThreadKey(serverId: "srv", threadId: "thread-2")
        var snapshot = makeSnapshot(
            threads: [
                makeThreadSnapshot(key: trackedKey),
                makeThreadSnapshot(key: otherKey)
            ]
        )
        snapshot.pendingApprovals = [
            PendingApproval(
                id: "approval-1",
                serverId: otherKey.serverId,
                kind: .command,
                threadId: otherKey.threadId,
                turnId: "turn-2",
                itemId: "item-2",
                command: "pwd",
                path: nil,
                grantRoot: nil,
                cwd: "/tmp",
                reason: nil
            )
        ]

        XCTAssertFalse(snapshot.threadHasTrackedTurn(for: trackedKey))
        XCTAssertTrue(snapshot.threadHasTrackedTurn(for: otherKey))
        XCTAssertEqual(snapshot.threadsWithTrackedTurns.map(\.key), [otherKey])
    }

    private func makeSnapshot(threads: [AppThreadSnapshot]) -> AppSnapshotRecord {
        let server = AppServerSnapshot(
            serverId: "srv",
            displayName: "Server",
            host: "srv.local",
            port: 8390,
            wakeMac: nil,
            isLocal: false,
            supportsIpc: false,
            hasIpc: false,
            health: .connected,
            transportState: .connected,
            ipcState: .unsupported,
            capabilities: AppServerCapabilities(
                canUseTransportActions: true,
                canBrowseDirectories: true,
                canStartThreads: true,
                canResumeThreads: true,
                canUseIpc: false,
                canResumeViaIpc: false
            ),
            account: nil,
            requiresOpenaiAuth: false,
            rateLimits: nil,
            availableModels: nil,
            connectionProgress: nil
        )
        let sessionSummaries = threads.map { thread in
            AppSessionSummary(
                key: thread.key,
                serverDisplayName: server.displayName,
                serverHost: server.host,
                title: thread.info.title ?? "",
                preview: thread.info.preview ?? "",
                cwd: thread.info.cwd ?? "",
                model: thread.model ?? "",
                modelProvider: thread.info.modelProvider ?? "",
                parentThreadId: thread.info.parentThreadId,
                agentNickname: thread.info.agentNickname,
                agentRole: thread.info.agentRole,
                agentDisplayLabel: thread.key.threadId,
                agentStatus: .unknown,
                updatedAt: thread.info.updatedAt,
                hasActiveTurn: thread.hasActiveTurn,
                isSubagent: false,
                isFork: false
            )
        }

        return AppSnapshotRecord(
            servers: [server],
            threads: threads,
            sessionSummaries: sessionSummaries,
            agentDirectoryVersion: 0,
            activeThread: nil,
            pendingApprovals: [],
            pendingUserInputs: [],
            voiceSession: AppVoiceSessionSnapshot(
                activeThread: nil,
                sessionId: nil,
                phase: nil,
                lastError: nil,
                transcriptEntries: [],
                handoffThreadKey: nil
            )
        )
    }

    private func makeThreadSnapshot(
        key: ThreadKey,
        status: ThreadSummaryStatus = .idle,
        activeTurnId: String? = nil
    ) -> AppThreadSnapshot {
        AppThreadSnapshot(
            key: key,
            info: ThreadInfo(
                id: key.threadId,
                title: "Thread",
                model: nil,
                status: status,
                preview: "Preview",
                cwd: "/tmp",
                path: nil,
                modelProvider: nil,
                agentNickname: nil,
                agentRole: nil,
                parentThreadId: nil,
                agentStatus: nil,
                createdAt: nil,
                updatedAt: nil
            ),
            collaborationMode: .default,
            model: nil,
            reasoningEffort: nil,
            effectiveApprovalPolicy: nil,
            effectiveSandboxPolicy: nil,
            hydratedConversationItems: [],
            queuedFollowUps: [],
            activeTurnId: activeTurnId,
            activePlanProgress: nil,
            pendingPlanImplementationPrompt: nil,
            contextTokensUsed: nil,
            modelContextWindow: nil,
            rateLimits: nil,
            realtimeSessionId: nil
        )
    }
}
