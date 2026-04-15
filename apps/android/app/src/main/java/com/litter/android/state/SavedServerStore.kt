package com.litter.android.state

import android.content.Context
import android.content.SharedPreferences
import org.json.JSONArray
import org.json.JSONObject
import uniffi.codex_mobile_client.AppDiscoveredServer
import uniffi.codex_mobile_client.AppDiscoverySource
import uniffi.codex_mobile_client.SavedServerRecord

/**
 * Persistent server list stored in SharedPreferences.
 * Platform-specific — cannot live in Rust.
 */
data class SavedServer(
    val id: String,
    val name: String,
    val hostname: String,
    val port: Int,
    val agentPorts: List<Int> = emptyList(),
    val sshPort: Int? = null,
    val source: String = "manual", // local, bonjour, tailscale, lanProbe, arpScan, ssh, manual
    val hasAgentServer: Boolean = false,
    val wakeMAC: String? = null,
    val preferredConnectionMode: String? = null, // direct or ssh
    val preferredAgentPort: Int? = null,
    val sshPortForwardingEnabled: Boolean? = null, // legacy migration only
    val websocketURL: String? = null,
    val os: String? = null,
    val sshBanner: String? = null,
    val rememberedByUser: Boolean = false,
) {
    /** Stable key for deduplication across discovery cycles. */
    val deduplicationKey: String
        get() = websocketURL ?: normalizedHostKey(hostname)

    private fun normalizedHostKey(host: String): String {
        val trimmed = host.trim().trimStart('[').trimEnd(']')
        val withoutScope = if (!trimmed.contains(":")) {
            trimmed.substringBefore('%')
        } else {
            trimmed
        }
        return withoutScope.lowercase()
    }

    fun toJson(): JSONObject = JSONObject().apply {
        put("id", id)
        put("name", name)
        put("hostname", hostname)
        put("port", port)
        put("agentPorts", JSONArray(availableDirectAgentPorts))
        sshPort?.let { put("sshPort", it) }
        put("source", source)
        put("hasAgentServer", hasAgentServer)
        wakeMAC?.let { put("wakeMAC", it) }
        preferredConnectionMode?.let { put("preferredConnectionMode", it) }
        preferredAgentPort?.let { put("preferredAgentPort", it) }
        sshPortForwardingEnabled?.let { put("sshPortForwardingEnabled", it) }
        websocketURL?.let { put("websocketURL", it) }
        os?.let { put("os", it) }
        sshBanner?.let { put("sshBanner", it) }
        put("rememberedByUser", rememberedByUser)
    }

    val availableDirectAgentPorts: List<Int>
        get() {
            val ordered = buildList {
                if (hasAgentServer && port > 0) add(port)
                addAll(agentPorts.filter { it > 0 })
            }
            return ordered.distinct()
        }

    val resolvedPreferredConnectionMode: String?
        get() = when (preferredConnectionMode) {
            "direct", "directCodex" -> if (availableDirectAgentPorts.isNotEmpty() || websocketURL != null) "direct" else null
            "ssh" -> if (canConnectViaSsh) "ssh" else null
            else -> if (sshPortForwardingEnabled == true) "ssh" else null
        }

    val prefersSshConnection: Boolean
        get() = resolvedPreferredConnectionMode == "ssh"

    val canConnectViaSsh: Boolean
        get() = websocketURL == null && (
            sshPort != null ||
                source == "ssh" ||
                (!hasAgentServer && resolvedSshPort > 0) ||
                preferredConnectionMode == "ssh" ||
                sshPortForwardingEnabled == true
        )

    val resolvedSshPort: Int
        get() = sshPort ?: port.takeIf { !hasAgentServer && it > 0 } ?: 22

    val resolvedPreferredAgentPort: Int?
        get() = when {
            resolvedPreferredConnectionMode != "direct" -> null
            preferredAgentPort != null && availableDirectAgentPorts.contains(preferredAgentPort) -> preferredAgentPort
            else -> null
        }

    val requiresConnectionChoice: Boolean
        get() = websocketURL == null &&
            resolvedPreferredConnectionMode == null &&
            (
                availableDirectAgentPorts.size > 1 ||
                    (availableDirectAgentPorts.isNotEmpty() && canConnectViaSsh)
            )

    val directAgentPort: Int?
        get() = when {
            websocketURL != null -> null
            prefersSshConnection -> null
            resolvedPreferredAgentPort != null -> resolvedPreferredAgentPort
            requiresConnectionChoice -> null
            availableDirectAgentPorts.isNotEmpty() -> availableDirectAgentPorts.first()
            else -> null
        }

    fun withPreferredConnection(mode: String?, agentPortValue: Int? = null): SavedServer =
        copy(
            port = when (mode) {
                "direct" -> agentPortValue ?: directAgentPort ?: availableDirectAgentPorts.firstOrNull() ?: port
                "ssh" -> resolvedSshPort
                else -> port
            },
            agentPorts = availableDirectAgentPorts,
            sshPort = sshPort ?: if (canConnectViaSsh) resolvedSshPort else null,
            preferredConnectionMode = mode,
            preferredAgentPort = if (mode == "direct") {
                agentPortValue ?: directAgentPort ?: availableDirectAgentPorts.firstOrNull()
            } else {
                null
            },
            sshPortForwardingEnabled = null,
        )

    fun normalizedForPersistence(): SavedServer = withPreferredConnection(
        mode = resolvedPreferredConnectionMode,
        agentPortValue = resolvedPreferredAgentPort ?: availableDirectAgentPorts.firstOrNull(),
    )

    companion object {
        fun normalizeWakeMac(raw: String?): String? {
            val compact = raw
                ?.trim()
                ?.replace(":", "")
                ?.replace("-", "")
                ?.lowercase()
                ?: return null
            if (compact.length != 12 || compact.any { !it.isDigit() && it !in 'a'..'f' }) {
                return null
            }
            return buildString {
                compact.chunked(2).forEachIndexed { index, chunk ->
                    if (index > 0) append(':')
                    append(chunk)
                }
            }
        }

        fun fromJson(obj: JSONObject): SavedServer = SavedServer(
            id = obj.getString("id"),
            name = obj.optString("name", ""),
            hostname = obj.optString("hostname", ""),
            port = obj.optInt("port", 0),
            agentPorts = buildList {
                val ports = obj.optJSONArray("agentPorts") ?: obj.optJSONArray("codexPorts")
                if (ports != null) {
                    for (index in 0 until ports.length()) {
                        add(ports.optInt(index))
                    }
                }
            },
            sshPort = if (obj.has("sshPort")) obj.getInt("sshPort") else null,
            source = obj.optString("source", "manual"),
            hasAgentServer = obj.optBoolean("hasAgentServer", obj.optBoolean("hasCodexServer", false)),
            wakeMAC = if (obj.has("wakeMAC")) obj.getString("wakeMAC") else null,
            preferredConnectionMode = obj.optString("preferredConnectionMode").ifBlank { null }
                ?.let { mode ->
                    // Migrate legacy "directCodex" to "direct"
                    if (mode == "directCodex") "direct" else mode
                },
            preferredAgentPort = if (obj.has("preferredAgentPort")) {
                obj.getInt("preferredAgentPort")
            } else if (obj.has("preferredCodexPort")) {
                obj.getInt("preferredCodexPort")
            } else {
                null
            },
            sshPortForwardingEnabled = if (obj.has("sshPortForwardingEnabled")) {
                obj.optBoolean("sshPortForwardingEnabled")
            } else {
                null
            },
            websocketURL = if (obj.has("websocketURL")) obj.getString("websocketURL") else null,
            os = if (obj.has("os")) obj.getString("os") else null,
            sshBanner = if (obj.has("sshBanner")) obj.getString("sshBanner") else null,
            rememberedByUser = if (obj.has("rememberedByUser")) {
                obj.optBoolean("rememberedByUser")
            } else {
                true
            },
        )

        fun from(server: AppDiscoveredServer): SavedServer = SavedServer(
            id = server.id,
            name = server.displayName,
            hostname = server.host,
            port = server.agentPort?.toInt() ?: server.port.toInt(),
            agentPorts = server.agentPorts.map { it.toInt() },
            sshPort = server.sshPort?.toInt(),
            source = when (server.source) {
                AppDiscoverySource.BONJOUR -> "bonjour"
                AppDiscoverySource.TAILSCALE -> "tailscale"
                AppDiscoverySource.LAN_PROBE -> "lanProbe"
                AppDiscoverySource.ARP_SCAN -> "arpScan"
                AppDiscoverySource.MANUAL -> "manual"
                AppDiscoverySource.LOCAL -> "local"
            },
            hasAgentServer = server.agentPort != null || server.agentPorts.isNotEmpty(),
            os = if (server.sshBanner != null) server.os else server.os,
            sshBanner = server.sshBanner,
        )
    }
}

fun SavedServer.toRecord() = SavedServerRecord(
    id = id,
    name = name,
    hostname = hostname,
    port = port.toUShort(),
    agentPorts = agentPorts.map { it.toUShort() },
    sshPort = sshPort?.toUShort(),
    source = source,
    hasAgentServer = hasAgentServer,
    wakeMac = wakeMAC,
    preferredConnectionMode = preferredConnectionMode,
    preferredAgentPort = preferredAgentPort?.toUShort(),
    sshPortForwardingEnabled = sshPortForwardingEnabled,
    websocketUrl = websocketURL,
    rememberedByUser = rememberedByUser,
)

object SavedServerStore {
    private const val PREFS_NAME = "codex_saved_servers_prefs"
    private const val KEY = "codex_saved_servers"

    private fun prefs(context: Context): SharedPreferences =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    fun load(context: Context): List<SavedServer> {
        val json = prefs(context).getString(KEY, null) ?: return emptyList()
        return try {
            val array = JSONArray(json)
            val decoded = (0 until array.length()).map { SavedServer.fromJson(array.getJSONObject(it)) }
            val migrated = decoded.map { it.normalizedForPersistence() }
            if (decoded != migrated) {
                save(context, migrated)
            }
            migrated
        } catch (_: Exception) {
            emptyList()
        }
    }

    fun save(context: Context, servers: List<SavedServer>) {
        val array = JSONArray()
        servers.forEach { array.put(it.toJson()) }
        prefs(context).edit().putString(KEY, array.toString()).apply()
    }

    fun upsert(context: Context, server: SavedServer) {
        val existing = load(context).toMutableList()
        val prior = existing.firstOrNull { it.id == server.id || it.deduplicationKey == server.deduplicationKey }
        existing.removeAll { it.id == server.id || it.deduplicationKey == server.deduplicationKey }
        existing.add(server.copy(rememberedByUser = prior?.rememberedByUser ?: server.rememberedByUser))
        save(context, existing)
    }

    fun remember(context: Context, server: SavedServer) {
        val existing = load(context).toMutableList()
        existing.removeAll { it.id == server.id || it.deduplicationKey == server.deduplicationKey }
        existing.add(server.copy(rememberedByUser = true))
        save(context, existing)
    }

    fun remembered(context: Context): List<SavedServer> =
        load(context).filter { it.rememberedByUser }

    fun remove(context: Context, serverId: String) {
        val existing = load(context).toMutableList()
        existing.removeAll { it.id == serverId }
        save(context, existing)
    }

    fun rename(context: Context, serverId: String, newName: String) {
        val trimmed = newName.trim()
        if (trimmed.isEmpty()) return

        val existing = load(context)
        val renamed = existing.map { server ->
            if (server.id == serverId) server.copy(name = trimmed) else server
        }
        if (renamed != existing) {
            save(context, renamed)
        }
    }

    fun updateWakeMac(context: Context, serverId: String, host: String, wakeMac: String?) {
        val normalizedWakeMac = SavedServer.normalizeWakeMac(wakeMac) ?: return
        val existing = load(context)
        val updated = existing.map { server ->
            if (server.id == serverId || normalizedHostKey(server.hostname) == normalizedHostKey(host)) {
                if (server.wakeMAC != normalizedWakeMac) server.copy(wakeMAC = normalizedWakeMac) else server
            } else {
                server
            }
        }
        if (updated != existing) {
            save(context, updated)
        }
    }

    private fun normalizedHostKey(host: String): String {
        val trimmed = host.trim().trimStart('[').trimEnd(']').replace("%25", "%")
        val withoutScope = if (!trimmed.contains(":")) {
            trimmed.substringBefore('%')
        } else {
            trimmed
        }
        return withoutScope.lowercase()
    }
}
