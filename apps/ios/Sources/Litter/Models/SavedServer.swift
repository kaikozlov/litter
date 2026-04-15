import Foundation

struct SavedServer: Codable, Identifiable, Equatable {
    let id: String
    let name: String
    let hostname: String
    let port: UInt16?
    let agentPorts: [UInt16]
    let sshPort: UInt16?
    let source: ServerSource
    let hasAgentServer: Bool
    let wakeMAC: String?
    let preferredConnectionMode: PreferredConnectionMode?
    let preferredAgentPort: UInt16?
    let sshPortForwardingEnabled: Bool?
    let websocketURL: String?
    let rememberedByUser: Bool

    init(
        id: String,
        name: String,
        hostname: String,
        port: UInt16?,
        agentPorts: [UInt16],
        sshPort: UInt16?,
        source: ServerSource,
        hasAgentServer: Bool,
        wakeMAC: String?,
        preferredConnectionMode: PreferredConnectionMode?,
        preferredAgentPort: UInt16?,
        sshPortForwardingEnabled: Bool?,
        websocketURL: String?,
        rememberedByUser: Bool = false
    ) {
        self.id = id
        self.name = name
        self.hostname = hostname
        self.port = port
        self.agentPorts = agentPorts
        self.sshPort = sshPort
        self.source = source
        self.hasAgentServer = hasAgentServer
        self.wakeMAC = wakeMAC
        self.preferredConnectionMode = preferredConnectionMode
        self.preferredAgentPort = preferredAgentPort
        self.sshPortForwardingEnabled = sshPortForwardingEnabled
        self.websocketURL = websocketURL
        self.rememberedByUser = rememberedByUser
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case name
        case hostname
        case port
        case agentPorts
        case sshPort
        case source
        case hasAgentServer
        case wakeMAC
        case preferredConnectionMode
        case preferredAgentPort
        case sshPortForwardingEnabled
        case websocketURL
        case rememberedByUser
        // Legacy keys for backward compatibility
        case codexPorts
        case hasCodexServer
        case preferredCodexPort
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let port = try container.decodeIfPresent(UInt16.self, forKey: .port)

        // Decode hasAgentServer, falling back to legacy hasCodexServer
        let hasAgentServer: Bool
        if container.contains(.hasAgentServer) {
            hasAgentServer = try container.decode(Bool.self, forKey: .hasAgentServer)
        } else {
            hasAgentServer = try container.decode(Bool.self, forKey: .hasCodexServer)
        }

        self.id = try container.decode(String.self, forKey: .id)
        self.name = try container.decode(String.self, forKey: .name)
        self.hostname = try container.decode(String.self, forKey: .hostname)
        self.port = port

        // Decode agentPorts, falling back to legacy codexPorts
        if container.contains(.agentPorts) {
            self.agentPorts = try container.decodeIfPresent([UInt16].self, forKey: .agentPorts)
                ?? (hasAgentServer ? (port.map { [$0] } ?? []) : [])
        } else {
            self.agentPorts = try container.decodeIfPresent([UInt16].self, forKey: .codexPorts)
                ?? (hasAgentServer ? (port.map { [$0] } ?? []) : [])
        }

        self.sshPort = try container.decodeIfPresent(UInt16.self, forKey: .sshPort)
        self.source = try container.decode(ServerSource.self, forKey: .source)
        self.hasAgentServer = hasAgentServer
        self.wakeMAC = try container.decodeIfPresent(String.self, forKey: .wakeMAC)
        self.preferredConnectionMode = try container.decodeIfPresent(
            PreferredConnectionMode.self,
            forKey: .preferredConnectionMode
        )

        // Decode preferredAgentPort, falling back to legacy preferredCodexPort
        if container.contains(.preferredAgentPort) {
            self.preferredAgentPort = try container.decodeIfPresent(UInt16.self, forKey: .preferredAgentPort)
        } else {
            self.preferredAgentPort = try container.decodeIfPresent(UInt16.self, forKey: .preferredCodexPort)
        }

        self.sshPortForwardingEnabled = try container.decodeIfPresent(
            Bool.self,
            forKey: .sshPortForwardingEnabled
        )
        self.websocketURL = try container.decodeIfPresent(String.self, forKey: .websocketURL)
        self.rememberedByUser = try container.decodeIfPresent(Bool.self, forKey: .rememberedByUser) ?? true
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(name, forKey: .name)
        try container.encode(hostname, forKey: .hostname)
        try container.encodeIfPresent(port, forKey: .port)
        try container.encode(agentPorts, forKey: .agentPorts)
        try container.encodeIfPresent(sshPort, forKey: .sshPort)
        try container.encode(source, forKey: .source)
        try container.encode(hasAgentServer, forKey: .hasAgentServer)
        try container.encodeIfPresent(wakeMAC, forKey: .wakeMAC)
        try container.encodeIfPresent(preferredConnectionMode, forKey: .preferredConnectionMode)
        try container.encodeIfPresent(preferredAgentPort, forKey: .preferredAgentPort)
        try container.encodeIfPresent(sshPortForwardingEnabled, forKey: .sshPortForwardingEnabled)
        try container.encodeIfPresent(websocketURL, forKey: .websocketURL)
        try container.encode(rememberedByUser, forKey: .rememberedByUser)
    }

    func toDiscoveredServer() -> DiscoveredServer {
        let agentPort = hasAgentServer ? (preferredAgentPort ?? port) : nil
        let resolvedSshPort = sshPort ?? (hasAgentServer ? nil : port)
        return DiscoveredServer(
            id: id,
            name: name,
            hostname: hostname,
            port: agentPort,
            agentPorts: resolvedAgentPorts,
            sshPort: resolvedSshPort,
            source: source,
            hasAgentServer: hasAgentServer,
            wakeMAC: wakeMAC,
            sshPortForwardingEnabled: false,
            websocketURL: websocketURL,
            preferredConnectionMode: migratedPreferredConnectionMode,
            preferredAgentPort: preferredAgentPort
        )
    }

    static func from(_ server: DiscoveredServer, rememberedByUser: Bool = false) -> SavedServer {
        SavedServer(
            id: server.id,
            name: server.name,
            hostname: server.hostname,
            port: server.port,
            agentPorts: server.agentPorts,
            sshPort: server.sshPort,
            source: server.source,
            hasAgentServer: server.hasAgentServer,
            wakeMAC: server.wakeMAC,
            preferredConnectionMode: server.preferredConnectionMode,
            preferredAgentPort: server.preferredAgentPort,
            sshPortForwardingEnabled: nil,
            websocketURL: server.websocketURL,
            rememberedByUser: rememberedByUser
        )
    }

    private var resolvedAgentPorts: [UInt16] {
        if !agentPorts.isEmpty {
            return agentPorts
        }
        if let port, hasAgentServer {
            return [port]
        }
        return []
    }

    private var migratedPreferredConnectionMode: PreferredConnectionMode? {
        preferredConnectionMode ?? (sshPortForwardingEnabled == true ? .ssh : nil)
    }

    func toRecord() -> SavedServerRecord {
        SavedServerRecord(
            id: id,
            name: name,
            hostname: hostname,
            port: port ?? 0,
            agentPorts: agentPorts,
            sshPort: sshPort,
            source: source.rawValue,
            hasAgentServer: hasAgentServer,
            wakeMac: wakeMAC,
            preferredConnectionMode: preferredConnectionMode?.rawValue,
            preferredAgentPort: preferredAgentPort,
            sshPortForwardingEnabled: sshPortForwardingEnabled,
            websocketUrl: websocketURL,
            rememberedByUser: rememberedByUser
        )
    }
}
