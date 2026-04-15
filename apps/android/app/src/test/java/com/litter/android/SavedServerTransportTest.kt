package com.litter.android

import com.litter.android.state.SavedServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SavedServerTransportTest {
    @Test
    fun agentAndSshDiscoveryRequiresChoiceUntilPreferenceIsSet() {
        val server =
            SavedServer(
                id = "server-1",
                name = "Studio",
                hostname = "192.168.1.203",
                port = 8390,
                agentPorts = listOf(8390),
                sshPort = 22,
                hasAgentServer = true,
            )

        assertFalse(server.prefersSshConnection)
        assertTrue(server.requiresConnectionChoice)
        assertNull(server.directAgentPort)
    }

    @Test
    fun sshPreferenceForcesSshTransport() {
        val server =
            SavedServer(
                id = "server-2",
                name = "SSH Tunnel",
                hostname = "10.0.0.5",
                port = 8390,
                agentPorts = listOf(8390),
                sshPort = 22,
                hasAgentServer = true,
                preferredConnectionMode = "ssh",
            )

        assertTrue(server.prefersSshConnection)
        assertNull(server.directAgentPort)
        assertEquals(22, server.resolvedSshPort)
    }

    @Test
    fun legacyForwardingFlagMigratesToSshPreference() {
        val server =
            SavedServer(
                id = "server-3",
                name = "Old Saved Host",
                hostname = "192.168.1.203",
                port = 8390,
                agentPorts = listOf(8390),
                sshPort = 22,
                hasAgentServer = true,
                sshPortForwardingEnabled = true,
            )

        assertTrue(server.prefersSshConnection)
        assertNull(server.directAgentPort)
        assertEquals(22, server.resolvedSshPort)
    }

    @Test
    fun agentOnlyHostUsesDirectTransport() {
        val server =
            SavedServer(
                id = "server-4",
                name = "Agent",
                hostname = "10.0.0.4",
                port = 9234,
                agentPorts = listOf(9234),
                hasAgentServer = true,
            )

        assertFalse(server.prefersSshConnection)
        assertEquals(9234, server.directAgentPort)
    }
}
