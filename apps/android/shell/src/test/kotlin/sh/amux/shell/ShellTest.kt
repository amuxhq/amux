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

    @Test
    fun addresslessQrInvitationUsesDiscoveredLanHostWithoutAccount() {
        val hostId = HostId(UUID.fromString("55555555-5555-5555-5555-555555555555"))
        val invitation = sh.amux.core.runtime.PairingInvitation(
            host = hostId,
            addrs = emptyList(),
            link = "https://amux.sh/p#placeholder",
        )
        val modal = PairingModal.Link(invitation)
        val pairing = sh.amux.core.runtime.PairingState()

        // Without discovery, an addressless QR invitation requires signing in.
        val beforeDiscovery = StateMappers.toPairingSheetState(
            pairing = pairing,
            modal = modal,
            signedIn = false,
            signingIn = false,
            nowMs = 0L,
            discovered = emptyList(),
        )
        assertTrue(beforeDiscovery.phase is sh.amux.features.PairingPhaseUi.NeedsAccount)

        // Once local discovery finds that host (e.g. IPv6-only LAN), no account is needed.
        val discoveredHost = HostView(
            hostId = hostId.bytes,
            name = "ipv6-desk",
            local = false,
            trusted = false,
            candidate = true,
            reach = Reach.Online(RouteKind.Direct),
            addrs = listOf("[2001:db8::1]:4433"),
            relayOnly = false,
            pairingOpen = true,
            current = true,
            providers = emptyList(),
            platform = Platform.Linux,
        )
        val afterDiscovery = StateMappers.toPairingSheetState(
            pairing = pairing,
            modal = modal,
            signedIn = false,
            signingIn = false,
            nowMs = 0L,
            discovered = listOf(discoveredHost),
        )
        assertTrue(afterDiscovery.phase is sh.amux.features.PairingPhaseUi.Checking)
    }

    @Test
    fun closingConfirmingPairingSheetAbandonsPendingPair() {
        val router = Router()
        val accounts = AccountRegistry()
        val abandoned = mutableListOf<sh.amux.values.PendingPair>()
        val stores = StoreBundle(
            account = AccountId("signed-out"),
            abandonPairOp = { pending ->
                abandoned += pending
                Result.success(Unit)
            },
        )
        stores.fleet.show(FleetView(sections = emptyList(), needYou = 0, working = 0), hosts = emptyList())

        val hostId = HostId(UUID.fromString("66666666-6666-6666-6666-666666666666"))
        val desk = HostView(
            hostId = hostId.bytes,
            name = "desk",
            local = false,
            trusted = false,
            candidate = true,
            reach = Reach.Online(RouteKind.Direct),
            addrs = listOf("192.168.1.9:4433"),
            relayOnly = false,
            pairingOpen = true,
            current = true,
            providers = emptyList(),
            platform = Platform.Macos,
        )
        val pending = sh.amux.values.PendingPair(
            token = listOf(1, 2, 3, 4),
            hostId = hostId.bytes,
            name = "desk",
            fingerprint = "abcd 1234",
            expiresAtMs = 999_999L,
            via = RouteKind.Direct,
        )
        stores.pairing.open(machine = desk)
        val attempt = stores.pairing.checking()
        stores.pairing.reached(Result.success(pending), attempt = attempt)
        router.presentPairing(PairingModal.Code(hostId))

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

        composeRule.onNodeWithTag(TestTags.PAIRING_CONFIRM).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.PAIRING_CLOSE).performClick()
        composeRule.waitForIdle()

        assertNull(router.snapshot.pairing)
        assertEquals(listOf(pending), abandoned)
    }
}
