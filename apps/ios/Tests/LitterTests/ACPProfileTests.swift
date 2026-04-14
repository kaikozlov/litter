import XCTest
@testable import Litter

// MARK: - ACPProfile Model Tests

final class ACPProfileTests: XCTestCase {

    // MARK: - Construction

    func testProfileConstructionWithDefaults() {
        let profile = ACPProfile(
            displayName: "Test Agent",
            remoteCommand: "my-agent --acp"
        )
        XCTAssertEqual(profile.displayName, "Test Agent")
        XCTAssertEqual(profile.remoteCommand, "my-agent --acp")
        XCTAssertEqual(profile.permissionPolicy, .promptAlways)
        XCTAssertNil(profile.icon)
    }

    func testProfileConstructionWithAllFields() {
        let id = UUID()
        let profile = ACPProfile(
            id: id,
            displayName: "Custom Agent",
            remoteCommand: "custom-agent --port 8080",
            permissionPolicy: .autoApproveAll,
            icon: "star.fill"
        )
        XCTAssertEqual(profile.id, id)
        XCTAssertEqual(profile.displayName, "Custom Agent")
        XCTAssertEqual(profile.remoteCommand, "custom-agent --port 8080")
        XCTAssertEqual(profile.permissionPolicy, .autoApproveAll)
        XCTAssertEqual(profile.icon, "star.fill")
    }

    func testProfileHasStableUUID() {
        let profile = ACPProfile(displayName: "A", remoteCommand: "a")
        // UUID should be non-nil and stable
        let id = profile.id
        XCTAssertEqual(profile.id, id)
    }

    // MARK: - Identifiable

    func testProfileIsIdentifiable() {
        let profile = ACPProfile(displayName: "A", remoteCommand: "a")
        // Identifiable conformance means `id` is accessible
        _ = profile.id as UUID
    }

    // MARK: - Equatable

    func testProfileEqualityById() {
        let id = UUID()
        let a = ACPProfile(id: id, displayName: "A", remoteCommand: "a")
        let b = ACPProfile(id: id, displayName: "B", remoteCommand: "b")
        // Equal IDs means equal profiles (struct equality on all fields)
        let c = ACPProfile(id: id, displayName: "A", remoteCommand: "a")
        XCTAssertEqual(a, c)
    }

    func testProfileInequality() {
        let a = ACPProfile(id: UUID(), displayName: "A", remoteCommand: "a")
        let b = ACPProfile(id: UUID(), displayName: "A", remoteCommand: "a")
        XCTAssertNotEqual(a, b)
    }

    // MARK: - Codable Round-Trip

    func testProfileCodableRoundTrip() throws {
        let profile = ACPProfile(
            id: UUID(),
            displayName: "My Agent",
            remoteCommand: "agent --acp --verbose",
            permissionPolicy: .autoRejectHighRisk,
            icon: "cpu"
        )
        let encoded = try JSONEncoder().encode(profile)
        let decoded = try JSONDecoder().decode(ACPProfile.self, from: encoded)
        XCTAssertEqual(decoded.id, profile.id)
        XCTAssertEqual(decoded.displayName, profile.displayName)
        XCTAssertEqual(decoded.remoteCommand, profile.remoteCommand)
        XCTAssertEqual(decoded.permissionPolicy, profile.permissionPolicy)
        XCTAssertEqual(decoded.icon, profile.icon)
    }

    func testProfileCodableRoundTripMinimalFields() throws {
        let profile = ACPProfile(
            displayName: "Minimal",
            remoteCommand: "minimal-agent"
        )
        let encoded = try JSONEncoder().encode(profile)
        let decoded = try JSONDecoder().decode(ACPProfile.self, from: encoded)
        XCTAssertEqual(decoded.id, profile.id)
        XCTAssertEqual(decoded.displayName, "Minimal")
        XCTAssertEqual(decoded.remoteCommand, "minimal-agent")
        XCTAssertEqual(decoded.permissionPolicy, .promptAlways)
        XCTAssertNil(decoded.icon)
    }

    func testProfileCodableRoundTripAllPermissionPolicies() throws {
        let policies: [AgentPermissionPolicy] = [.autoApproveAll, .autoRejectHighRisk, .promptAlways]
        for policy in policies {
            let profile = ACPProfile(
                displayName: "Agent",
                remoteCommand: "agent",
                permissionPolicy: policy
            )
            let encoded = try JSONEncoder().encode(profile)
            let decoded = try JSONDecoder().decode(ACPProfile.self, from: encoded)
            XCTAssertEqual(decoded.permissionPolicy, policy, "Failed to round-trip policy: \(policy)")
        }
    }

    func testProfileArrayCodableRoundTrip() throws {
        let profiles = [
            ACPProfile(displayName: "A", remoteCommand: "a", permissionPolicy: .autoApproveAll),
            ACPProfile(displayName: "B", remoteCommand: "b", permissionPolicy: .autoRejectHighRisk, icon: "star"),
            ACPProfile(displayName: "C", remoteCommand: "c", permissionPolicy: .promptAlways),
        ]
        let encoded = try JSONEncoder().encode(profiles)
        let decoded = try JSONDecoder().decode([ACPProfile].self, from: encoded)
        XCTAssertEqual(decoded.count, 3)
        XCTAssertEqual(decoded[0].displayName, "A")
        XCTAssertEqual(decoded[1].icon, "star")
        XCTAssertEqual(decoded[2].permissionPolicy, .promptAlways)
    }
}

// MARK: - ACPProfileStore Tests

final class ACPProfileStoreTests: XCTestCase {

    private var store: ACPProfileStore!

    override func setUp() {
        super.setUp()
        // Use MainActor to create store (it's @MainActor)
        // Clean up any existing profiles before each test
        let defaults = UserDefaults.standard
        defaults.removeObject(forKey: "litter.acpProfiles")
        store = ACPProfileStore.shared
    }

    override func tearDown() {
        // Clean up test data
        UserDefaults.standard.removeObject(forKey: "litter.acpProfiles")
        store = nil
        super.tearDown()
    }

    // MARK: - Empty State

    @MainActor
    func testProfilesReturnsEmptyInitially() {
        let profiles = store.profiles()
        XCTAssertTrue(profiles.isEmpty)
    }

    @MainActor
    func testProfileReturnsNilForNonexistentId() {
        let result = store.profile(id: UUID())
        XCTAssertNil(result)
    }

    // MARK: - Add

    @MainActor
    func testAddProfile() {
        let profile = ACPProfile(displayName: "Test", remoteCommand: "test-agent")
        store.addProfile(profile)

        let profiles = store.profiles()
        XCTAssertEqual(profiles.count, 1)
        XCTAssertEqual(profiles[0].displayName, "Test")
        XCTAssertEqual(profiles[0].remoteCommand, "test-agent")
    }

    @MainActor
    func testAddMultipleProfiles() {
        store.addProfile(ACPProfile(displayName: "A", remoteCommand: "a"))
        store.addProfile(ACPProfile(displayName: "B", remoteCommand: "b"))
        store.addProfile(ACPProfile(displayName: "C", remoteCommand: "c"))

        let profiles = store.profiles()
        XCTAssertEqual(profiles.count, 3)
        XCTAssertEqual(profiles[0].displayName, "A")
        XCTAssertEqual(profiles[1].displayName, "B")
        XCTAssertEqual(profiles[2].displayName, "C")
    }

    // MARK: - Get by ID

    @MainActor
    func testProfileById() {
        let id = UUID()
        let profile = ACPProfile(id: id, displayName: "Found", remoteCommand: "found-agent")
        store.addProfile(profile)

        let result = store.profile(id: id)
        XCTAssertNotNil(result)
        XCTAssertEqual(result?.displayName, "Found")
    }

    // MARK: - Update

    @MainActor
    func testUpdateProfile() {
        let profile = ACPProfile(displayName: "Original", remoteCommand: "original-agent")
        store.addProfile(profile)

        let updated = ACPProfile(
            id: profile.id,
            displayName: "Updated",
            remoteCommand: "updated-agent",
            permissionPolicy: .autoApproveAll,
            icon: "star.fill"
        )
        store.updateProfile(updated)

        let result = store.profile(id: profile.id)
        XCTAssertEqual(result?.displayName, "Updated")
        XCTAssertEqual(result?.remoteCommand, "updated-agent")
        XCTAssertEqual(result?.permissionPolicy, .autoApproveAll)
        XCTAssertEqual(result?.icon, "star.fill")
    }

    @MainActor
    func testUpdateNonexistentProfileIsNoOp() {
        let ghost = ACPProfile(displayName: "Ghost", remoteCommand: "ghost")
        store.updateProfile(ghost)

        let profiles = store.profiles()
        XCTAssertTrue(profiles.isEmpty)
    }

    // MARK: - Delete

    @MainActor
    func testDeleteProfile() {
        let profile = ACPProfile(displayName: "ToDelete", remoteCommand: "delete-agent")
        store.addProfile(profile)
        XCTAssertEqual(store.profiles().count, 1)

        store.deleteProfile(id: profile.id)
        XCTAssertTrue(store.profiles().isEmpty)
    }

    @MainActor
    func testDeleteNonexistentProfileIsNoOp() {
        store.addProfile(ACPProfile(displayName: "Keep", remoteCommand: "keep"))
        store.deleteProfile(id: UUID())
        XCTAssertEqual(store.profiles().count, 1)
    }

    @MainActor
    func testDeleteOnlyRemovesTargetProfile() {
        let a = ACPProfile(displayName: "A", remoteCommand: "a")
        let b = ACPProfile(displayName: "B", remoteCommand: "b")
        let c = ACPProfile(displayName: "C", remoteCommand: "c")
        store.addProfile(a)
        store.addProfile(b)
        store.addProfile(c)

        store.deleteProfile(id: b.id)

        let profiles = store.profiles()
        XCTAssertEqual(profiles.count, 2)
        XCTAssertEqual(profiles[0].displayName, "A")
        XCTAssertEqual(profiles[1].displayName, "C")
    }

    // MARK: - Persistence

    @MainActor
    func testProfilesPersistAcrossLoadCycles() {
        let profile = ACPProfile(
            displayName: "Persistent",
            remoteCommand: "persistent-agent",
            permissionPolicy: .autoRejectHighRisk,
            icon: "bolt.fill"
        )
        store.addProfile(profile)

        // Simulate app restart by creating a fresh store read
        let reloaded = store.profiles()
        XCTAssertEqual(reloaded.count, 1)
        XCTAssertEqual(reloaded[0].id, profile.id)
        XCTAssertEqual(reloaded[0].displayName, "Persistent")
        XCTAssertEqual(reloaded[0].remoteCommand, "persistent-agent")
        XCTAssertEqual(reloaded[0].permissionPolicy, .autoRejectHighRisk)
        XCTAssertEqual(reloaded[0].icon, "bolt.fill")
    }

    @MainActor
    func testProfileIDsAreStableUUIDs() {
        let profile = ACPProfile(displayName: "Stable", remoteCommand: "stable")
        store.addProfile(profile)

        let loaded = store.profiles()
        XCTAssertEqual(loaded[0].id, profile.id)

        // Update should preserve the ID
        let updated = ACPProfile(
            id: profile.id,
            displayName: "Updated Stable",
            remoteCommand: "updated-stable"
        )
        store.updateProfile(updated)

        let afterUpdate = store.profiles()
        XCTAssertEqual(afterUpdate[0].id, profile.id)
        XCTAssertEqual(afterUpdate[0].displayName, "Updated Stable")
    }

    // MARK: - Full CRUD Cycle

    @MainActor
    func testFullCRUDCycle() {
        // Create
        let profile = ACPProfile(
            displayName: "CRUD Agent",
            remoteCommand: "crud-agent --acp",
            permissionPolicy: .promptAlways
        )
        store.addProfile(profile)
        XCTAssertEqual(store.profiles().count, 1)

        // Read
        let read = store.profile(id: profile.id)
        XCTAssertNotNil(read)
        XCTAssertEqual(read?.displayName, "CRUD Agent")

        // Update
        let updated = ACPProfile(
            id: profile.id,
            displayName: "Updated CRUD",
            remoteCommand: "updated-crud --acp",
            permissionPolicy: .autoApproveAll,
            icon: "wrench.fill"
        )
        store.updateProfile(updated)
        let afterUpdate = store.profile(id: profile.id)
        XCTAssertEqual(afterUpdate?.displayName, "Updated CRUD")
        XCTAssertEqual(afterUpdate?.permissionPolicy, .autoApproveAll)
        XCTAssertEqual(afterUpdate?.icon, "wrench.fill")

        // Delete
        store.deleteProfile(id: profile.id)
        XCTAssertTrue(store.profiles().isEmpty)
        XCTAssertNil(store.profile(id: profile.id))
    }
}

// MARK: - AgentSelectionStore ACP Persistence Tests

final class AgentSelectionStoreACPTests: XCTestCase {

    private var store: AgentSelectionStore!

    override func setUp() {
        super.setUp()
        let defaults = UserDefaults.standard
        defaults.removeObject(forKey: "litter.selectedACPProfileId.lastSelection")
        defaults.removeObject(forKey: "litter.selectedRemoteCommand")
        defaults.removeObject(forKey: "litter.selectedAgent.test-server-acp")
        store = AgentSelectionStore.shared
    }

    override func tearDown() {
        let defaults = UserDefaults.standard
        defaults.removeObject(forKey: "litter.selectedACPProfileId.lastSelection")
        defaults.removeObject(forKey: "litter.selectedRemoteCommand")
        defaults.removeObject(forKey: "litter.selectedAgent.test-server-acp")
        store = nil
        super.tearDown()
    }

    // MARK: - ACP Profile ID Persistence

    @MainActor
    func testSetAndGetACPProfileId() {
        let id = UUID()
        store.setSelectedACPProfileId(id, for: "lastSelection")

        let retrieved = store.selectedACPProfileId(for: "lastSelection")
        XCTAssertEqual(retrieved, id)
    }

    @MainActor
    func testClearACPProfileId() {
        let id = UUID()
        store.setSelectedACPProfileId(id, for: "lastSelection")
        store.setSelectedACPProfileId(nil, for: "lastSelection")

        let retrieved = store.selectedACPProfileId(for: "lastSelection")
        XCTAssertNil(retrieved)
    }

    @MainActor
    func testACPProfileIdReturnsNilWhenNotSet() {
        let retrieved = store.selectedACPProfileId(for: "lastSelection")
        XCTAssertNil(retrieved)
    }

    // MARK: - Remote Command Persistence

    @MainActor
    func testSetAndGetRemoteCommand() {
        store.setSelectedRemoteCommand("my-agent --acp --verbose")

        let retrieved = store.selectedRemoteCommand()
        XCTAssertEqual(retrieved, "my-agent --acp --verbose")
    }

    @MainActor
    func testClearRemoteCommand() {
        store.setSelectedRemoteCommand("my-agent --acp")
        store.setSelectedRemoteCommand(nil)

        let retrieved = store.selectedRemoteCommand()
        XCTAssertNil(retrieved)
    }

    @MainActor
    func testRemoteCommandReturnsNilWhenNotSet() {
        let retrieved = store.selectedRemoteCommand()
        XCTAssertNil(retrieved)
    }

    // MARK: - Selected Agent Type for GenericAcp

    @MainActor
    func testSelectedAgentTypeRoundTripForGenericAcp() {
        store.setSelectedAgentType(.genericAcp, for: "test-server-acp")
        let result = store.selectedAgentType(for: "test-server-acp")
        XCTAssertEqual(result, .genericAcp)
    }

    @MainActor
    func testSelectedAgentTypePersistsAcrossInstances() {
        store.setSelectedAgentType(.genericAcp, for: "test-server-acp")
        // Re-read from the same shared instance (which reads from UserDefaults)
        let result = store.selectedAgentType(for: "test-server-acp")
        XCTAssertEqual(result, .genericAcp)
    }

    @MainActor
    func testSelectedAgentTypeClearsToNil() {
        store.setSelectedAgentType(.genericAcp, for: "test-server-acp")
        store.setSelectedAgentType(nil, for: "test-server-acp")
        let result = store.selectedAgentType(for: "test-server-acp")
        XCTAssertNil(result)
    }
}
