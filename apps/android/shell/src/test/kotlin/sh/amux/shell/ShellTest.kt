package sh.amux.shell

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.AccountRegistry
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.StoreBundle
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.features.TestTags
import sh.amux.values.FleetView
import sh.amux.values.HostView
import sh.amux.values.Platform
import sh.amux.values.Reach
import sh.amux.values.RouteKind

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class ShellTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun switchesTabsPushesSettingsHidingBottomBarAndPopsBack() {
        val router = Router()
        val accounts = AccountRegistry()
        val stores = StoreBundle(account = AccountId("signed-out"))
        stores.fleet.show(FleetView(sections = emptyList(), needYou = 0, working = 0), hosts = emptyList())

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                )
            }
        }

        // Starts on Agents with bottom bar visible
        composeRule.onNodeWithTag(TestTags.HOME_SCREEN).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.TAB_AGENTS).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.TAB_HOSTS).assertIsDisplayed()

        // Switch to Computers tab
        composeRule.onNodeWithTag(TestTags.TAB_HOSTS).performClick()
        composeRule.onNodeWithTag(TestTags.HOSTS_SCREEN).assertIsDisplayed()

        // Back to Agents, open account switcher, push Settings
        composeRule.onNodeWithTag(TestTags.TAB_AGENTS).performClick()
        composeRule.onNodeWithTag(TestTags.HOME_ACCOUNT_MENU).performClick()
        composeRule.onNodeWithTag(TestTags.ACCOUNTS_SHEET).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.ACCOUNTS_SETTINGS).performClick()

        // Settings is full-screen and hides bottom bar
        composeRule.onNodeWithTag(TestTags.SCREEN_SETTINGS).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.TAB_AGENTS).assertDoesNotExist()

        // Back button returns to Agents and restores bottom bar
        composeRule.onNodeWithTag(TestTags.NAV_BACK).performClick()
        composeRule.onNodeWithTag(TestTags.HOME_SCREEN).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.TAB_AGENTS).assertIsDisplayed()
    }

    @Test
    fun autoRaisesPairingForOpenWindowAndDeclinesOnDismiss() {
        val router = Router()
        val accounts = AccountRegistry()
        val stores = StoreBundle(account = AccountId("signed-out"))
        val laptopId = HostId(UUID.fromString("44444444-4444-4444-4444-444444444444"))
        val laptop = HostView(
            hostId = laptopId.bytes,
            name = "laptop",
            local = false,
            trusted = false,
            candidate = true,
            reach = Reach.Online(RouteKind.Direct),
            addrs = listOf("192.168.1.4:4433"),
            relayOnly = false,
            pairingOpen = true,
            current = true,
            providers = emptyList(),
            platform = Platform.Macos,
        )
        stores.hosts.show(listOf(laptop))
        stores.fleet.show(FleetView(sections = emptyList(), needYou = 0, working = 0), hosts = listOf(laptop))

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                )
            }
        }

        composeRule.waitForIdle()
        assertEquals(PairingModal.Code(laptopId), router.snapshot.pairing)
        composeRule.onNodeWithTag(TestTags.PAIRING_SHEET).assertIsDisplayed()

        // Closing the pairing sheet records declineWindow so it does not re-raise
        composeRule.onNodeWithTag(TestTags.PAIRING_CLOSE).performClick()
        composeRule.waitForIdle()
        assertNull(router.snapshot.pairing)
        assertTrue(laptopId in stores.hosts.snapshot.declinedWindows)
    }
}
