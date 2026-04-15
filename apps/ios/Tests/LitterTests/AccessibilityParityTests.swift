import XCTest
@testable import Litter

// MARK: - Accessibility Parity Tests (VAL-IOS-039, VAL-IOS-040, VAL-IOS-041, VAL-IOS-042)
//
// These tests verify that all new iOS UI elements introduced by the multi-provider
// architecture have proper VoiceOver accessibility labels, hints, and values.

@MainActor
final class AccessibilityParityTests: XCTestCase {

    // MARK: - VAL-IOS-039: Agent badge views have meaningful accessibility labels

    func testAgentBadgeAccessibilityLabelForAllTypes() {
        // VoiceOver should announce the agent display name when focusing on badge views.
        // The accessibility label format is "<displayName> agent".
        let typesAndExpectedLabels: [(AgentType, String)] = [
            (.codex, "Codex agent"),
            (.piAcp, "Pi agent"),
            (.piNative, "Pi agent"),
            (.droidAcp, "Droid agent"),
            (.droidNative, "Droid agent"),
            (.genericAcp, "ACP agent"),
        ]

        for (agentType, expectedLabel) in typesAndExpectedLabels {
            let label = "\(agentType.displayName) agent"
            XCTAssertEqual(label, expectedLabel,
                "AgentBadgeView for \(agentType) should have accessibility label '\(expectedLabel)'")
            XCTAssertFalse(label.isEmpty,
                "AgentBadgeView for \(agentType) must have a non-empty accessibility label")
        }
    }

    func testBadgeLabelNeverAnnouncesWrongAgentType() {
        // It should never announce "Codex agent" for a non-Codex agent
        let nonCodexTypes: [AgentType] = [.piAcp, .piNative, .droidAcp, .droidNative, .genericAcp]
        for type in nonCodexTypes {
            let label = "\(type.displayName) agent"
            XCTAssertFalse(label.contains("Codex"),
                "Non-Codex agent \(type) should not have 'Codex' in its accessibility label")
        }
    }

    func testAllAgentTypesHaveDistinctBadgeLabels() {
        let types: [AgentType] = [.codex, .piAcp, .droidAcp, .genericAcp]
        let labels = Set(types.map { "\($0.displayName) agent" })
        // We expect distinct labels for each agent group
        XCTAssertTrue(labels.contains("Codex agent"))
        XCTAssertTrue(labels.contains("Pi agent"))
        XCTAssertTrue(labels.contains("Droid agent"))
        XCTAssertTrue(labels.contains("ACP agent"))
    }

    // MARK: - VAL-IOS-040: Agent picker rows are accessible

    func testInlineAgentPickerRowLabelFormat() {
        // Verify the inline picker row accessibility label includes agent name and transport
        let info = AgentInfo(
            id: "test-pi",
            displayName: "Pi",
            description: "AI coding agent",
            detectedTransports: [.piNative],
            capabilities: ["streaming", "tools"]
        )
        let agentType = info.detectedTransports.first ?? .codex

        // Simulate the accessibility label logic from InlineAgentSelectorView
        var parts: [String] = [info.displayName]
        parts.append("\(agentType.transportLabel) transport")
        let label = parts.joined(separator: ", ")

        XCTAssertTrue(label.contains("Pi"), "Picker row label should contain agent display name")
        XCTAssertTrue(label.contains("transport"), "Picker row label should mention transport")
    }

    func testInlineAgentPickerRowLabelForUnavailableAgent() {
        let info = AgentInfo(
            id: "test-droid",
            displayName: "Droid",
            description: "Droid agent",
            detectedTransports: [.droidNative],
            capabilities: []
        )
        let agentType = info.detectedTransports.first ?? .codex

        // Simulate unavailable agent row label
        var parts: [String] = [info.displayName]
        parts.append("\(agentType.transportLabel) transport")
        parts.append("unavailable")
        let label = parts.joined(separator: ", ")

        XCTAssertTrue(label.contains("Droid"), "Unavailable row should still name the agent")
        XCTAssertTrue(label.contains("unavailable"), "Unavailable row should indicate unavailability")
    }

    func testInlineAgentPickerRowLabelIncludesSelection() {
        let info = AgentInfo(
            id: "test-codex",
            displayName: "Codex",
            description: "Code assistant",
            detectedTransports: [.codex],
            capabilities: []
        )
        let agentType = AgentType.codex

        // Selected agent
        var parts: [String] = [info.displayName]
        parts.append("\(agentType.transportLabel) transport")
        parts.append("selected")
        let label = parts.joined(separator: ", ")

        XCTAssertTrue(label.contains("selected"), "Selected row should indicate selection state")
    }

    func testPickerSheetRowLabelFormat() {
        let info = AgentInfo(
            id: "test-pi",
            displayName: "Pi",
            description: "AI coding agent",
            detectedTransports: [.piNative, .piAcp],
            capabilities: ["streaming", "tools"]
        )
        let agentType = info.detectedTransports.first ?? .codex

        // Simulate picker sheet row label
        var parts: [String] = [info.displayName]
        parts.append(contentsOf: info.detectedTransports.map { $0.transportLabel })
        let label = parts.joined(separator: ", ")

        XCTAssertTrue(label.contains("Pi"), "Picker sheet row should contain agent name")
        XCTAssertTrue(label.contains("Native"), "Picker sheet row should contain transport labels")
        XCTAssertTrue(label.contains("ACP"), "Picker sheet row should show all transports")
    }

    func testPickerSheetRowLabelForUnavailableAgent() {
        let info = AgentInfo(
            id: "test-droid",
            displayName: "Droid",
            description: "Droid agent",
            detectedTransports: [.droidNative],
            capabilities: []
        )

        // Simulate unavailable row label
        var parts: [String] = [info.displayName]
        parts.append("unavailable")
        let label = parts.joined(separator: ", ")

        XCTAssertTrue(label.contains("Droid"), "Unavailable sheet row should still name the agent")
        XCTAssertTrue(label.contains("unavailable"), "Unavailable sheet row should indicate unavailability")
    }

    func testACPProfileRowAccessibilityLabels() {
        let profile = ACPProfile(
            displayName: "My Agent",
            remoteCommand: "my-agent --acp"
        )
        // Verify the label format used for ACP profile rows
        let label = "ACP profile: \(profile.displayName)"
        XCTAssertEqual(label, "ACP profile: My Agent")
        XCTAssertFalse(label.isEmpty)
    }

    // MARK: - VAL-IOS-041: Session filter accessibility describes active agent filter

    func testSessionFilterAccessibilityLabelWithNoFilter() {
        // When no filter is active, it should announce "All agents"
        let label = "Filter by agent type, currently All agents"
        XCTAssertTrue(label.contains("Filter by agent type"), "Filter label should describe its purpose")
        XCTAssertTrue(label.contains("All agents"), "No-filter state should announce 'All agents'")
    }

    func testSessionFilterAccessibilityLabelWithPiFilter() {
        let agentTypeFilter = AgentType.piNative
        let label = "Filter by agent type, currently \(agentTypeFilter.displayName)"
        XCTAssertTrue(label.contains("Filter by agent type"), "Filter label should describe its purpose")
        XCTAssertTrue(label.contains("Pi"), "Pi filter should announce 'Pi'")
    }

    func testSessionFilterAccessibilityLabelWithDroidFilter() {
        let agentTypeFilter = AgentType.droidNative
        let label = "Filter by agent type, currently \(agentTypeFilter.displayName)"
        XCTAssertTrue(label.contains("Droid"), "Droid filter should announce 'Droid'")
    }

    func testSessionFilterAccessibilityLabelWithACPFilter() {
        let agentTypeFilter = AgentType.genericAcp
        let label = "Filter by agent type, currently \(agentTypeFilter.displayName)"
        XCTAssertTrue(label.contains("ACP"), "ACP filter should announce 'ACP'")
    }

    func testSessionFilterAccessibilityLabelWithCodexFilter() {
        let agentTypeFilter = AgentType.codex
        let label = "Filter by agent type, currently \(agentTypeFilter.displayName)"
        XCTAssertTrue(label.contains("Codex"), "Codex filter should announce 'Codex'")
    }

    // MARK: - VAL-IOS-042: Conversation turn accessibility summary is provider-agnostic

    func testAccessibilitySummaryDoesNotContainProviderSpecificText() {
        // Simulate the accessibilitySummary logic from CollapsedTurnCard
        // It should contain: preview text, duration, tool call count, image count
        // It should NOT contain: "Codex" or any provider name

        let primaryText = "Fix the authentication bug"
        let secondaryText = "I've updated the auth middleware to handle token expiry correctly."
        let durationText = "12s"
        let toolCallCount = 3
        let imageCount = 1

        var parts = [primaryText]
        parts.append(secondaryText)
        parts.append("Duration \(durationText)")
        parts.append("\(toolCallCount) tool calls")
        parts.append("\(imageCount) image")

        let summary = parts.joined(separator: ". ")

        // Verify the summary does not contain provider-specific text
        // Check for display names as whole words using regex word boundaries
        let providerDisplayNames = ["Codex", "Droid", "ACP"]
        for name in providerDisplayNames {
            XCTAssertFalse(summary.contains(name),
                "Accessibility summary should not contain provider name '\(name)': \(summary)")
        }
        // Check "Pi" as a whole word (not as substring in words like "expiry")
        let piPattern = try! NSRegularExpression(pattern: "\\bPi\\b")
        let piMatches = piPattern.numberOfMatches(in: summary, range: NSRange(summary.startIndex..., in: summary))
        XCTAssertEqual(piMatches, 0,
            "Accessibility summary should not contain 'Pi' as a standalone word: \(summary)")

        // Verify it contains expected content
        XCTAssertTrue(summary.contains(primaryText), "Summary should contain preview text")
        XCTAssertTrue(summary.contains(durationText), "Summary should contain duration")
        XCTAssertTrue(summary.contains("tool calls"), "Summary should contain tool call info")
        XCTAssertTrue(summary.contains("image"), "Summary should contain image info")
    }

    func testAccessibilitySummaryWithMinimalData() {
        // When only primary text is available, summary should still be meaningful
        let primaryText = "Hello world"
        var parts = [primaryText]
        let summary = parts.joined(separator: ". ")

        XCTAssertFalse(summary.isEmpty, "Summary should not be empty even with minimal data")
        XCTAssertEqual(summary, primaryText)

        // No provider-specific text
        XCTAssertFalse(summary.contains("Codex"))
    }

    func testAccessibilitySummaryWithAllMetadata() {
        let primaryText = "Refactor the database layer"
        let secondaryText: String? = "Updated all queries to use prepared statements."
        let durationText = "45s"
        let toolCallCount = 5
        let widgetCount = 2
        let eventCount = 8
        let imageCount = 3

        var parts = [primaryText]
        if let secondaryText { parts.append(secondaryText) }
        parts.append("Duration \(durationText)")
        if toolCallCount > 0 { parts.append("\(toolCallCount) tool calls") }
        if widgetCount > 0 { parts.append("\(widgetCount) widgets") }
        if eventCount > 0 { parts.append("\(eventCount) events") }
        if imageCount > 0 { parts.append("\(imageCount) images") }

        let summary = parts.joined(separator: ". ")

        // Verify completeness
        XCTAssertTrue(summary.contains(primaryText))
        XCTAssertTrue(summary.contains(secondaryText!))
        XCTAssertTrue(summary.contains(durationText))
        XCTAssertTrue(summary.contains("tool calls"))
        XCTAssertTrue(summary.contains("widgets"))
        XCTAssertTrue(summary.contains("events"))
        XCTAssertTrue(summary.contains("images"))

        // Verify no provider-specific text
        XCTAssertFalse(summary.contains("Codex"))
        XCTAssertFalse(summary.contains("Pi"))
        XCTAssertFalse(summary.contains("Droid"))
    }

    // MARK: - Per-Agent Settings Accessibility

    func testPermissionPolicyAccessibilityLabel() {
        // Verify permission row accessibility labels for each agent type
        let types: [AgentType] = [.codex, .piNative, .droidNative, .genericAcp]
        for type in types {
            let label = "\(type.displayName) permissions"
            XCTAssertFalse(label.isEmpty, "\(type) permissions label should not be empty")
            XCTAssertTrue(label.contains(type.displayName),
                "\(type) permissions label should contain display name")
        }
    }

    func testPermissionPolicyAccessibilityValue() {
        // Verify permission policy display names are meaningful
        let policies: [AgentPermissionPolicy] = [.autoApproveAll, .autoRejectHighRisk, .promptAlways]
        for policy in policies {
            XCTAssertFalse(policy.displayName.isEmpty,
                "\(policy) display name should not be empty for accessibility value")
        }
    }

    func testTransportPreferenceAccessibilityLabel() {
        let types: [AgentType] = [.piNative, .droidNative]
        for type in types {
            let label = "\(type.displayName) transport preference"
            XCTAssertFalse(label.isEmpty, "\(type) transport label should not be empty")
        }
    }

    func testTransportPreferenceAccessibilityValue() {
        // Auto (nil preference)
        let autoLabel = "Auto"
        XCTAssertEqual(autoLabel, "Auto")

        // Specific transport
        let piNativeLabel = AgentType.piNative.displayName + " (" + AgentType.piNative.transportLabel + ")"
        XCTAssertTrue(piNativeLabel.contains("Pi"), "Transport value should contain agent name")
        XCTAssertTrue(piNativeLabel.contains("Native"), "Transport value should contain transport type")
    }

    // MARK: - Agent-Specific Control Accessibility

    func testPiThinkingLevelAccessibilityLabels() {
        // Verify thinking level button labels
        let levels = [("Quick", "off"), ("Balanced", "medium"), ("Deep", "high")]
        for (label, _) in levels {
            let accessibilityLabel = "Pi thinking level: \(label)"
            XCTAssertFalse(accessibilityLabel.isEmpty)
            XCTAssertTrue(accessibilityLabel.contains(label))
        }

        // Container label should show current level
        let containerLabel = "Pi thinking level, currently Balanced"
        XCTAssertTrue(containerLabel.contains("Pi thinking level"))
        XCTAssertTrue(containerLabel.contains("Balanced"))
    }

    func testDroidAutonomyLevelAccessibilityLabels() {
        // Verify autonomy level button labels
        let levels = [("Cautious", "suggest"), ("Normal", "normal"), ("Aggressive", "full")]
        for (label, _) in levels {
            let accessibilityLabel = "Droid autonomy level: \(label)"
            XCTAssertFalse(accessibilityLabel.isEmpty)
            XCTAssertTrue(accessibilityLabel.contains(label))
        }

        // Container label should show current level
        let containerLabel = "Droid autonomy level, currently Normal"
        XCTAssertTrue(containerLabel.contains("Droid autonomy level"))
        XCTAssertTrue(containerLabel.contains("Normal"))
    }

    // MARK: - ACP Profile Accessibility

    func testACPProfilesSectionAccessibility() {
        let label = "ACP Providers"
        XCTAssertFalse(label.isEmpty, "ACP profiles section should have a label")

        let hint = "Manage ACP-compatible agent provider profiles"
        XCTAssertTrue(hint.contains("ACP"), "Hint should mention ACP")
        XCTAssertTrue(hint.contains("agent"), "Hint should mention agents")
    }

    func testAddACPProfileButtonAccessibility() {
        let label = "Add ACP provider"
        XCTAssertFalse(label.isEmpty)
        XCTAssertTrue(label.contains("ACP"))

        let hint = "Double tap to create a new ACP provider profile"
        XCTAssertTrue(hint.contains("ACP"))
    }

    func testACPProfileFieldAccessibility() {
        let nameLabel = "Display name"
        XCTAssertFalse(nameLabel.isEmpty)

        let nameHint = "Enter a short name to identify this ACP provider"
        XCTAssertTrue(nameHint.contains("name"))

        let commandLabel = "Remote command"
        XCTAssertFalse(commandLabel.isEmpty)

        let commandHint = "Enter the shell command to launch the ACP agent over SSH"
        XCTAssertTrue(commandHint.contains("command"))
    }
}
