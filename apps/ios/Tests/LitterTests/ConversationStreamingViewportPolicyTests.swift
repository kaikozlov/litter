import XCTest
@testable import Litter

final class ConversationStreamingViewportPolicyTests: XCTestCase {
    func testShouldMaintainBottomAnchorWhileStreamingAndAutoFollowing() {
        XCTAssertTrue(
            ConversationStreamingViewportPolicy.shouldMaintainBottomAnchor(
                isStreaming: true,
                isNearBottom: true,
                autoFollowStreaming: true,
                userIsDraggingScroll: false
            )
        )
    }

    func testShouldNotMaintainBottomAnchorWhileUserIsDragging() {
        XCTAssertFalse(
            ConversationStreamingViewportPolicy.shouldMaintainBottomAnchor(
                isStreaming: true,
                isNearBottom: true,
                autoFollowStreaming: true,
                userIsDraggingScroll: true
            )
        )
    }

    func testRetainsActiveAndLatestCompletedLiveDetailItems() {
        let active = ConversationItem(
            id: "active",
            content: .commandExecution(
                ConversationCommandExecutionData(
                    command: "tail -f log",
                    cwd: "/tmp",
                    status: .inProgress,
                    output: nil,
                    exitCode: nil,
                    durationMs: nil,
                    processId: nil,
                    actions: []
                )
            )
        )
        let latestCompleted = ConversationItem(
            id: "latest-completed",
            content: .mcpToolCall(
                ConversationMcpToolCallData(
                    server: "search",
                    tool: "find",
                    status: .completed,
                    durationMs: 120,
                    argumentsJSON: nil,
                    contentSummary: nil,
                    structuredContentJSON: nil,
                    rawOutputJSON: nil,
                    errorMessage: nil,
                    progressMessages: []
                )
            )
        )
        let olderCompleted = ConversationItem(
            id: "older-completed",
            content: .webSearch(
                ConversationWebSearchData(
                    query: "lag trace",
                    actionJSON: nil,
                    isInProgress: false
                )
            )
        )

        let retained = ConversationLiveDetailRetentionPolicy.retainedRichDetailItemIDs(
            for: [olderCompleted, latestCompleted, active]
        )

        XCTAssertEqual(retained, ["active", "latest-completed"])
    }

    func testRetainsOnlyLatestCompletedWhenNoActiveLiveDetailExists() {
        let olderCompleted = ConversationItem(
            id: "older-completed",
            content: .fileChange(
                ConversationFileChangeData(
                    status: .completed,
                    changes: [],
                    outputDelta: nil
                )
            )
        )
        let latestCompleted = ConversationItem(
            id: "latest-completed",
            content: .dynamicToolCall(
                ConversationDynamicToolCallData(
                    tool: "read",
                    status: .completed,
                    durationMs: 30,
                    success: true,
                    argumentsJSON: nil,
                    contentSummary: nil
                )
            )
        )

        let retained = ConversationLiveDetailRetentionPolicy.retainedRichDetailItemIDs(
            for: [olderCompleted, latestCompleted]
        )

        XCTAssertEqual(retained, ["latest-completed"])
    }
}
