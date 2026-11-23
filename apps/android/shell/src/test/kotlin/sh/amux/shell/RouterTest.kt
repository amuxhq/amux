package sh.amux.shell

import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.core.runtime.AgentKey
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.PairingInvitation

class RouterTest {
    private val hostId = HostId(UUID.fromString("11111111-2222-3333-4444-555555555555"))
    private val agentKey = AgentKey(hostId, UUID.fromString("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"))

    @Test
    fun perTabBackStacksAndReSelectPopToRoot() {
        val left = mutableListOf<Route>()
        val router = Router(
            listener = object : RouteListener {
                override fun onRouteLeft(route: Route) {
                    left += route
                }
            },
        )

        assertEquals(Tab.Agents, router.snapshot.tab)
        assertTrue(router.snapshot.showsBottomBar)

        router.push(Route.Conversation(agentKey))
        assertEquals(Route.Conversation(agentKey), router.snapshot.topRoute)
        assertFalse(router.snapshot.showsBottomBar)

        // Switching tabs preserves the Agents stack
        router.selectTab(Tab.Hosts)
        assertEquals(Tab.Hosts, router.snapshot.tab)
        assertTrue(router.snapshot.showsBottomBar)
        assertNull(router.snapshot.topRoute)

        // Pushing Host stays on Hosts tab
        router.push(Route.Host(hostId))
        assertEquals(Route.Host(hostId), router.snapshot.topRoute)

        // Switching back to Agents restores Conversation
        router.selectTab(Tab.Agents)
        assertEquals(Route.Conversation(agentKey), router.snapshot.topRoute)

        // Re-selecting Agents pops it to root and notifies listener
        router.selectTab(Tab.Agents)
        assertNull(router.snapshot.topRoute)
        assertEquals(listOf<Route>(Route.Conversation(agentKey)), left)
    }

    @Test
    fun pairingModalStepsAsideForPaywallAndReturns() {
        val router = Router()
        router.presentPairing(PairingModal.Code(hostId))
        assertTrue(router.snapshot.showsPairing)

        router.push(Route.Paywall(Tab.Agents))
        assertFalse(router.snapshot.showsPairing)
        assertNotNull(router.snapshot.pairing)

        assertTrue(router.pop())
        assertTrue(router.snapshot.showsPairing)
    }

    @Test
    fun leaveAccountClearsStacksAndCodePairingButKeepsLinkInvitation() {
        val router = Router()
        router.push(Route.Settled)
        router.presentPairing(PairingModal.Code(hostId))
        router.leaveAccount()

        assertTrue(router.snapshot.agentsStack.isEmpty())
        assertTrue(router.snapshot.hostsStack.isEmpty())
        assertNull(router.snapshot.pairing)

        val invitation = PairingInvitation(
            host = hostId,
            addrs = emptyList(),
            link = "https://amux.sh/p#abc",
        )
        router.presentPairing(PairingModal.Link(invitation))
        router.leaveAccount()
        assertEquals(PairingModal.Link(invitation), router.snapshot.pairing)
    }

    @Test
    fun leaveAccountKeepsSettingsOpenToShowTheSignIn() {
        val router = Router()
        router.push(Route.Settled)
        router.push(Route.Settings)
        router.leaveAccount()

        assertEquals(listOf<Route>(Route.Settings), router.snapshot.agentsStack)
        assertEquals(Route.Settings, router.snapshot.topRoute)
    }

    @Test
    fun popAndCloseBehaveDeterministically() {
        val router = Router()
        assertFalse(router.pop())
        router.push(Route.Settled)
        assertFalse(router.close(Route.Settings))
        assertTrue(router.close(Route.Settled))
        assertNull(router.snapshot.topRoute)
    }
}
