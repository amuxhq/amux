package sh.amux.features

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
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
import sh.amux.values.Provider
import sh.amux.values.ProviderSignIn
import sh.amux.values.Reach
import sh.amux.values.RouteKind

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class HostPageTest {
    @get:Rule
    val composeRule = createComposeRule()

    private fun host(
        reach: Reach = Reach.Online(RouteKind.Direct),
        addrs: List<String> = listOf("192.168.1.10:4433", "fe80::1:4433"),
        providers: List<ProviderSignIn> = listOf(
            ProviderSignIn(Provider.Claude, signedIn = true),
            ProviderSignIn(Provider.Codex, signedIn = false),
        ),
        lastDialError: String? = null,
    ): HostView = HostView(
        hostId = List(16) { 7 },
        name = "desk",
        local = false,
        trusted = true,
        candidate = false,
        reach = reach,
        addrs = addrs,
        relayOnly = false,
        pairingOpen = false,
        current = true,
        providers = providers,
        lastDialError = lastDialError,
        platform = Platform.Macos,
        version = "0.9.4",
    )

    @Test
    fun rendersGoneStateWhenHostIsNull() {
        val actions = mutableListOf<HostPageAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                HostPage(
                    state = HostPageState(host = null),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(HostPageTags.GONE).assertIsDisplayed()
        composeRule.onNodeWithTag(HostPageTags.BACK).performClick()
        assertEquals(HostPageAction.Back, actions.single())
    }

    @Test
    fun rendersDetailsProvidersCopyableAddressesAndFingerprintAndRevokeFlow() {
        val sampleHost = host()
        val fingerprint = "abcdef0123456789abcdef0123456789"
        val actions = mutableListOf<HostPageAction>()

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                HostPage(
                    state = HostPageState(
                        host = sampleHost,
                        fingerprint = fingerprint,
                        canOfferSignIn = true,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithText("desk").assertIsDisplayed()
        composeRule.onNodeWithTag(HostPageTags.REACH).assertIsDisplayed()
        composeRule.onNodeWithText("macOS").assertIsDisplayed()
        composeRule.onNodeWithText("0.9.4").assertIsDisplayed()

        // Providers section
        composeRule.onNodeWithTag(HostPageTags.providerRow("claude")).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(HostPageTags.providerRow("codex")).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithText("Signed in").assertIsDisplayed()
        composeRule.onNodeWithText("Not signed in").assertIsDisplayed()

        // Copy address shows Copied feedback and emits action
        composeRule.onNodeWithTag(HostPageTags.addressRow(0)).performScrollTo().performClick()
        assertEquals(HostPageAction.CopyAddress("192.168.1.10:4433"), actions.last())
        composeRule.onNodeWithTag(HostPageTags.COPIED_BADGE, useUnmergedTree = true).assertIsDisplayed()

        // Copy fingerprint emits action
        composeRule.onNodeWithTag(HostPageTags.FINGERPRINT_ROW).performScrollTo().performClick()
        assertEquals(HostPageAction.CopyFingerprint(fingerprint), actions.last())
        composeRule.onNodeWithTag(HostPageTags.COPIED_BADGE, useUnmergedTree = true).assertIsDisplayed()

        // Sign-in offer
        composeRule.onNodeWithTag(HostPageTags.OFFER_SIGN_IN).performScrollTo().performClick()
        assertEquals(HostPageAction.OfferSignIn(sampleHost.hostId), actions.last())

        // Pair Again
        composeRule.onNodeWithTag(HostPageTags.PAIR_AGAIN).performScrollTo().performClick()
        assertEquals(HostPageAction.PairAgain(sampleHost.hostId), actions.last())

        // Revoke opens confirmation dialog; cancel does not emit Revoke; confirm emits Revoke.
        composeRule.onNodeWithTag(HostPageTags.REVOKE).performScrollTo().performClick()
        composeRule.onNodeWithTag(HostPageTags.REVOKE_DIALOG).assertIsDisplayed()
        composeRule.onNodeWithTag(HostPageTags.REVOKE_CANCEL).performClick()
        assertTrue(actions.none { it is HostPageAction.Revoke })

        composeRule.onNodeWithTag(HostPageTags.REVOKE).performScrollTo().performClick()
        composeRule.onNodeWithTag(HostPageTags.REVOKE_CONFIRM).performClick()
        assertEquals(HostPageAction.Revoke(sampleHost.hostId), actions.last())
    }

    @Test
    fun rendersAwaySubscribeAndOfflineLastSeenDetails() {
        val actions = mutableListOf<HostPageAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Dark, photographed = true) {
                HostPage(
                    state = HostPageState(
                        host = host(reach = Reach.Away, addrs = emptyList(), providers = emptyList()),
                        accountPro = false,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(HostPageTags.MEANING).assertIsDisplayed()
        composeRule.onNodeWithTag(HostPageTags.SUBSCRIBE).assertIsDisplayed()
        composeRule.onNodeWithTag(HostPageTags.SUBSCRIBE).performClick()
        assertEquals(HostPageAction.Subscribe, actions.single())
    }

    @Test
    fun rendersOfflineLastSeenAndDialErrorFact() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                HostPage(
                    state = HostPageState(
                        host = host(
                            reach = Reach.Offline(Offline.Plain),
                            addrs = emptyList(),
                            providers = emptyList(),
                            lastDialError = "connection refused",
                        ),
                        wentOfflineAtMs = 10_000L,
                        nowMs = 190_000L,
                    ),
                    onAction = {},
                )
            }
        }

        composeRule.onNodeWithTag(HostPageTags.FACT_LAST_SEEN).assertIsDisplayed()
        composeRule.onNodeWithTag(HostPageTags.FACT_LAST_TRY).assertIsDisplayed()
        composeRule.onNodeWithText("Could not connect").assertIsDisplayed()
    }
}
