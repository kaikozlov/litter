import XCTest
@testable import Litter

// MARK: - AgentBadgeView / AgentTypePill Tests (VAL-IOS-010, VAL-IOS-011, VAL-IOS-012)

@MainActor
final class AgentBadgePickerTests: XCTestCase {

    // MARK: - VAL-IOS-010: AgentBadgeView renders correctly for all agent types

    func testAllAgentTypesHaveDistinctIcons() {
        let types: [AgentType] = [.codex, .piAcp, .piNative, .droidAcp, .droidNative, .genericAcp]
        let icons = Set(types.map { $0.icon })
        // Each agent type group should have a distinct icon
        // Pi variants share "brain", Droid variants share "robot", Codex has "terminal.fill", GenericAcp has "gearshape.fill"
        XCTAssertTrue(icons.contains("terminal.fill"), "Codex should use terminal.fill icon")
        XCTAssertTrue(icons.contains("brain"), "Pi should use brain icon")
        XCTAssertTrue(icons.contains("robot"), "Droid should use robot icon")
        XCTAssertTrue(icons.contains("gearshape.fill"), "GenericAcp should use gearshape.fill icon")
    }

    func testAllAgentTypesHaveDistinctTintColors() {
        let codexColor = AgentType.codex.tintColor
        let piColor = AgentType.piNative.tintColor
        let droidColor = AgentType.droidNative.tintColor
        let acpColor = AgentType.genericAcp.tintColor

        // Ensure colors are distinct
        XCTAssertNotEqual(codexColor, piColor, "Codex and Pi should have different tint colors")
        XCTAssertNotEqual(codexColor, droidColor, "Codex and Droid should have different tint colors")
        XCTAssertNotEqual(piColor, droidColor, "Pi and Droid should have different tint colors")
        XCTAssertNotEqual(acpColor, codexColor, "GenericAcp and Codex should have different tint colors")
    }

    func testAllAgentTypesHaveNonEmptyDisplayNames() {
        let types: [AgentType] = [.codex, .piAcp, .piNative, .droidAcp, .droidNative, .genericAcp]
        for type in types {
            XCTAssertFalse(type.displayName.isEmpty, "\(type) should have a non-empty display name")
        }
    }

    func testCodexDisplayName() {
        XCTAssertEqual(AgentType.codex.displayName, "Codex")
    }

    func testPiDisplayNames() {
        XCTAssertEqual(AgentType.piNative.displayName, "Pi")
        XCTAssertEqual(AgentType.piAcp.displayName, "Pi")
    }

    func testDroidDisplayNames() {
        XCTAssertEqual(AgentType.droidNative.displayName, "Droid")
        XCTAssertEqual(AgentType.droidAcp.displayName, "Droid")
    }

    func testGenericAcpDisplayName() {
        XCTAssertEqual(AgentType.genericAcp.displayName, "ACP")
    }

    // MARK: - VAL-IOS-011: AgentTypePill renders correctly for all agent types

    func testAllAgentTypesHaveIcons() {
        let types: [AgentType] = [.codex, .piAcp, .piNative, .droidAcp, .droidNative, .genericAcp]
        for type in types {
            XCTAssertFalse(type.icon.isEmpty, "\(type) should have a non-empty SF Symbol icon name")
        }
    }

    func testAllAgentTypesHaveTransportLabels() {
        let types: [AgentType] = [.codex, .piAcp, .piNative, .droidAcp, .droidNative, .genericAcp]
        for type in types {
            XCTAssertFalse(type.transportLabel.isEmpty, "\(type) should have a non-empty transport label")
        }
    }

    func testCodexTransportLabel() {
        XCTAssertEqual(AgentType.codex.transportLabel, "WebSocket")
    }

    func testAcpTransportLabels() {
        XCTAssertEqual(AgentType.piAcp.transportLabel, "ACP")
        XCTAssertEqual(AgentType.droidAcp.transportLabel, "ACP")
        XCTAssertEqual(AgentType.genericAcp.transportLabel, "ACP")
    }

    func testNativeTransportLabels() {
        XCTAssertEqual(AgentType.piNative.transportLabel, "Native")
        XCTAssertEqual(AgentType.droidNative.transportLabel, "Native")
    }

    // MARK: - VAL-IOS-012: AgentBadgeView is accessible

    func testAllAgentTypesHaveAccessibilityLabels() {
        // Verify that displayName produces meaningful labels that would
        // be used as accessibility labels (e.g. "Pi agent", "Droid agent")
        let typesAndLabels: [(AgentType, String)] = [
            (.codex, "Codex agent"),
            (.piNative, "Pi agent"),
            (.piAcp, "Pi agent"),
            (.droidNative, "Droid agent"),
            (.droidAcp, "Droid agent"),
            (.genericAcp, "ACP agent"),
        ]
        for (type, expectedLabel) in typesAndLabels {
            let label = "\(type.displayName) agent"
            XCTAssertEqual(label, expectedLabel, "\(type) accessibility label should be \(expectedLabel)")
            XCTAssertFalse(label.isEmpty, "\(type) should produce a non-empty accessibility label")
        }
    }

    // MARK: - AgentType Helper Properties

    func testIsPiProperty() {
        XCTAssertTrue(AgentType.piNative.isPi)
        XCTAssertTrue(AgentType.piAcp.isPi)
        XCTAssertFalse(AgentType.codex.isPi)
        XCTAssertFalse(AgentType.droidNative.isPi)
        XCTAssertFalse(AgentType.genericAcp.isPi)
    }

    func testIsDroidProperty() {
        XCTAssertTrue(AgentType.droidNative.isDroid)
        XCTAssertTrue(AgentType.droidAcp.isDroid)
        XCTAssertFalse(AgentType.codex.isDroid)
        XCTAssertFalse(AgentType.piNative.isDroid)
        XCTAssertFalse(AgentType.genericAcp.isDroid)
    }

    func testUsesACPProperty() {
        XCTAssertTrue(AgentType.piAcp.usesACP)
        XCTAssertTrue(AgentType.droidAcp.usesACP)
        XCTAssertTrue(AgentType.genericAcp.usesACP)
        XCTAssertFalse(AgentType.codex.usesACP)
        XCTAssertFalse(AgentType.piNative.usesACP)
        XCTAssertFalse(AgentType.droidNative.usesACP)
    }

    // MARK: - VAL-IOS-017: Agent Selection Persistence

    func testAgentSelectionPersistedToUserDefaults() {
        let store = AgentSelectionStore.shared
        let testServerId = "test-server-persistence-\(UUID().uuidString.prefix(8))"
        defer {
            store.setSelectedAgentType(nil, for: testServerId)
        }

        // No selection initially
        XCTAssertNil(store.selectedAgentType(for: testServerId))

        // Set and verify
        store.setSelectedAgentType(.piNative, for: testServerId)
        XCTAssertEqual(store.selectedAgentType(for: testServerId), .piNative)

        // Clear and verify
        store.setSelectedAgentType(nil, for: testServerId)
        XCTAssertNil(store.selectedAgentType(for: testServerId))
    }

    func testAgentSelectionSurvivesMultipleWrites() {
        let store = AgentSelectionStore.shared
        let testServerId = "test-server-multiwrite-\(UUID().uuidString.prefix(8))"
        defer {
            store.setSelectedAgentType(nil, for: testServerId)
        }

        store.setSelectedAgentType(.codex, for: testServerId)
        XCTAssertEqual(store.selectedAgentType(for: testServerId), .codex)

        store.setSelectedAgentType(.droidNative, for: testServerId)
        XCTAssertEqual(store.selectedAgentType(for: testServerId), .droidNative)

        store.setSelectedAgentType(.genericAcp, for: testServerId)
        XCTAssertEqual(store.selectedAgentType(for: testServerId), .genericAcp)
    }

    // MARK: - VAL-IOS-018: Auto-selection when only one agent is available

    func testEffectiveAgentTypeAutoSelectsSingleAgent() {
        let store = AgentSelectionStore.shared
        let testServerId = "test-server-auto-\(UUID().uuidString.prefix(8))"
        defer {
            store.setSelectedAgentType(nil, for: testServerId)
        }

        // When only one agent is available, it should be auto-selected
        let result = store.effectiveAgentType(for: testServerId, availableAgents: [.piNative])
        XCTAssertEqual(result, .piNative)
        // The auto-selection should be persisted
        XCTAssertEqual(store.selectedAgentType(for: testServerId), .piNative)
    }

    func testEffectiveAgentTypeReturnsNilForMultipleAgents() {
        let store = AgentSelectionStore.shared
        let testServerId = "test-server-multi-\(UUID().uuidString.prefix(8))"
        defer {
            store.setSelectedAgentType(nil, for: testServerId)
        }

        // When multiple agents are available with no selection, should return nil
        let result = store.effectiveAgentType(
            for: testServerId,
            availableAgents: [.codex, .piNative]
        )
        XCTAssertNil(result)
    }

    func testEffectiveAgentTypeReturnsStoredSelection() {
        let store = AgentSelectionStore.shared
        let testServerId = "test-server-stored-\(UUID().uuidString.prefix(8))"
        defer {
            store.setSelectedAgentType(nil, for: testServerId)
        }

        store.setSelectedAgentType(.droidAcp, for: testServerId)
        let result = store.effectiveAgentType(
            for: testServerId,
            availableAgents: [.codex, .piNative, .droidAcp]
        )
        XCTAssertEqual(result, .droidAcp)
    }

    func testEffectiveAgentTypeIgnoresInvalidSelection() {
        let store = AgentSelectionStore.shared
        let testServerId = "test-server-invalid-\(UUID().uuidString.prefix(8))"
        defer {
            store.setSelectedAgentType(nil, for: testServerId)
        }

        // Store a selection that's not in the available list
        store.setSelectedAgentType(.droidAcp, for: testServerId)
        let result = store.effectiveAgentType(
            for: testServerId,
            availableAgents: [.codex, .piNative]
        )
        // Should return nil since the stored selection is not available
        XCTAssertNil(result)
    }

    // MARK: - VAL-IOS-019: Picker re-opens when selection is needed but missing

    func testShouldShowPickerForMultipleAgents() {
        let store = AgentSelectionStore.shared
        XCTAssertTrue(store.shouldShowPicker(availableAgents: [.codex, .piNative]))
        XCTAssertTrue(store.shouldShowPicker(availableAgents: [.codex, .piNative, .droidAcp]))
    }

    func testShouldNotShowPickerForSingleAgent() {
        let store = AgentSelectionStore.shared
        XCTAssertFalse(store.shouldShowPicker(availableAgents: [.codex]))
        XCTAssertFalse(store.shouldShowPicker(availableAgents: [.piNative]))
    }

    // MARK: - AgentType Persistence (Codable)

    func testAgentTypeCodableRoundTrip() throws {
        let types: [AgentType] = [.codex, .piAcp, .piNative, .droidAcp, .droidNative, .genericAcp]
        for type in types {
            let encoded = try JSONEncoder().encode(type)
            let decoded = try JSONDecoder().decode(AgentType.self, from: encoded)
            XCTAssertEqual(decoded, type, "\(type) should round-trip through Codable")
        }
    }

    func testAgentTypePersistentKeys() {
        XCTAssertEqual(AgentType.codex.persistentKey, "codex")
        XCTAssertEqual(AgentType.piAcp.persistentKey, "piAcp")
        XCTAssertEqual(AgentType.piNative.persistentKey, "piNative")
        XCTAssertEqual(AgentType.droidAcp.persistentKey, "droidAcp")
        XCTAssertEqual(AgentType.droidNative.persistentKey, "droidNative")
        XCTAssertEqual(AgentType.genericAcp.persistentKey, "genericAcp")
    }

    func testAgentTypeFromPersistentKey() {
        XCTAssertEqual(AgentType.fromPersistentKey("codex"), .codex)
        XCTAssertEqual(AgentType.fromPersistentKey("piAcp"), .piAcp)
        XCTAssertEqual(AgentType.fromPersistentKey("piNative"), .piNative)
        XCTAssertEqual(AgentType.fromPersistentKey("droidAcp"), .droidAcp)
        XCTAssertEqual(AgentType.fromPersistentKey("droidNative"), .droidNative)
        XCTAssertEqual(AgentType.fromPersistentKey("genericAcp"), .genericAcp)
        // Unknown key defaults to Codex
        XCTAssertEqual(AgentType.fromPersistentKey("unknown"), .codex)
    }

    // MARK: - AgentPermissionPolicy Tests (VAL-IOS-010 related)

    func testPermissionPolicyDisplayNames() {
        XCTAssertEqual(AgentPermissionPolicy.autoApproveAll.displayName, "Auto Approve All")
        XCTAssertEqual(AgentPermissionPolicy.autoRejectHighRisk.displayName, "Auto Reject High Risk")
        XCTAssertEqual(AgentPermissionPolicy.promptAlways.displayName, "Prompt Always")
    }

    func testPermissionPolicyShortNames() {
        XCTAssertEqual(AgentPermissionPolicy.autoApproveAll.shortName, "Auto")
        XCTAssertEqual(AgentPermissionPolicy.autoRejectHighRisk.shortName, "Safe")
        XCTAssertEqual(AgentPermissionPolicy.promptAlways.shortName, "Prompt")
    }

    func testPermissionPolicyIcons() {
        XCTAssertFalse(AgentPermissionPolicy.autoApproveAll.icon.isEmpty)
        XCTAssertFalse(AgentPermissionPolicy.autoRejectHighRisk.icon.isEmpty)
        XCTAssertFalse(AgentPermissionPolicy.promptAlways.icon.isEmpty)
    }

    func testPermissionPolicyCodableRoundTrip() throws {
        let policies: [AgentPermissionPolicy] = [.autoApproveAll, .autoRejectHighRisk, .promptAlways]
        for policy in policies {
            let encoded = try JSONEncoder().encode(policy)
            let decoded = try JSONDecoder().decode(AgentPermissionPolicy.self, from: encoded)
            XCTAssertEqual(decoded, policy, "\(policy) should round-trip through Codable")
        }
    }

    // MARK: - Agent Selection Store - Agent Types Cache

    func testAgentTypesCacheUpdateAndRetrieve() {
        let store = AgentSelectionStore.shared
        let testServerId = "test-server-types-\(UUID().uuidString.prefix(8))"

        // Default is [.codex]
        XCTAssertEqual(store.agentTypes(for: testServerId), [.codex])

        // Update and verify
        store.updateAgentTypes([.codex, .piNative], for: testServerId)
        XCTAssertEqual(store.agentTypes(for: testServerId), [.codex, .piNative])

        // Update again
        store.updateAgentTypes([.codex, .piNative, .droidAcp], for: testServerId)
        XCTAssertEqual(store.agentTypes(for: testServerId), [.codex, .piNative, .droidAcp])
    }

    func testSSHCapabilityCache() {
        let store = AgentSelectionStore.shared
        let testServerId = "test-server-ssh-\(UUID().uuidString.prefix(8))"

        // Default is false
        XCTAssertFalse(store.serverHasSSH(testServerId))

        // Update and verify
        store.updateSSHCapability(true, for: testServerId)
        XCTAssertTrue(store.serverHasSSH(testServerId))
    }

    // MARK: - Agent Selection Store - ACP Profile Selection

    func testACPProfileSelectionPersistence() {
        let store = AgentSelectionStore.shared
        let context = "test-context-\(UUID().uuidString.prefix(8))"
        let profileId = UUID()
        defer {
            store.setSelectedACPProfileId(nil, for: context)
        }

        XCTAssertNil(store.selectedACPProfileId(for: context))

        store.setSelectedACPProfileId(profileId, for: context)
        XCTAssertEqual(store.selectedACPProfileId(for: context), profileId)

        store.setSelectedACPProfileId(nil, for: context)
        XCTAssertNil(store.selectedACPProfileId(for: context))
    }

    func testRemoteCommandPersistence() {
        let store = AgentSelectionStore.shared
        defer {
            store.setSelectedRemoteCommand(nil)
        }

        XCTAssertNil(store.selectedRemoteCommand())

        store.setSelectedRemoteCommand("my-agent --acp")
        XCTAssertEqual(store.selectedRemoteCommand(), "my-agent --acp")

        store.setSelectedRemoteCommand(nil)
        XCTAssertNil(store.selectedRemoteCommand())
    }
}
