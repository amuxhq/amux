package sh.amux.features

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.testsupport.DoorReading
import sh.amux.values.HostView
import sh.amux.values.PendingPair
import sh.amux.values.Platform
import sh.amux.values.Reach
import sh.amux.values.RouteKind

/**
 * The Android journeys find what they tap, and read what they check, in the
 * merged semantics tree, as TalkBack does. A tag on a node that merges into a
 * row disappears from that tree, so these tests look each one up there (the
 * finders' default) rather than in the unmerged tree.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class JourneyTagsTest {
    @get:Rule
    val composeRule = createComposeRule()

    /** The label the door reports for the element tagged [tag]. */
    private fun label(tag: String): String? =
        DoorReading.label(composeRule.onNodeWithTag(tag).fetchSemanticsNode())

    /** The value the door reports for the element tagged [tag]. */
    private fun value(tag: String): String? =
        DoorReading.value(composeRule.onNodeWithTag(tag).fetchSemanticsNode())

    private fun enabled(tag: String): Boolean =
        SemanticsProperties.Disabled !in composeRule.onNodeWithTag(tag).fetchSemanticsNode().config

    @Test
    fun settingsTagsTheJourneysUseStandInTheMergedTree() {
        val actions = mutableListOf<SettingsAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SettingsScreen(
                    state = SettingsScreenState(
                        accounts = listOf(
                            SettingsAccountItem(
                                id = "ada",
                                name = "Ada",
                                email = "ada@example.com",
                                line = "ada@example.com",
                                signedIn = false,
                                selected = true,
                            ),
                            SettingsAccountItem(
                                id = "grace",
                                name = "Grace",
                                email = "grace@example.com",
                                line = "1 device",
                                signedIn = true,
                                selected = false,
                            ),
                        ),
                        deviceName = "Pixel 9",
                        deviceFingerprint = "0123456789abcdef",
                        appVersion = "0.9.0",
                    ),
                    onAction = { actions += it },
                )
            }
        }

        assertEquals("you", SettingsTags.SCREEN)
        assertEquals("ada@example.com", value(SettingsTags.SCREEN))
        assertEquals("account.ada@example.com", SettingsTags.accountRow("ada@example.com"))
        assertEquals("Ada, Signed out", label(SettingsTags.accountRow("ada@example.com")))
        assertEquals("selected", value(SettingsTags.accountRow("ada@example.com")))
        assertEquals("Grace, 1 device", label(SettingsTags.accountRow("grace@example.com")))
        assertEquals("signed in", value(SettingsTags.accountRow("grace@example.com")))
        assertEquals("system", value(SettingsTags.APPEARANCE_GROUP))

        // A signed-out account's row signs it in again, as its own Sign In does.
        composeRule.onNodeWithTag(SettingsTags.accountRow("ada@example.com")).performClick()
        assertEquals(SettingsAction.SignInAgain("ada"), actions.last())
        actions.clear()
        composeRule.onNodeWithTag(SettingsTags.accountSignInAgain("ada@example.com"))
            .performScrollTo()
            .performClick()
        assertEquals(SettingsAction.SignInAgain("ada"), actions.last())
        for (tag in listOf(
            SettingsTags.ADD_ACCOUNT,
            SettingsTags.APPEARANCE_GROUP,
            SettingsTags.APPEARANCE_LIGHT,
            SettingsTags.APPEARANCE_DARK,
            SettingsTags.APPEARANCE_SYSTEM,
            SettingsTags.DEVICE_NAME_ROW,
            SettingsTags.DEVICE_FINGERPRINT_ROW,
            SettingsTags.APP_VERSION_ROW,
        )) {
            composeRule.onNodeWithTag(tag).assertExists()
        }
    }

    @Test
    fun signedInAccountOffersSignOutAndSignedOutPhoneOffersSignIn() {
        var signedIn by mutableStateOf(true)
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SettingsScreen(
                    state = SettingsScreenState(
                        accounts = if (signedIn) {
                            listOf(
                                SettingsAccountItem(
                                    id = "ada",
                                    name = "Ada",
                                    email = "ada@example.com",
                                    line = "1 device",
                                    signedIn = true,
                                    selected = true,
                                ),
                            )
                        } else {
                            emptyList()
                        },
                    ),
                    onAction = {},
                )
            }
        }
        assertEquals("you.signOut", SettingsTags.SIGN_OUT)
        composeRule.onNodeWithTag(SettingsTags.SIGN_OUT).assertExists()
        signedIn = false
        assertEquals("you.signIn", SettingsTags.SIGN_IN_BUTTON)
        assertEquals("none", value(SettingsTags.SCREEN))
        assertEquals("Sign In", label(SettingsTags.SIGN_IN_BUTTON))
        assertEquals(null, value(SettingsTags.SIGN_IN_BUTTON))
        assertTrue(enabled(SettingsTags.SIGN_IN_BUTTON))
    }

    @Test
    fun signInUnderWayReadsSigningInAndTakesNoPress() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SettingsScreen(state = SettingsScreenState(addingAccount = true), onAction = {})
            }
        }
        assertEquals("signing in", value(SettingsTags.SIGN_IN_BUTTON))
        assertFalse(enabled(SettingsTags.SIGN_IN_BUTTON))
    }

    @Test
    fun accountSwitcherItemsAreChosenByTheirNames() {
        val picked = mutableListOf<String>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AccountSwitcherContent(
                    accounts = listOf(
                        AccountUi("a1", "ada", "ada@example.com", "2 devices", signedIn = true, selected = true),
                        AccountUi("b1", "bob", "bob@example.com", "", signedIn = false, selected = false),
                    ),
                    onSelectAccount = { picked += "select $it" },
                    onSignInAgain = { picked += "sign in $it" },
                    onAddAccount = {},
                    onOpenSettings = { picked += "settings" },
                )
            }
        }
        val ada = TestTags.accountRow("ada@example.com")
        assertEquals("ada", label(ada))
        assertNotNull(composeRule.onNodeWithTag(ada).fetchSemanticsNode().config.getOrNull(SemanticsActions.OnClick))
        assertEquals("bob", label(TestTags.accountRow("bob@example.com")))
        assertEquals("Settings", label(TestTags.ACCOUNTS_SETTINGS))

        composeRule.onNodeWithTag(TestTags.accountRow("bob@example.com")).performClick()
        composeRule.onNodeWithTag(ada).performClick()
        composeRule.onNodeWithTag(TestTags.ACCOUNTS_SETTINGS).performClick()
        assertEquals(listOf("sign in b1", "select a1", "settings"), picked)
    }

    @Test
    fun signInOfferReadsWhichWayItGoes() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SignInOfferSheetContent(
                    state = SignInOfferSheetState(
                        prompt = SignInOfferPromptUi.Offered(hostName = "desk", email = "ada@example.com"),
                    ),
                    onAction = {},
                )
            }
        }
        assertEquals("sign-in-offer", AccountSheetTags.SIGN_IN_OFFER_SHEET)
        assertEquals("offered", value(AccountSheetTags.SIGN_IN_OFFER_SHEET))
        assertEquals("Sign in as ada@example.com?", label(AccountSheetTags.SIGN_IN_OFFER_TITLE))
        assertTrue(label(AccountSheetTags.SIGN_IN_OFFER_DETAIL)!!.contains("desk offers its account"))
        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_OFFER_ACCEPT).assertExists()
        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_OFFER_DECLINE).assertExists()
    }

    @Test
    fun signInResultButtonsAreChosenByTheirWords() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SignInResultDialogContent(
                    result = SignInResultUi.Mismatched(
                        expectedEmail = "ada@example.com",
                        signedInEmail = "grace@example.com",
                    ),
                    onAction = {},
                )
            }
        }
        assertEquals("Signed in as another account", label(AccountSheetTags.SIGN_IN_RESULT_TITLE))
        assertEquals(
            "You signed in as grace@example.com, not ada@example.com.",
            label(AccountSheetTags.SIGN_IN_RESULT_BODY),
        )
        assertEquals("Try Again", label(AccountSheetTags.SIGN_IN_RESULT_TRY_AGAIN))
        assertEquals("Cancel", label(AccountSheetTags.SIGN_IN_RESULT_DISMISS))
    }

    @Test
    fun reportReceiptIsItsOwnTextInTheMergedTree() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReportOverlayContent(
                    state = ReportUiState(delivery = ReportDeliveryUi.Sent(referenceId = "report-1")),
                    onAction = {},
                )
            }
        }

        composeRule.onNodeWithTag(ReportTags.SENT).assertIsDisplayed()
        assertEquals("Report sent", label(ReportTags.SENT))
        assertEquals("report-1", value(ReportTags.SENT))
        composeRule.onNodeWithTag(ReportTags.RECEIPT_ID).assertTextEquals("report-1")
        composeRule.onNodeWithTag(ReportTags.COPY_RECEIPT).assertExists()
    }

    @Test
    fun reportRefusalReadsNotSentWithTheServicesReason() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReportOverlayContent(
                    state = ReportUiState(delivery = ReportDeliveryUi.Failed(reason = "uploads are closed")),
                    onAction = {},
                )
            }
        }
        assertEquals("Not sent", label(ReportTags.REFUSAL))
        assertEquals("uploads are closed", value(ReportTags.REFUSAL))
    }

    @Test
    fun reportPromptIsThePressThatOpensTheReport() {
        var opened = false
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReportOfferBanner(onOpen = { opened = true }, onDismiss = {})
            }
        }
        assertEquals("report.prompt", ReportTags.OFFER_OPEN)
        composeRule.onNodeWithTag(ReportTags.OFFER_OPEN).performClick()
        assertTrue(opened)
    }

    @Test
    fun pairingConfirmTagsStandInTheMergedTree() {
        val desk = HostView(
            hostId = List(16) { 4 },
            link = 0L,
            sameNetwork = false,
            name = "desk",
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
        val pending = PendingPair(
            token = List(16) { 7 },
            hostId = desk.hostId,
            name = "desk",
            fingerprint = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            expiresAtMs = 1_750_000_240_000L,
            via = RouteKind.Direct,
        )
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                PairingSheetContent(
                    state = PairingSheetState(
                        phase = PairingPhaseUi.Confirming(pending = pending, platformName = "macOS"),
                        machine = desk,
                        nowMs = 1_750_000_000_000L,
                    ),
                    onAction = {},
                )
            }
        }

        composeRule.onNodeWithTag(TestTags.PAIR_CONFIRM_SHEET).assertExists()
        composeRule.onNodeWithTag(TestTags.PIN_SHEET).assertDoesNotExist()
        assertEquals("desk", value(TestTags.PAIR_CONFIRM_SHEET))
        assertEquals("Pair with desk?", value(TestTags.PAIR_CONFIRM_TITLE))
        assertEquals("desk", value(TestTags.PAIR_CONFIRM_NAME))
        assertEquals("Fingerprint", label(TestTags.PAIR_CONFIRM_FINGERPRINT))
        assertEquals(FingerprintFormat.grouped(pending.fingerprint), value(TestTags.PAIR_CONFIRM_FINGERPRINT))
        composeRule.onNodeWithTag(TestTags.PAIR_CONFIRM_EXPIRY).assertExists()
        assertEquals("Pair", label(TestTags.PAIR_CONFIRM_TRUST))
        assertEquals("Not Now", label(TestTags.PAIR_CONFIRM_ABANDON))
    }

    @Test
    fun pinTagsStandInTheMergedTree() {
        val desk = HostView(
            hostId = List(16) { 4 },
            link = 0L,
            sameNetwork = false,
            name = "desk",
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
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                PairingSheetContent(
                    state = PairingSheetState(digits = "418", phase = PairingPhaseUi.Entering, machine = desk),
                    onAction = {},
                )
            }
        }

        composeRule.onNodeWithTag(TestTags.PIN_SHEET).assertExists()
        composeRule.onNodeWithTag(TestTags.PAIR_CONFIRM_SHEET).assertDoesNotExist()
        assertEquals("418", value(TestTags.PIN_SHEET))
        assertEquals("Pair with desk?", value(TestTags.PIN_TITLE))
        assertEquals("Code", label(TestTags.PIN_CODE))
        assertEquals("418", value(TestTags.PIN_CODE))
        composeRule.onNodeWithTag(TestTags.PIN_EXPIRY).assertExists()
    }
}
