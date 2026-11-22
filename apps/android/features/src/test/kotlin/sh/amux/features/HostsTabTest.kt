package sh.amux.features

import androidx.compose.ui.test.assertContentDescriptionContains
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.values.HostView
import sh.amux.values.Offline
import sh.amux.values.Platform
import sh.amux.values.Reach
import sh.amux.values.RouteKind

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class HostsTabTest {
    @get:Rule
    val composeRule = createComposeRule()

    private fun host(
        index: Int,
        name: String,
        reach: Reach = Reach.Online(RouteKind.Direct),
        trusted: Boolean = true,
        candidate: Boolean = false,
        pairingOpen: Boolean = false,
        sameNetwork: Boolean = false,
    ): HostView = HostView(
        hostId = List(16) { index },
        link = 0L,
        sameNetwork = sameNetwork,
        name = name,
        local = false,
        trusted = trusted,
        candidate = candidate,
        reach = reach,
        addrs = if (candidate) listOf("192.168.1.$index:4433") else emptyList(),
        relayOnly = false,
        pairingOpen = pairingOpen,
        current = true,
        providers = emptyList(),
        platform = Platform.Macos,
    )

    @Test
    fun rendersEmptyStateWhenNoHostsOrFound() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                HostsTab(state = HostsTabState(), onAction = {})
            }
        }
        composeRule.onNodeWithTag(TestTags.HOSTS_EMPTY).assertIsDisplayed()
        composeRule.onNodeWithText("No devices yet").assertIsDisplayed()
    }

    @Test
    fun rendersFoundCardsAboveReachGroupedPairedHosts() {
        val desk = host(1, "desk", Reach.Online(RouteKind.Direct))
        val studio = host(2, "studio", Reach.Online(RouteKind.Relay))
        val away = host(3, "mini", Reach.Away)
        val offline = host(4, "pi", Reach.Offline(Offline.Plain))
        val foundOpen = host(5, "laptop", trusted = false, candidate = true, pairingOpen = true)
        val foundClosed = host(6, "imac", trusted = false, candidate = true, pairingOpen = false)

        val actions = mutableListOf<HostsAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                HostsTab(
                    state = HostsTabState(
                        hosts = listOf(desk, studio, away, offline),
                        discovered = listOf(foundOpen, foundClosed),
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(TestTags.hostsFound(foundOpen.hostId)).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.hostsFound(foundClosed.hostId)).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.hostsRow(desk.hostId)).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.hostsRow(studio.hostId)).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.hostsRow(away.hostId)).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.hostsRow(offline.hostId)).assertIsDisplayed()

        composeRule.onNodeWithTag(TestTags.hostsPair(foundOpen.hostId)).performClick()
        assertEquals(HostsAction.Pair(foundOpen.hostId), actions.last())

        composeRule.onNodeWithTag(TestTags.hostsRow(desk.hostId)).performClick()
        assertEquals(HostsAction.Open(desk.hostId), actions.last())
        // Named by the device's id, as the journeys address it on every client.
        assertEquals("hosts.row.01010101-0101-0101-0101-010101010101", TestTags.hostsRow(desk.hostId))
    }

    /**
     * A paired device reached directly on the network both are on reads
     * Local, on its row and to somebody who cannot see it; one reached
     * directly across networks still reads Direct, and one through the
     * relay Relayed.
     */
    @Test
    fun pairedDeviceOnThisNetworkReadsLocal() {
        val desk = host(1, "desk", Reach.Online(RouteKind.Direct), sameNetwork = true)
        val studio = host(2, "studio", Reach.Online(RouteKind.Direct))
        val build = host(3, "build", Reach.Online(RouteKind.Relay))
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                HostsTab(state = HostsTabState(hosts = listOf(desk, studio, build)), onAction = {})
            }
        }

        composeRule.onNodeWithText("macOS · Local").assertIsDisplayed()
        composeRule.onNodeWithText("macOS · Direct").assertIsDisplayed()
        composeRule.onNodeWithText("macOS · Relayed").assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.hostsRow(desk.hostId))
            .assertContentDescriptionContains("desk, macOS, connected, local")
        composeRule.onNodeWithTag(TestTags.hostsRow(studio.hostId))
            .assertContentDescriptionContains("studio, macOS, connected, direct")
        composeRule.onNodeWithTag(TestTags.hostsRow(build.hostId))
            .assertContentDescriptionContains("build, macOS, connected, relayed")
    }
}
