package sh.amux.features

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.values.HostView
import sh.amux.values.PendingPair
import sh.amux.values.Platform
import sh.amux.values.Reach
import sh.amux.values.RouteKind

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class PairingSheetTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val laptop = HostView(
        hostId = List(16) { 4 },
        link = 0L,
        sameNetwork = false,
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

    @Test
    fun pinEntryFiltersPastedSpaceSeparatedSixDigits() {
        val actions = mutableListOf<PairingAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                PairingSheetContent(
                    state = PairingSheetState(
                        digits = "",
                        phase = PairingPhaseUi.Entering,
                        machine = laptop,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(TestTags.PAIRING_DIGITS).performTextInput("418 207")
        assertEquals(PairingAction.Digits("418207"), actions.last())
    }

    @Test
    fun confirmingDisplaysFullGroupedFingerprintAndConfirmCancelActions() {
        val fingerprint = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        val pending = PendingPair(
            token = List(16) { 7 },
            hostId = laptop.hostId,
            name = "laptop",
            fingerprint = fingerprint,
            expiresAtMs = 1_750_000_240_000L,
            via = RouteKind.Direct,
        )
        val actions = mutableListOf<PairingAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                PairingSheetContent(
                    state = PairingSheetState(
                        phase = PairingPhaseUi.Confirming(pending = pending, platformName = "macOS"),
                        machine = laptop,
                        nowMs = 1_750_000_000_000L,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(TestTags.PAIRING_FINGERPRINT).assertIsDisplayed()
        composeRule.onNodeWithText(FingerprintFormat.grouped(fingerprint)).assertIsDisplayed()
        // A direct dial to the laptop discovery found here stays Local, in
        // the header and the line under the name alike.
        composeRule.onNodeWithText("Local").assertIsDisplayed()
        composeRule.onNodeWithText("macOS · Local").assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.PAIRING_CONFIRM).performClick()
        assertEquals(PairingAction.Confirm(pending), actions.last())

        composeRule.onNodeWithTag(TestTags.PAIRING_CANCEL).performClick()
        assertEquals(PairingAction.Abandon(pending), actions.last())
    }

    @Test
    fun storeFailureScreenShowsCauseAndCallsRelaunch() {
        var relaunched = false
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                StoreFailureScreen(
                    message = "database disk image is malformed",
                    onRelaunch = { relaunched = true },
                )
            }
        }

        composeRule.onNodeWithTag(TestTags.STORE_FAILURE_TITLE).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.STORE_FAILURE_CAUSE).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.STORE_FAILURE_RELAUNCH).performClick()
        assertTrue(relaunched)
    }

    @Test
    fun headerSaysLocalForALaptopFoundOnThisNetwork() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                PairingSheetContent(
                    state = PairingSheetState(phase = PairingPhaseUi.Entering, machine = laptop),
                    onAction = {},
                )
            }
        }

        composeRule.onNodeWithText("Local").assertIsDisplayed()
    }

    /**
     * Local only for a candidate discovery found here: an address, not one
     * only the relay reaches. A candidate with no address here reads Relayed,
     * one a paired device announced reads no route, and a paired device reads
     * Local only when reached directly on the network both are on.
     */
    @Test
    fun headerRouteSaysLocalOnlyForADeviceFoundHere() {
        fun route(machine: HostView, phase: PairingPhaseUi = PairingPhaseUi.Entering) =
            pairingRoute(PairingSheetState(phase = phase, machine = machine))

        assertEquals(PairingRoute.Local, route(laptop))
        val announced = laptop.copy(addrs = emptyList(), reach = Reach.Online(RouteKind.Unspecified))
        assertEquals(PairingRoute.Relayed, route(announced))
        assertNull(route(announced.copy(through = "desk")))
        assertEquals(PairingRoute.Relayed, route(laptop.copy(relayOnly = true, addrs = emptyList())))
        assertEquals(PairingRoute.Relayed, route(laptop.copy(reach = Reach.Online(RouteKind.Relay))))
        assertEquals(PairingRoute.Relayed, route(laptop, PairingPhaseUi.NeedsSubscription))

        val paired = laptop.copy(candidate = false, trusted = true, addrs = emptyList())
        assertEquals(PairingRoute.Direct, route(paired))
        assertEquals(PairingRoute.Local, route(paired.copy(sameNetwork = true)))
        assertEquals(PairingRoute.Relayed, route(paired.copy(reach = Reach.Online(RouteKind.Relay))))
        assertNull(route(paired.copy(reach = Reach.Away)))

        val pending = PendingPair(
            token = List(16) { 7 },
            hostId = laptop.hostId,
            name = "laptop",
            fingerprint = "00",
            expiresAtMs = 0L,
            via = RouteKind.Direct,
        )
        assertEquals(PairingRoute.Local, confirmingRoute(pending, laptop))
        assertEquals(PairingRoute.Direct, confirmingRoute(pending, null))
        assertEquals(PairingRoute.Direct, confirmingRoute(pending, laptop.copy(hostId = List(16) { 9 })))
        assertEquals(PairingRoute.Relayed, confirmingRoute(pending.copy(via = RouteKind.Relay), laptop))
    }
}
