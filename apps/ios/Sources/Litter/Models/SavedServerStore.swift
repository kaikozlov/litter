import Foundation

@MainActor
enum SavedServerStore {
    private static let savedServersKey = "codex_saved_servers"

    static func save(_ servers: [SavedServer]) {
        guard let data = try? JSONEncoder().encode(servers) else { return }
        UserDefaults.standard.set(data, forKey: savedServersKey)
    }

    static func load() -> [SavedServer] {
        guard let data = UserDefaults.standard.data(forKey: savedServersKey) else { return [] }
        let decoded = (try? JSONDecoder().decode([SavedServer].self, from: data)) ?? []
        let migrated = decoded.map { saved -> SavedServer in
            let server = saved.toDiscoveredServer()
            return SavedServer.from(server, rememberedByUser: saved.rememberedByUser)
        }
        if migrated != decoded {
            save(migrated)
        }
        return migrated
    }

    static func upsert(_ server: DiscoveredServer) {
        var saved = load()
        let existing = existingMatch(for: server, in: saved)
        saved.removeAll { entry in matches(server, entry) }
        saved.append(
            SavedServer.from(
                server,
                rememberedByUser: existing?.rememberedByUser ?? false
            )
        )
        save(saved)
    }

    static func remember(_ server: DiscoveredServer) {
        var saved = load()
        saved.removeAll { entry in matches(server, entry) }
        saved.append(SavedServer.from(server, rememberedByUser: true))
        save(saved)
    }

    static func rememberedServers() -> [SavedServer] {
        load().filter(\.rememberedByUser)
    }

    static func remove(serverId: String) {
        var saved = load()
        saved.removeAll { $0.id == serverId }
        save(saved)
    }

    static func rename(serverId: String, newName: String) {
        var saved = load()
        guard let index = saved.firstIndex(where: { $0.id == serverId }) else { return }
        let old = saved[index]
        saved[index] = SavedServer(
            id: old.id,
            name: newName,
            hostname: old.hostname,
            port: old.port,
            codexPorts: old.codexPorts,
            sshPort: old.sshPort,
            source: old.source,
            hasCodexServer: old.hasCodexServer,
            wakeMAC: old.wakeMAC,
            preferredConnectionMode: old.preferredConnectionMode,
            preferredCodexPort: old.preferredCodexPort,
            sshPortForwardingEnabled: old.sshPortForwardingEnabled,
            websocketURL: old.websocketURL,
            rememberedByUser: old.rememberedByUser
        )
        save(saved)
    }

    private static func existingMatch(for server: DiscoveredServer, in saved: [SavedServer]) -> SavedServer? {
        saved.first { matches(server, $0) }
    }

    private static func matches(_ server: DiscoveredServer, _ savedServer: SavedServer) -> Bool {
        savedServer.id == server.id || savedServer.toDiscoveredServer().deduplicationKey == server.deduplicationKey
    }
}
