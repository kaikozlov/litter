import Foundation

// MARK: - ACP Profile Model

/// Represents a saved ACP provider profile that the user can connect to
/// via the GenericAcp transport. Profiles capture the remote command needed
/// to launch an ACP-compatible agent over SSH, along with display metadata
/// and permission policy.
///
/// Profiles are persisted to UserDefaults via ``ACPProfileStore`` and are
/// Codable for JSON serialization. They integrate with the Rust layer
/// through the ``AgentPermissionPolicy`` UniFFI type for cross-platform
/// permission consistency.
struct ACPProfile: Codable, Identifiable, Equatable {
    /// Stable unique identifier for this profile.
    let id: UUID

    /// User-visible name (e.g., "My Custom Agent", "Work Claude").
    var displayName: String

    /// The shell command to spawn the ACP agent over SSH
    /// (e.g., `"my-agent --acp"`, `"npx some-acp-server"`).
    var remoteCommand: String

    /// Default permission policy for sessions created with this profile.
    var permissionPolicy: AgentPermissionPolicy

    /// Optional SF Symbol name for a custom icon (e.g., "star.fill", "cpu").
    /// When nil, the default gearshape.fill icon is used.
    var icon: String?

    init(
        id: UUID = UUID(),
        displayName: String,
        remoteCommand: String,
        permissionPolicy: AgentPermissionPolicy = .promptAlways,
        icon: String? = nil
    ) {
        self.id = id
        self.displayName = displayName
        self.remoteCommand = remoteCommand
        self.permissionPolicy = permissionPolicy
        self.icon = icon
    }
}

// MARK: - ACP Profile Store

/// Manages persistence of ``ACPProfile`` instances via UserDefaults.
///
/// Provides CRUD operations for ACP provider profiles. Profiles survive
/// across app launches and are used by the agent picker to surface
/// GenericAcp connection options.
///
/// Usage:
/// ```swift
/// let store = ACPProfileStore.shared
/// let profile = ACPProfile(displayName: "My Agent", remoteCommand: "my-agent --acp")
/// store.addProfile(profile)
/// let profiles = store.profiles()
/// ```
@MainActor
final class ACPProfileStore {
    static let shared = ACPProfileStore()

    private let defaults = UserDefaults.standard
    private static let profilesKey = "litter.acpProfiles"

    private init() {}

    // MARK: - Read

    /// Returns all saved ACP profiles, ordered by insertion time.
    func profiles() -> [ACPProfile] {
        guard let data = defaults.data(forKey: Self.profilesKey) else { return [] }
        return (try? JSONDecoder().decode([ACPProfile].self, from: data)) ?? []
    }

    /// Returns the profile with the given ID, if it exists.
    func profile(id: UUID) -> ACPProfile? {
        profiles().first { $0.id == id }
    }

    // MARK: - Create

    /// Appends a new profile and persists the change.
    func addProfile(_ profile: ACPProfile) {
        var all = profiles()
        all.append(profile)
        persist(all)
    }

    // MARK: - Update

    /// Updates an existing profile by ID and persists the change.
    /// Does nothing if no profile with the given ID exists.
    func updateProfile(_ profile: ACPProfile) {
        var all = profiles()
        guard let index = all.firstIndex(where: { $0.id == profile.id }) else { return }
        all[index] = profile
        persist(all)
    }

    // MARK: - Delete

    /// Removes the profile with the given ID and persists the change.
    func deleteProfile(id: UUID) {
        var all = profiles()
        all.removeAll { $0.id == id }
        persist(all)
    }

    // MARK: - Persistence

    private func persist(_ profiles: [ACPProfile]) {
        guard let data = try? JSONEncoder().encode(profiles) else { return }
        defaults.set(data, forKey: Self.profilesKey)
    }
}
