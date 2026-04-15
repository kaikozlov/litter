import Foundation

enum ServerSource: String, Codable, Hashable {
    case local
    case bonjour
    case ssh
    case tailscale
    case manual

    init(_ source: AppDiscoverySource) {
        switch source {
        case .bonjour, .lanProbe, .arpScan:
            self = .bonjour
        case .tailscale:
            self = .tailscale
        case .manual:
            self = .manual
        case .local:
            self = .local
        }
    }
}

enum PreferredConnectionMode: String, Codable, Hashable {
    case directAgent
    case ssh

    /// Backward compatibility: decode old `directCodex` value as `directAgent`.
    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        let rawValue = try container.decode(String.self)
        switch rawValue {
        case "directCodex", "directAgent":
            self = .directAgent
        case "ssh":
            self = .ssh
        default:
            self = .directAgent
        }
    }
}

struct DiscoveredServer: Identifiable, Hashable {
    let id: String
    let name: String
    let hostname: String
    let port: UInt16?
    let agentPorts: [UInt16]
    let sshPort: UInt16?
    let source: ServerSource
    let hasAgentServer: Bool
    let wakeMAC: String?
    let sshPortForwardingEnabled: Bool
    let websocketURL: String?
    let preferredConnectionMode: PreferredConnectionMode?
    let preferredAgentPort: UInt16?
    let os: String?
    let sshBanner: String?
    let agentTypes: [AgentType]
    let agentInfos: [AgentInfo]

    init(
        id: String,
        name: String,
        hostname: String,
        port: UInt16?,
        agentPorts: [UInt16] = [],
        sshPort: UInt16? = nil,
        source: ServerSource,
        hasAgentServer: Bool,
        wakeMAC: String? = nil,
        sshPortForwardingEnabled: Bool = false,
        websocketURL: String? = nil,
        preferredConnectionMode: PreferredConnectionMode? = nil,
        preferredAgentPort: UInt16? = nil,
        os: String? = nil,
        sshBanner: String? = nil,
        agentTypes: [AgentType] = [.codex],
        agentInfos: [AgentInfo] = []
    ) {
        let normalizedAgentPorts = Self.normalizedPorts(agentPorts, fallback: port)
        let resolvedPreferredMode = Self.resolvedPreferredConnectionMode(
            preferredConnectionMode,
            agentPorts: normalizedAgentPorts,
            sshPort: sshPort,
            websocketURL: websocketURL
        )
        let resolvedPreferredAgentPort = Self.resolvedPreferredAgentPort(
            preferredConnectionMode: resolvedPreferredMode,
            preferredAgentPort: preferredAgentPort,
            agentPorts: normalizedAgentPorts
        )

        self.id = id
        self.name = name
        self.hostname = hostname
        self.port = resolvedPreferredAgentPort
            ?? (normalizedAgentPorts.contains(port ?? 0) ? port : nil)
            ?? normalizedAgentPorts.first
        self.agentPorts = normalizedAgentPorts
        self.sshPort = sshPort
        self.source = source
        self.hasAgentServer = hasAgentServer || !normalizedAgentPorts.isEmpty || websocketURL != nil
        self.wakeMAC = Self.normalizeWakeMAC(wakeMAC)
        self.sshPortForwardingEnabled = sshPortForwardingEnabled
        self.websocketURL = websocketURL
        self.preferredConnectionMode = resolvedPreferredMode
        self.preferredAgentPort = resolvedPreferredAgentPort
        self.os = os
        self.sshBanner = sshBanner
        self.agentTypes = agentTypes
        self.agentInfos = agentInfos
    }

    var connectionTarget: ConnectionTarget? {
        if source == .local { return .local }
        if let websocketURL, let url = URL(string: websocketURL) { return .remoteURL(url) }
        if preferredConnectionMode == .ssh {
            return nil
        }
        if let port = resolvedDirectAgentPort, !requiresConnectionChoice {
            return .remote(host: hostname, port: port)
        }
        return nil
    }

    var resolvedSSHPort: UInt16 {
        sshPort ?? 22
    }

    var availableDirectAgentPorts: [UInt16] {
        agentPorts
    }

    var resolvedDirectAgentPort: UInt16? {
        if preferredConnectionMode == .directAgent, let preferredAgentPort {
            return preferredAgentPort
        }
        if let port, availableDirectAgentPorts.contains(port) {
            return port
        }
        return availableDirectAgentPorts.first
    }

    var canConnectViaSSH: Bool {
        sshPort != nil
    }

    var hasValidPreferredConnection: Bool {
        preferredConnectionMode != nil
    }

    var requiresConnectionChoice: Bool {
        guard source != .local, websocketURL == nil else { return false }
        guard preferredConnectionMode == nil else { return false }
        let directCount = availableDirectAgentPorts.count
        return directCount > 1 || (directCount > 0 && canConnectViaSSH)
    }

    func withConnectionPreference(
        _ mode: PreferredConnectionMode?,
        agentPort: UInt16? = nil
    ) -> DiscoveredServer {
        DiscoveredServer(
            id: id,
            name: name,
            hostname: hostname,
            port: agentPort ?? port,
            agentPorts: agentPorts,
            sshPort: sshPort,
            source: source,
            hasAgentServer: hasAgentServer,
            wakeMAC: wakeMAC,
            sshPortForwardingEnabled: sshPortForwardingEnabled,
            websocketURL: websocketURL,
            preferredConnectionMode: mode,
            preferredAgentPort: mode == .directAgent ? (agentPort ?? resolvedDirectAgentPort) : nil,
            os: os,
            sshBanner: sshBanner,
            agentTypes: agentTypes,
            agentInfos: agentInfos
        )
    }

    var deduplicationKey: String {
        if source == .local {
            return "local"
        }

        if let websocketURL, let url = URL(string: websocketURL) {
            let host = Self.normalizedHostKey(url.host ?? hostname)
            return host.isEmpty ? id : host
        }

        let host = Self.normalizedHostKey(hostname)
        return host.isEmpty ? id : host
    }

    static func normalizeWakeMAC(_ raw: String?) -> String? {
        guard let raw else { return nil }
        let compact = raw
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: ":", with: "")
            .replacingOccurrences(of: "-", with: "")
            .lowercased()
        guard compact.count == 12 else { return nil }
        guard compact.allSatisfy({ $0.isHexDigit }) else { return nil }
        var groups: [String] = []
        groups.reserveCapacity(6)
        var index = compact.startIndex
        for _ in 0..<6 {
            let next = compact.index(index, offsetBy: 2)
            groups.append(String(compact[index..<next]))
            index = next
        }
        return groups.joined(separator: ":")
    }

    private static func normalizedPorts(_ ports: [UInt16], fallback: UInt16?) -> [UInt16] {
        var ordered = [UInt16]()
        if let fallback {
            ordered.append(fallback)
        }
        ordered.append(contentsOf: ports)

        var seen = Set<UInt16>()
        return ordered.filter { seen.insert($0).inserted }
    }

    private static func resolvedPreferredConnectionMode(
        _ mode: PreferredConnectionMode?,
        agentPorts: [UInt16],
        sshPort: UInt16?,
        websocketURL: String?
    ) -> PreferredConnectionMode? {
        switch mode {
        case .directAgent:
            return !agentPorts.isEmpty || websocketURL != nil ? .directAgent : nil
        case .ssh:
            return sshPort != nil ? .ssh : nil
        case nil:
            return nil
        }
    }

    private static func resolvedPreferredAgentPort(
        preferredConnectionMode: PreferredConnectionMode?,
        preferredAgentPort: UInt16?,
        agentPorts: [UInt16]
    ) -> UInt16? {
        guard preferredConnectionMode == .directAgent else { return nil }
        guard let preferredAgentPort, agentPorts.contains(preferredAgentPort) else { return nil }
        return preferredAgentPort
    }

    private static func normalizedHostKey(_ raw: String) -> String {
        var normalized = raw
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
            .replacingOccurrences(of: "%25", with: "%")

        if !normalized.contains(":"), let scopeIndex = normalized.firstIndex(of: "%") {
            normalized = String(normalized[..<scopeIndex])
        }

        return normalized.lowercased()
    }
}
