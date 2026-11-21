package sh.amux.features

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
    ): HostView = HostView(
        hostId = List(16) { index },
        link = 0L,
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
        composeRule.onNodeWithText("No computers yet").assertIsDisplayed()
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

        composeRule.onNodeWithTag(TestTags.hostsFound("laptop")).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.hostsFound("imac")).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.hostsRow("desk")).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.hostsRow("studio")).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.hostsRow("mini")).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.hostsRow("pi")).assertIsDisplayed()

        composeRule.onNodeWithTag(TestTags.hostsPair("laptop")).performClick()
        assertEquals(HostsAction.Pair(foundOpen.hostId), actions.last())

        composeRule.onNodeWithTag(TestTags.hostsRow("desk")).performClick()
        assertEquals(HostsAction.Open(desk.hostId), actions.last())
    }
}
