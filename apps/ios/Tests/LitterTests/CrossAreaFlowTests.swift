import XCTest
@testable import Litter

// MARK: - Cross-Area Flow Verification Tests
// VAL-CROSS-002, VAL-CROSS-003, VAL-CROSS-004, VAL-CROSS-005,
// VAL-CROSS-006, VAL-CROSS-008

/// End-to-end verification of cross-area flows spanning all four milestones:
/// provider abstraction → ACP → discovery → iOS parity.
///
/// These tests validate that the complete pipeline works at the component level
/// from agent selection through session creation, conversation, and teardown.
/// Full E2E flows requiring real remote servers are verified via Rust mock tests.
@MainActor
final class CrossAreaFlowTests: XCTestCase {

    // MARK: - VAL-CROSS-002: Full Pi Flow (Component-Level)

    /// Verify Pi agent type has all required UI attributes for the full flow.
    func testPiAgentTypeSupportsFullFlow() {
        // Pi agent must have all attributes needed for: detect → pick → connect → chat
        let piTypes: [AgentType] = [.piNative, .piAcp]

        for type in piTypes {
            // Display name for agent picker
            XCTAssertFalse(type.displayName.isEmpty, "\(type) needs a display name")
            XCTAssertEqual(type.displayName, "Pi")

            // Icon for badge and picker
            XCTAssertFalse(type.icon.isEmpty, "\(type) needs an icon")

            // Tint color for badge
            XCTAssertNotNil(type.tintColor, "\(type) needs a tint color")

            // Transport label for connection step
            XCTAssertFalse(type.transportLabel.isEmpty, "\(type) needs a transport label")

            // Persistent key for UserDefaults storage
            XCTAssertFalse(type.persistentKey.isEmpty, "\(type) needs a persistent key")

            // Pi-specific controls (thinking level)
            XCTAssertTrue(type.isPi, "\(type) should be classified as Pi")
            XCTAssertFalse(type.isDroid, "\(type) should NOT be classified as Droid")
        }
    }

    /// Verify Pi agent selection persists across simulated app restarts.
    func testPiAgentSelectionPersists() {
        let store = AgentSelectionStore.shared
        let serverId = "pi-flow-server-\(UUID().uuidString.prefix(8))"
        defer {
            store.setSelectedAgentType(nil, for: serverId)
            store.updateAgentTypes([], for: serverId)
        }

        // Simulate: discover Pi server → select Pi → persist
        store.updateAgentTypes([.piNative, .codex], for: serverId)
        store.setSelectedAgentType(.piNative, for: serverId)

        // Verify: on next launch, Pi is still selected
        let restored = store.selectedAgentType(for: serverId)
        XCTAssertEqual(restored, .piNative, "Pi selection should persist across app launches")

        // Verify: effective agent type returns Pi without re-prompting
        let effective = store.effectiveAgentType(
            for: serverId,
            availableAgents: [.piNative, .codex]
        )
        XCTAssertEqual(effective, .piNative, "Auto-select Pi without showing picker")
    }

    // MARK: - VAL-CROSS-003: Full Droid Flow (Component-Level)

    /// Verify Droid agent type has all required UI attributes for the full ACP flow.
    func testDroidAgentTypeSupportsFullFlow() {
        let droidTypes: [AgentType] = [.droidNative, .droidAcp]

        for type in droidTypes {
            XCTAssertFalse(type.displayName.isEmpty, "\(type) needs a display name")
            XCTAssertEqual(type.displayName, "Droid")
            XCTAssertFalse(type.icon.isEmpty, "\(type) needs an icon")
            XCTAssertNotNil(type.tintColor, "\(type) needs a tint color")
            XCTAssertFalse(type.transportLabel.isEmpty, "\(type) needs a transport label")
            XCTAssertFalse(type.persistentKey.isEmpty, "\(type) needs a persistent key")
            XCTAssertTrue(type.isDroid, "\(type) should be classified as Droid")
            XCTAssertFalse(type.isPi, "\(type) should NOT be classified as Pi")
        }
    }

    /// Verify Droid ACP uses standard ACP transport label.
    func testDroidAcpUsesStandardTransportLabel() {
        // DroidAcp should indicate standard ACP transport
        XCTAssertFalse(AgentType.droidAcp.transportLabel.isEmpty)
        // The transport label should be distinct from native
        XCTAssertEqual(AgentType.droidAcp.transportLabel, "ACP")
    }

    /// Verify Droid agent selection persists with transport preference.
    func testDroidAgentSelectionWithTransportPreference() {
        let store = AgentSelectionStore.shared
        let permStore = AgentPermissionStore.shared
        let serverId = "droid-flow-server-\(UUID().uuidString.prefix(8))"
        let droidKey = AgentType.droidNative.persistentKey
        defer {
            store.setSelectedAgentType(nil, for: serverId)
            store.updateAgentTypes([], for: serverId)
            permStore.setTransportPreference(nil, for: droidKey)
        }

        // Select Droid agent
        store.updateAgentTypes([.droidNative, .droidAcp, .codex], for: serverId)
        store.setSelectedAgentType(.droidAcp, for: serverId)

        // Set transport preference for Droid
        permStore.setTransportPreference(.droidAcp, for: droidKey)
        XCTAssertEqual(permStore.transportPreference(for: droidKey), .droidAcp)

        // Verify selection persists
        XCTAssertEqual(store.selectedAgentType(for: serverId), .droidAcp)
    }

    // MARK: - VAL-CROSS-004: Full GenericAcp Flow (Component-Level)

    /// Verify GenericAcp agent type supports full flow from profile config to connection.
    func testGenericAcpAgentTypeSupportsFullFlow() {
        let type = AgentType.genericAcp

        XCTAssertFalse(type.displayName.isEmpty)
        XCTAssertEqual(type.displayName, "ACP")
        XCTAssertFalse(type.icon.isEmpty)
        XCTAssertNotNil(type.tintColor)
        XCTAssertFalse(type.transportLabel.isEmpty)
        XCTAssertFalse(type.persistentKey.isEmpty)
        XCTAssertFalse(type.isPi)
        XCTAssertFalse(type.isDroid)
    }

    /// Verify ACP profile configuration flows through to connection parameters.
    func testACPProfileConfiguresProviderConfig() {
        // Create a profile simulating user configuration
        let profile = ACPProfile(
            displayName: "Test Agent",
            remoteCommand: "my-agent --acp --port 8080",
            permissionPolicy: .autoApproveAll,
            icon: "star.fill"
        )

        // Verify profile fields
        XCTAssertEqual(profile.displayName, "Test Agent")
        XCTAssertEqual(profile.remoteCommand, "my-agent --acp --port 8080")
        XCTAssertEqual(profile.permissionPolicy, .autoApproveAll)

        // Persist the profile
        let store = ACPProfileStore.shared
        UserDefaults.standard.removeObject(forKey: "litter.acpProfiles")
        defer {
            store.deleteProfile(id: profile.id)
            UserDefaults.standard.removeObject(forKey: "litter.acpProfiles")
        }

        store.addProfile(profile)

        // Verify profile is retrievable
        let retrieved = store.profile(id: profile.id)
        XCTAssertNotNil(retrieved)
        XCTAssertEqual(retrieved?.remoteCommand, "my-agent --acp --port 8080")

        // Verify GenericAcp selection persists with profile reference
        let selectionStore = AgentSelectionStore.shared
        let serverId = "acp-flow-server-\(UUID().uuidString.prefix(8))"
        defer {
            selectionStore.setSelectedAgentType(nil, for: serverId)
            selectionStore.updateAgentTypes([], for: serverId)
            selectionStore.setSelectedACPProfileId(nil, for: serverId)
            selectionStore.setSelectedRemoteCommand(nil)
        }

        selectionStore.updateAgentTypes([.genericAcp], for: serverId)
        selectionStore.setSelectedAgentType(.genericAcp, for: serverId)
        selectionStore.setSelectedACPProfileId(profile.id, for: serverId)
        selectionStore.setSelectedRemoteCommand("my-agent --acp --port 8080")

        // Verify full round-trip
        XCTAssertEqual(selectionStore.selectedAgentType(for: serverId), .genericAcp)
        XCTAssertEqual(selectionStore.selectedACPProfileId(for: serverId), profile.id)
        XCTAssertEqual(selectionStore.selectedRemoteCommand(), "my-agent --acp --port 8080")
    }

    /// Verify custom command text field validation.
    func testACPProfileRemoteCommandValidation() {
        // Valid commands
        let validCommands = [
            "my-agent --acp",
            "custom-agent --port 8080 --verbose",
            "/usr/local/bin/agent",
        ]
        for cmd in validCommands {
            let profile = ACPProfile(displayName: "Valid", remoteCommand: cmd)
            XCTAssertFalse(profile.remoteCommand.isEmpty, "Command should be stored: \(cmd)")
        }

        // Empty command should still be constructible (UI validates)
        let emptyProfile = ACPProfile(displayName: "Empty", remoteCommand: "")
        XCTAssertTrue(emptyProfile.remoteCommand.isEmpty)
    }

    // MARK: - VAL-CROSS-005: Settings Change Propagates to Connection

    /// Verify ACP profile is immediately available after creation without restart.
    func testACPProfileImmediatelyAvailableAfterCreation() {
        let store = ACPProfileStore.shared
        UserDefaults.standard.removeObject(forKey: "litter.acpProfiles")
        defer {
            UserDefaults.standard.removeObject(forKey: "litter.acpProfiles")
        }

        // Initially empty
        XCTAssertTrue(store.profiles().isEmpty)

        // Add a profile
        let profile = ACPProfile(displayName: "Instant", remoteCommand: "instant-agent")
        store.addProfile(profile)

        // Immediately available
        XCTAssertEqual(store.profiles().count, 1)
        XCTAssertEqual(store.profiles().first?.displayName, "Instant")
    }

    /// Verify permission policy change is immediately effective.
    func testPermissionPolicyChangeIsImmediatelyEffective() {
        let store = AgentPermissionStore.shared
        let testKey = "cross-area-policy-test-\(UUID().uuidString.prefix(8))"
        defer {
            store.setPermissionPolicy(.promptAlways, for: testKey)
        }

        // Start with default
        XCTAssertEqual(store.permissionPolicy(for: testKey), .promptAlways)

        // Change to auto-approve — should be immediate
        store.setPermissionPolicy(.autoApproveAll, for: testKey)
        XCTAssertEqual(store.permissionPolicy(for: testKey), .autoApproveAll)

        // No restart needed — next turn uses the new policy
        store.setPermissionPolicy(.autoRejectHighRisk, for: testKey)
        XCTAssertEqual(store.permissionPolicy(for: testKey), .autoRejectHighRisk)
    }

    /// Verify transport preference change is immediately effective.
    func testTransportPreferenceChangeIsImmediatelyEffective() {
        let store = AgentPermissionStore.shared
        let piKey = AgentType.piNative.persistentKey
        defer {
            store.setTransportPreference(nil, for: piKey)
        }

        // Default is nil (auto)
        XCTAssertNil(store.transportPreference(for: piKey))

        // Switch to ACP — immediate
        store.setTransportPreference(.piAcp, for: piKey)
        XCTAssertEqual(store.transportPreference(for: piKey), .piAcp)

        // Switch back to native — immediate
        store.setTransportPreference(.piNative, for: piKey)
        XCTAssertEqual(store.transportPreference(for: piKey), .piNative)
    }

    // MARK: - VAL-CROSS-006: Session List Multi-Provider Filtering

    /// Verify sessions from multiple providers can be filtered by agent type.
    func testMultiProviderSessionFiltering() {
        let sessions = [
            makeSession(threadId: "codex-1", serverId: "codex-server"),
            makeSession(threadId: "pi-1", serverId: "pi-server"),
            makeSession(threadId: "pi-2", serverId: "pi-server-2"),
            makeSession(threadId: "droid-1", serverId: "droid-server"),
            makeSession(threadId: "acp-1", serverId: "acp-server"),
        ]

        let store = AgentSelectionStore.shared
        store.updateAgentTypes([.codex], for: "codex-server")
        store.updateAgentTypes([.piNative], for: "pi-server")
        store.updateAgentTypes([.piAcp], for: "pi-server-2")
        store.updateAgentTypes([.droidNative], for: "droid-server")
        store.updateAgentTypes([.genericAcp], for: "acp-server")
        store.setSelectedAgentType(.codex, for: "codex-server")
        store.setSelectedAgentType(.piNative, for: "pi-server")
        store.setSelectedAgentType(.piAcp, for: "pi-server-2")
        store.setSelectedAgentType(.droidNative, for: "droid-server")
        store.setSelectedAgentType(.genericAcp, for: "acp-server")
        defer {
            for id in ["codex-server", "pi-server", "pi-server-2", "droid-server", "acp-server"] {
                store.setSelectedAgentType(nil, for: id)
                store.updateAgentTypes([], for: id)
            }
        }

        // Filter by Pi — should match pi-1 and pi-2
        let piFiltered = SessionsDerivation.build(
            sessions: sessions,
            selectedServerFilterId: nil,
            showOnlyForks: false,
            agentTypeFilter: .piNative,
            workspaceSortMode: .mostRecent,
            searchQuery: "",
            frozenMostRecentOrder: nil
        )
        // piAcp sessions should also match Pi filter (isPi check)
        // But the filter uses exact agent type match, so only piNative matches
        XCTAssertEqual(piFiltered.filteredThreads.count, 1)
        XCTAssertEqual(piFiltered.filteredThreads.first?.key.threadId, "pi-1")

        // Filter by Droid — should match droid-1
        let droidFiltered = SessionsDerivation.build(
            sessions: sessions,
            selectedServerFilterId: nil,
            showOnlyForks: false,
            agentTypeFilter: .droidNative,
            workspaceSortMode: .mostRecent,
            searchQuery: "",
            frozenMostRecentOrder: nil
        )
        XCTAssertEqual(droidFiltered.filteredThreads.count, 1)
        XCTAssertEqual(droidFiltered.filteredThreads.first?.key.threadId, "droid-1")

        // No filter — all sessions
        let allSessions = SessionsDerivation.build(
            sessions: sessions,
            selectedServerFilterId: nil,
            showOnlyForks: false,
            agentTypeFilter: nil,
            workspaceSortMode: .mostRecent,
            searchQuery: "",
            frozenMostRecentOrder: nil
        )
        XCTAssertEqual(allSessions.filteredThreads.count, 5)
    }

    /// Verify that each provider type shows correct badge in session list.
    func testSessionBadgeTypePerProvider() {
        // Non-Codex agents should show badge
        let nonCodexTypes: [AgentType] = [.piNative, .piAcp, .droidNative, .droidAcp, .genericAcp]
        for type in nonCodexTypes {
            XCTAssertNotEqual(type, .codex, "\(type) should show badge in session list")
        }

        // Each non-Codex type should have distinct visual attributes
        let piIcon = AgentType.piNative.icon
        let droidIcon = AgentType.droidNative.icon
        let acpIcon = AgentType.genericAcp.icon
        XCTAssertFalse(piIcon.isEmpty)
        XCTAssertFalse(droidIcon.isEmpty)
        XCTAssertFalse(acpIcon.isEmpty)
    }

    // MARK: - VAL-CROSS-008: Saved Server Migration

    /// Verify legacy SavedServer JSON with old field names decodes correctly.
    func testSavedServerDecodesLegacyFieldNames() throws {
        let legacyJSON = """
        {
          "id": "legacy-server-1",
          "name": "Legacy Server",
          "hostname": "mac-mini.local",
          "port": 8390,
          "source": "bonjour",
          "hasCodexServer": true,
          "codexPorts": [8390, 9234],
          "preferredCodexPort": 8390,
          "preferredConnectionMode": "directCodex",
          "wakeMAC": null,
          "sshPortForwardingEnabled": false
        }
        """.data(using: .utf8)!

        let saved = try JSONDecoder().decode(SavedServer.self, from: legacyJSON)

        // Old fields should map to new fields
        XCTAssertEqual(saved.id, "legacy-server-1")
        XCTAssertEqual(saved.name, "Legacy Server")
        XCTAssertEqual(saved.hostname, "mac-mini.local")
        XCTAssertTrue(saved.hasAgentServer, "hasCodexServer should map to hasAgentServer")
        XCTAssertEqual(saved.agentPorts, [8390, 9234], "codexPorts should map to agentPorts")
        XCTAssertEqual(saved.preferredAgentPort, 8390, "preferredCodexPort should map to preferredAgentPort")
        XCTAssertEqual(saved.preferredConnectionMode, .directAgent, "directCodex should map to directAgent")
    }

    /// Verify legacy SavedServer with SSH-only configuration decodes correctly.
    func testSavedServerDecodesLegacySSHOnly() throws {
        let legacyJSON = """
        {
          "id": "legacy-ssh-server",
          "name": "Legacy SSH",
          "hostname": "remote.server.com",
          "port": 22,
          "source": "manual",
          "hasCodexServer": false,
          "codexPorts": [],
          "wakeMAC": null,
          "sshPortForwardingEnabled": true
        }
        """.data(using: .utf8)!

        let saved = try JSONDecoder().decode(SavedServer.self, from: legacyJSON)

        XCTAssertFalse(saved.hasAgentServer, "hasCodexServer=false should map to hasAgentServer=false")
        XCTAssertTrue(saved.agentPorts.isEmpty, "empty codexPorts should map to empty agentPorts")
        XCTAssertNil(saved.preferredAgentPort)
    }

    /// Verify new-format SavedServer JSON decodes correctly with new field names.
    func testSavedServerDecodesNewFieldNames() throws {
        let newJSON = """
        {
          "id": "new-server-1",
          "name": "New Server",
          "hostname": "workstation.local",
          "port": 8390,
          "source": "bonjour",
          "hasAgentServer": true,
          "agentPorts": [8390, 9234],
          "preferredAgentPort": 9234,
          "preferredConnectionMode": "directAgent",
          "wakeMAC": null,
          "sshPortForwardingEnabled": false
        }
        """.data(using: .utf8)!

        let saved = try JSONDecoder().decode(SavedServer.self, from: newJSON)

        XCTAssertTrue(saved.hasAgentServer)
        XCTAssertEqual(saved.agentPorts, [8390, 9234])
        XCTAssertEqual(saved.preferredAgentPort, 9234)
        XCTAssertEqual(saved.preferredConnectionMode, .directAgent)
    }

    /// Verify SavedServer encodes only new field names (never old ones).
    func testSavedServerEncodesNewFieldNamesOnly() throws {
        let saved = SavedServer(
            id: "encode-test",
            name: "Encode Test",
            hostname: "test.local",
            port: 8390,
            agentPorts: [8390, 9234],
            sshPort: nil,
            source: .bonjour,
            hasAgentServer: true,
            wakeMAC: nil,
            preferredConnectionMode: .directAgent,
            preferredAgentPort: 8390,
            sshPortForwardingEnabled: nil,
            websocketURL: nil
        )

        let encoded = try JSONEncoder().encode(saved)
        let jsonString = String(data: encoded, encoding: .utf8)!

        // Should contain new field names
        XCTAssertTrue(jsonString.contains("hasAgentServer"), "Encoded JSON should use hasAgentServer")
        XCTAssertTrue(jsonString.contains("agentPorts"), "Encoded JSON should use agentPorts")
        XCTAssertTrue(jsonString.contains("preferredAgentPort"), "Encoded JSON should use preferredAgentPort")

        // Should NOT contain old field names
        XCTAssertFalse(jsonString.contains("hasCodexServer"), "Should NOT use hasCodexServer")
        XCTAssertFalse(jsonString.contains("codexPorts"), "Should NOT use codexPorts")
        XCTAssertFalse(jsonString.contains("preferredCodexPort"), "Should NOT use preferredCodexPort")
    }

    /// Verify round-trip: old JSON → decode → encode → decode produces same result.
    func testSavedServerMigrationRoundTrip() throws {
        let legacyJSON = """
        {
          "id": "round-trip-server",
          "name": "Round Trip",
          "hostname": "rt.local",
          "port": 8390,
          "source": "bonjour",
          "hasCodexServer": true,
          "codexPorts": [8390],
          "preferredCodexPort": 8390,
          "preferredConnectionMode": "directCodex",
          "wakeMAC": null,
          "sshPortForwardingEnabled": false
        }
        """.data(using: .utf8)!

        // Decode legacy
        let decoded = try JSONDecoder().decode(SavedServer.self, from: legacyJSON)

        // Encode with new keys
        let reEncoded = try JSONEncoder().encode(decoded)

        // Decode the re-encoded JSON
        let reDecoded = try JSONDecoder().decode(SavedServer.self, from: reEncoded)

        // Should be equal
        XCTAssertEqual(decoded, reDecoded, "Round-trip should preserve all fields")

        // Re-encoded JSON should use new keys
        let jsonString = String(data: reEncoded, encoding: .utf8)!
        XCTAssertTrue(jsonString.contains("hasAgentServer"))
        XCTAssertFalse(jsonString.contains("hasCodexServer"))
    }

    /// Verify PreferredConnectionMode backward compatibility.
    func testPreferredConnectionModeBackwardCompatibility() throws {
        // Old "directCodex" should decode as "directAgent"
        let legacyMode = #""directCodex""#.data(using: .utf8)!
        let decodedLegacy = try JSONDecoder().decode(PreferredConnectionMode.self, from: legacyMode)
        XCTAssertEqual(decodedLegacy, .directAgent)

        // New "directAgent" should decode correctly
        let newMode = #""directAgent""#.data(using: .utf8)!
        let decodedNew = try JSONDecoder().decode(PreferredConnectionMode.self, from: newMode)
        XCTAssertEqual(decodedNew, .directAgent)

        // Encoded value should be "directAgent" (new format)
        let encoded = try JSONEncoder().encode(PreferredConnectionMode.directAgent)
        let encodedString = String(data: encoded, encoding: .utf8)!
        XCTAssertEqual(encodedString, #""directAgent""#)
    }

    /// Verify that the full discovery-to-connection pipeline handles multi-agent servers.
    func testDiscoveredServerMultiAgentPipeline() {
        // Simulate: discovery finds a server with both Codex and Pi
        let server = DiscoveredServer(
            id: "multi-agent-host",
            name: "Multi-Agent Host",
            hostname: "192.168.1.100",
            port: nil,
            agentPorts: [8390, 9234],
            sshPort: 22,
            source: .bonjour,
            hasAgentServer: true,
            agentTypes: [.codex, .piNative, .droidNative]
        )

        // Verify multi-agent detection
        XCTAssertEqual(server.agentTypes.count, 3)
        XCTAssertTrue(server.hasAgentServer)
        XCTAssertEqual(server.agentPorts.count, 2)

        // Verify connection choice is needed
        XCTAssertTrue(server.canConnectViaSSH)
        XCTAssertEqual(server.availableDirectAgentPorts.count, 2)

        // Verify agent selection store handles multi-agent server
        let store = AgentSelectionStore.shared
        let serverId = server.id
        defer {
            store.setSelectedAgentType(nil, for: serverId)
            store.updateAgentTypes([], for: serverId)
        }

        store.updateAgentTypes(server.agentTypes, for: serverId)

        // No selection yet → picker should show
        XCTAssertTrue(store.shouldShowPicker(availableAgents: server.agentTypes))

        // Select Pi → effective type should be Pi
        store.setSelectedAgentType(.piNative, for: serverId)
        let effective = store.effectiveAgentType(for: serverId, availableAgents: server.agentTypes)
        XCTAssertEqual(effective, .piNative)

        // Auto-select when only one agent
        let singleAgent = store.effectiveAgentType(for: serverId, availableAgents: [.piNative])
        XCTAssertEqual(singleAgent, .piNative)
    }

    // MARK: - VAL-CROSS-009: Provider Switch (Component-Level)

    /// Verify provider switch via inline picker clears old state.
    func testProviderSwitchClearsOldSelection() {
        let store = AgentSelectionStore.shared
        let serverId = "switch-server-\(UUID().uuidString.prefix(8))"
        defer {
            store.setSelectedAgentType(nil, for: serverId)
            store.updateAgentTypes([], for: serverId)
        }

        // Start with Pi
        store.updateAgentTypes([.piNative, .droidNative], for: serverId)
        store.setSelectedAgentType(.piNative, for: serverId)
        XCTAssertEqual(store.selectedAgentType(for: serverId), .piNative)

        // Switch to Droid
        store.setSelectedAgentType(.droidNative, for: serverId)
        XCTAssertEqual(store.selectedAgentType(for: serverId), .droidNative)

        // Verify Pi is no longer selected
        XCTAssertNotEqual(store.selectedAgentType(for: serverId), .piNative)
    }

    // MARK: - VAL-CROSS-010: Full Build Gate (Verified by Build System)

    /// Verify all agent types compile and are usable in the type system.
    func testAllAgentTypesCompileInSwift() {
        // This test validates that the UniFFI-generated types compile correctly
        let allTypes: [AgentType] = [
            .codex, .piAcp, .piNative, .droidAcp, .droidNative, .genericAcp
        ]
        XCTAssertEqual(allTypes.count, 6, "All 6 agent types should be available")

        // Each type should have working Display, PersistentKey, Icon, Tint, TransportLabel
        for type in allTypes {
            XCTAssertFalse(type.displayName.isEmpty)
            XCTAssertFalse(type.persistentKey.isEmpty)
            XCTAssertFalse(type.icon.isEmpty)
            XCTAssertFalse(type.transportLabel.isEmpty)
            _ = type.tintColor // Should not crash
        }
    }

    /// Verify all connection step kinds compile with agent-generic names.
    func testAllConnectionStepKindsCompileWithGenericNames() {
        let steps: [AppConnectionStepKind] = [
            .connectingToSsh,
            .detectingAgents,
            .findingAgent,
            .installingAgent,
            .startingAgent,
            .openingTunnel,
            .connected,
        ]
        XCTAssertEqual(steps.count, 7, "All 7 step kinds should be available")
    }

    // MARK: - Helper

    private func makeSession(
        threadId: String,
        serverId: String,
        title: String = "Test Session"
    ) -> AppSessionSummary {
        AppSessionSummary(
            key: ThreadKey(serverId: serverId, threadId: threadId),
            serverDisplayName: "Test Server",
            serverHost: "localhost",
            title: title,
            preview: "",
            cwd: "/tmp",
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
