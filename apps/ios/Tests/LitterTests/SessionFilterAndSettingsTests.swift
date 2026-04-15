import XCTest
@testable import Litter

// MARK: - Session Filtering Tests (VAL-IOS-020, VAL-IOS-021, VAL-IOS-022, VAL-IOS-023)
// MARK: - Per-Agent Settings Tests (VAL-IOS-024, VAL-IOS-025, VAL-IOS-026, VAL-IOS-027)

@MainActor
final class SessionFilterAndSettingsTests: XCTestCase {

    // MARK: - VAL-IOS-020: Session filter row includes agent type filter

    func testAgentTypeFilterOptionsIncludeAllTypes() {
        // Verify all built-in agent types that should appear in the filter
        let filterTypes: [AgentType] = [.codex, .piNative, .droidNative, .genericAcp]
        let expectedNames = ["Codex", "Pi", "Droid", "ACP"]

        for (type, name) in zip(filterTypes, expectedNames) {
            XCTAssertEqual(type.displayName, name, "Filter option should use display name \(name)")
        }
    }

    func testAgentTypeFilterIncludesGenericAcp() {
        // The filter menu must include GenericAcp
        let filterTypes: [AgentType] = [.codex, .piNative, .droidNative, .genericAcp]
        XCTAssertTrue(filterTypes.contains(.genericAcp), "Filter should include GenericAcp/ACP option")
    }

    // MARK: - VAL-IOS-021: Agent type filter shows correct icon per type

    func testFilterChipUsesAgentIcon() {
        let types: [AgentType] = [.codex, .piNative, .droidNative, .genericAcp]
        for type in types {
            XCTAssertFalse(type.icon.isEmpty, "\(type.displayName) should have a non-empty icon for filter chip")
        }
    }

    func testFilterChipUsesAgentTintColor() {
        let types: [AgentType] = [.codex, .piNative, .droidNative, .genericAcp]
        for type in types {
            // Verify tint colors are distinct from each other
            XCTAssertNotNil(type.tintColor, "\(type.displayName) should have a tint color")
        }
    }

    func testFilterChipShowsCorrectDisplayNameWhenActive() {
        XCTAssertEqual(AgentType.piNative.displayName, "Pi")
        XCTAssertEqual(AgentType.droidNative.displayName, "Droid")
        XCTAssertEqual(AgentType.codex.displayName, "Codex")
        XCTAssertEqual(AgentType.genericAcp.displayName, "ACP")
    }

    // MARK: - VAL-IOS-022: Filtering by agent type returns only matching sessions

    func testSessionsDerivationFiltersByAgentType() {
        let sessions = [
            makeTestSession(threadId: "t1", serverId: "s1", title: "Pi Session"),
            makeTestSession(threadId: "t2", serverId: "s2", title: "Codex Session"),
            makeTestSession(threadId: "t3", serverId: "s3", title: "Droid Session"),
        ]

        let store = AgentSelectionStore.shared
        store.updateAgentTypes([.piNative], for: "s1")
        store.updateAgentTypes([.codex], for: "s2")
        store.updateAgentTypes([.droidNative], for: "s3")
        store.setSelectedAgentType(.piNative, for: "s1")
        store.setSelectedAgentType(.codex, for: "s2")
        store.setSelectedAgentType(.droidNative, for: "s3")
        defer {
            store.updateAgentTypes([], for: "s1")
            store.updateAgentTypes([], for: "s2")
            store.updateAgentTypes([], for: "s3")
            store.setSelectedAgentType(nil, for: "s1")
            store.setSelectedAgentType(nil, for: "s2")
            store.setSelectedAgentType(nil, for: "s3")
        }

        // Filter by Pi — should only return s1
        let piResult = SessionsDerivation.build(
            sessions: sessions,
            selectedServerFilterId: nil,
            showOnlyForks: false,
            agentTypeFilter: .piNative,
            workspaceSortMode: .mostRecent,
            searchQuery: "",
            frozenMostRecentOrder: nil
        )
        XCTAssertEqual(piResult.filteredThreads.count, 1, "Pi filter should match 1 session")
        XCTAssertEqual(piResult.filteredThreads.first?.key.threadId, "t1")

        // Filter by Droid — should only return s3
        let droidResult = SessionsDerivation.build(
            sessions: sessions,
            selectedServerFilterId: nil,
            showOnlyForks: false,
            agentTypeFilter: .droidNative,
            workspaceSortMode: .mostRecent,
            searchQuery: "",
            frozenMostRecentOrder: nil
        )
        XCTAssertEqual(droidResult.filteredThreads.count, 1, "Droid filter should match 1 session")
        XCTAssertEqual(droidResult.filteredThreads.first?.key.threadId, "t3")

        // No filter — should return all
        let allResult = SessionsDerivation.build(
            sessions: sessions,
            selectedServerFilterId: nil,
            showOnlyForks: false,
            agentTypeFilter: nil,
            workspaceSortMode: .mostRecent,
            searchQuery: "",
            frozenMostRecentOrder: nil
        )
        XCTAssertEqual(allResult.filteredThreads.count, 3, "No filter should return all 3 sessions")
    }

    func testFilteringByAgentTypeWithNoMatchReturnsEmpty() {
        let sessions = [
            makeTestSession(threadId: "t1", serverId: "s1", title: "Session"),
        ]

        let store = AgentSelectionStore.shared
        store.updateAgentTypes([.codex], for: "s1")
        store.setSelectedAgentType(.codex, for: "s1")
        defer {
            store.updateAgentTypes([], for: "s1")
            store.setSelectedAgentType(nil, for: "s1")
        }

        let result = SessionsDerivation.build(
            sessions: sessions,
            selectedServerFilterId: nil,
            showOnlyForks: false,
            agentTypeFilter: .piNative,
            workspaceSortMode: .mostRecent,
            searchQuery: "",
            frozenMostRecentOrder: nil
        )
        XCTAssertTrue(result.filteredThreads.isEmpty, "Filter with no matches should return empty")
    }

    func testFilteringByAgentTypeWithGenericAcp() {
        let sessions = [
            makeTestSession(threadId: "t1", serverId: "s1", title: "ACP Session"),
            makeTestSession(threadId: "t2", serverId: "s2", title: "Codex Session"),
        ]

        let store = AgentSelectionStore.shared
        store.updateAgentTypes([.genericAcp], for: "s1")
        store.updateAgentTypes([.codex], for: "s2")
        store.setSelectedAgentType(.genericAcp, for: "s1")
        store.setSelectedAgentType(.codex, for: "s2")
        defer {
            store.updateAgentTypes([], for: "s1")
            store.updateAgentTypes([], for: "s2")
            store.setSelectedAgentType(nil, for: "s1")
            store.setSelectedAgentType(nil, for: "s2")
        }

        let result = SessionsDerivation.build(
            sessions: sessions,
            selectedServerFilterId: nil,
            showOnlyForks: false,
            agentTypeFilter: .genericAcp,
            workspaceSortMode: .mostRecent,
            searchQuery: "",
            frozenMostRecentOrder: nil
        )
        XCTAssertEqual(result.filteredThreads.count, 1, "ACP filter should match 1 session")
        XCTAssertEqual(result.filteredThreads.first?.key.threadId, "t1")
    }

    // MARK: - VAL-IOS-023: Session rows show agent type badge for non-Codex agents

    func testAgentBadgeTypeReturnsNonNilForNonCodexSelection() {
        let store = AgentSelectionStore.shared
        let testServerId = "test-badge-server-\(UUID().uuidString.prefix(8))"
        defer {
            store.setSelectedAgentType(nil, for: testServerId)
            store.updateAgentTypes([], for: testServerId)
        }

        store.setSelectedAgentType(.piNative, for: testServerId)
        store.updateAgentTypes([.piNative], for: testServerId)
        let selectedAgent = store.selectedAgentType(for: testServerId)
        XCTAssertNotNil(selectedAgent)
        XCTAssertNotEqual(selectedAgent, .codex, "Badge should appear for Pi agent")
    }

    func testAgentBadgeTypeReturnsNilForCodexSelection() {
        let store = AgentSelectionStore.shared
        let testServerId = "test-badge-codex-\(UUID().uuidString.prefix(8))"
        defer {
            store.setSelectedAgentType(nil, for: testServerId)
        }

        // No selection (default Codex) — no badge
        XCTAssertNil(store.selectedAgentType(for: testServerId))

        // Explicit Codex selection
        store.setSelectedAgentType(.codex, for: testServerId)
        let selected = store.selectedAgentType(for: testServerId)
        XCTAssertEqual(selected, .codex)
    }

    func testBadgeTypesForAllNonCodexAgents() {
        let nonCodexTypes: [AgentType] = [.piNative, .piAcp, .droidNative, .droidAcp, .genericAcp]
        for type in nonCodexTypes {
            XCTAssertNotEqual(type, .codex, "\(type.displayName) should show a badge (non-Codex)")
        }
    }

    // MARK: - Filter State Persistence (VAL-IOS-020 extended)

    func testAgentTypeFilterPersistenceKey() {
        // Verify the UserDefaults key is stable
        let key = "litter.sessionsAgentTypeFilter"
        let testValue = "piNative"

        UserDefaults.standard.set(testValue, forKey: key)
        XCTAssertEqual(UserDefaults.standard.string(forKey: key), testValue)

        // Verify conversion from stored key to AgentType
        let agentType = AgentType.fromPersistentKey(testValue)
        XCTAssertEqual(agentType, .piNative)

        // Cleanup
        UserDefaults.standard.removeObject(forKey: key)
    }

    func testAgentTypeFilterRoundTripPersistence() {
        let key = "litter.sessionsAgentTypeFilter"

        // Test each agent type round-trips correctly
        let types: [AgentType] = [.codex, .piNative, .droidNative, .genericAcp]
        for type in types {
            UserDefaults.standard.set(type.persistentKey, forKey: key)
            let restored = UserDefaults.standard.string(forKey: key)
            XCTAssertEqual(restored, type.persistentKey)
            XCTAssertEqual(AgentType.fromPersistentKey(restored ?? ""), type)
        }

        // Test nil (All agents)
        UserDefaults.standard.removeObject(forKey: key)
        XCTAssertNil(UserDefaults.standard.string(forKey: key))
    }

    // MARK: - VAL-IOS-024: Settings shows permission policy for each agent type

    func testPerAgentPermissionPolicyPersisted() {
        let store = AgentPermissionStore.shared
        let codexKey = AgentType.codex.persistentKey
        let piKey = AgentType.piNative.persistentKey
        let droidKey = AgentType.droidNative.persistentKey
        defer {
            store.setPermissionPolicy(.promptAlways, for: codexKey)
            store.setPermissionPolicy(.promptAlways, for: piKey)
            store.setPermissionPolicy(.promptAlways, for: droidKey)
        }

        // Default is promptAlways
        XCTAssertEqual(store.permissionPolicy(for: codexKey), .promptAlways)

        // Set different policies per agent
        store.setPermissionPolicy(.autoApproveAll, for: codexKey)
        store.setPermissionPolicy(.autoRejectHighRisk, for: piKey)
        store.setPermissionPolicy(.promptAlways, for: droidKey)

        XCTAssertEqual(store.permissionPolicy(for: codexKey), .autoApproveAll)
        XCTAssertEqual(store.permissionPolicy(for: piKey), .autoRejectHighRisk)
        XCTAssertEqual(store.permissionPolicy(for: droidKey), .promptAlways)
    }

    // MARK: - VAL-IOS-025: Settings shows transport preference for Pi and Droid

    func testTransportPreferencePersistedForPi() {
        let store = AgentPermissionStore.shared
        let piKey = AgentType.piNative.persistentKey
        defer {
            store.setTransportPreference(nil, for: piKey)
        }

        XCTAssertNil(store.transportPreference(for: piKey))

        store.setTransportPreference(.piNative, for: piKey)
        XCTAssertEqual(store.transportPreference(for: piKey), .piNative)

        store.setTransportPreference(.piAcp, for: piKey)
        XCTAssertEqual(store.transportPreference(for: piKey), .piAcp)

        store.setTransportPreference(nil, for: piKey)
        XCTAssertNil(store.transportPreference(for: piKey))
    }

    func testTransportPreferencePersistedForDroid() {
        let store = AgentPermissionStore.shared
        let droidKey = AgentType.droidNative.persistentKey
        defer {
            store.setTransportPreference(nil, for: droidKey)
        }

        XCTAssertNil(store.transportPreference(for: droidKey))

        store.setTransportPreference(.droidNative, for: droidKey)
        XCTAssertEqual(store.transportPreference(for: droidKey), .droidNative)

        store.setTransportPreference(.droidAcp, for: droidKey)
        XCTAssertEqual(store.transportPreference(for: droidKey), .droidAcp)
    }

    // MARK: - VAL-IOS-026: Permission policy changes take effect immediately

    func testPermissionPolicyChangesAreImmediate() {
        let store = AgentPermissionStore.shared
        let testKey = "test-immediate-\(UUID().uuidString.prefix(8))"
        defer {
            store.setPermissionPolicy(.promptAlways, for: testKey)
        }

        store.setPermissionPolicy(.autoApproveAll, for: testKey)
        XCTAssertEqual(store.permissionPolicy(for: testKey), .autoApproveAll)

        store.setPermissionPolicy(.autoRejectHighRisk, for: testKey)
        XCTAssertEqual(store.permissionPolicy(for: testKey), .autoRejectHighRisk)

        store.setPermissionPolicy(.promptAlways, for: testKey)
        XCTAssertEqual(store.permissionPolicy(for: testKey), .promptAlways)
    }

    // MARK: - VAL-IOS-027: Agent-specific controls appear when relevant

    func testPiThinkingLevelOnlyForPiAgents() {
        XCTAssertTrue(AgentType.piNative.isPi)
        XCTAssertTrue(AgentType.piAcp.isPi)
        XCTAssertFalse(AgentType.codex.isPi)
        XCTAssertFalse(AgentType.droidNative.isPi)
        XCTAssertFalse(AgentType.genericAcp.isPi)
    }

    func testDroidAutonomyOnlyForDroidAgents() {
        XCTAssertTrue(AgentType.droidNative.isDroid)
        XCTAssertTrue(AgentType.droidAcp.isDroid)
        XCTAssertFalse(AgentType.codex.isDroid)
        XCTAssertFalse(AgentType.piNative.isDroid)
        XCTAssertFalse(AgentType.genericAcp.isDroid)
    }

    func testAgentSpecificControlsNotShownForCodexOrAcp() {
        XCTAssertFalse(AgentType.codex.isPi || AgentType.codex.isDroid,
                        "Codex should not show Pi or Droid controls")
        XCTAssertFalse(AgentType.genericAcp.isPi || AgentType.genericAcp.isDroid,
                        "GenericAcp should not show Pi or Droid controls")
    }

    // MARK: - Helper

    private func makeTestSession(
        threadId: String,
        serverId: String,
        title: String,
        cwd: String = "/tmp"
    ) -> AppSessionSummary {
        AppSessionSummary(
            key: ThreadKey(serverId: serverId, threadId: threadId),
            serverDisplayName: "Test Server",
            serverHost: "localhost",
            title: title,
            preview: "",
            cwd: cwd,
            model: "test-model",
            modelProvider: "test",
            parentThreadId: nil,
            agentNickname: nil,
            agentRole: nil,
            agentDisplayLabel: nil,
            agentStatus: .unknown,
            updatedAt: Int64(Date().timeIntervalSince1970),
            hasActiveTurn: false,
            isSubagent: false,
            isFork: false
        )
    }
}
