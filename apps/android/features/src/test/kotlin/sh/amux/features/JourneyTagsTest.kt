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
import org.junit.Assert.assertEquals
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
                                line = "1 computer",
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

        composeRule.onNodeWithTag(SettingsTags.accountRow("ada@example.com")).assertExists()
        composeRule.onNodeWithTag(SettingsTags.accountRow("grace@example.com")).assertExists()
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
                                    line = "1 computer",
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
        composeRule.onNodeWithTag(SettingsTags.SIGN_OUT).assertExists()
        signedIn = false
        composeRule.onNodeWithTag(SettingsTags.SIGN_IN_BUTTON).assertExists()
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
        composeRule.onNodeWithTag(ReportTags.RECEIPT_ID).assertTextEquals("report-1")
        composeRule.onNodeWithTag(ReportTags.COPY_RECEIPT).assertExists()
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

        composeRule.onNodeWithTag(TestTags.PAIRING_TITLE).assertExists()
        composeRule.onNodeWithTag(TestTags.PAIRING_FINGERPRINT).assertExists()
        composeRule.onNodeWithTag(TestTags.PAIRING_CONFIRM).assertExists()
    }
}
