package sh.amux.features

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.unit.Density
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SettingsScreenTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val proAccount = SettingsAccountItem(
        id = "sub-1",
        name = "Ada Lovelace",
        email = "ada@example.com",
        line = "2 hosts",
        signedIn = true,
        selected = true,
        entitlementBadge = SettingsEntitlementBadge.Pro,
        entitlementSummary = "Pro",
    )

    private val expiredAccount = SettingsAccountItem(
        id = "sub-2",
        name = "Grace Hopper",
        email = "grace@example.com",
        line = "grace@example.com",
        signedIn = false,
        selected = false,
        expired = true,
        entitlementBadge = SettingsEntitlementBadge.Free,
    )

    @Test
    fun signedOutShowsCloudRelayExplanationAndEmitsSignIn() {
        val actions = mutableListOf<SettingsAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SettingsScreen(
                    state = SettingsScreenState(
                        accounts = emptyList(),
                        deviceName = "Pixel 9",
                        appVersion = "0.9.0",
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(SettingsTags.SIGNED_OUT_CARD).assertIsDisplayed()
        composeRule.onNodeWithTag(SettingsTags.SIGN_IN_BUTTON).performClick()
        assertEquals(SettingsAction.SignIn, actions.last())

        composeRule.onNodeWithTag(SettingsTags.BACK).performClick()
        assertEquals(SettingsAction.Back, actions.last())
    }

    @Test
    fun signedInListsAccountsBadgesExpiredSignInAgainAndAccountActions() {
        val actions = mutableListOf<SettingsAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SettingsScreen(
                    state = SettingsScreenState(
                        accounts = listOf(proAccount, expiredAccount),
                        deviceName = "Pixel 9",
                        appVersion = "0.9.0",
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(SettingsTags.accountRow("ada@example.com")).assertIsDisplayed()
        composeRule.onNodeWithTag(SettingsTags.accountBadge("ada@example.com"), useUnmergedTree = true)
            .assertIsDisplayed()

        composeRule.onNodeWithTag(SettingsTags.accountSignInAgain("grace@example.com"))
            .performScrollTo()
            .performClick()
        assertEquals(SettingsAction.SignInAgain("sub-2"), actions.last())

        composeRule.onNodeWithTag(SettingsTags.ADD_ACCOUNT)
            .performScrollTo()
            .performClick()
        assertEquals(SettingsAction.AddAccount, actions.last())

        composeRule.onNodeWithTag(SettingsTags.MANAGE_SUBSCRIPTION)
            .performScrollTo()
            .performClick()
        assertEquals(SettingsAction.ManageSubscription("sub-1"), actions.last())

        composeRule.onNodeWithTag(SettingsTags.SIGN_OUT)
            .performScrollTo()
            .performClick()
        assertEquals(SettingsAction.SignOut("sub-1"), actions.last())

        composeRule.onNodeWithTag(SettingsTags.REMOVE_ACCOUNT)
            .performScrollTo()
            .performClick()
        assertEquals(SettingsAction.RemoveAccount("sub-1"), actions.last())

        composeRule.onNodeWithTag(SettingsTags.DELETE_ACCOUNT)
            .performScrollTo()
            .performClick()
        assertEquals(SettingsAction.DeleteAccount("sub-1"), actions.last())
    }

    @Test
    fun appearanceToggleEmitsLightDarkAndSystem() {
        val actions = mutableListOf<SettingsAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SettingsScreen(
                    state = SettingsScreenState(appearance = SettingsAppearance.System),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(SettingsTags.APPEARANCE_SYSTEM).performScrollTo().assertIsSelected()
        composeRule.onNodeWithTag(SettingsTags.APPEARANCE_DARK).performScrollTo().performClick()
        assertEquals(SettingsAction.SetAppearance(SettingsAppearance.Dark), actions.last())

        composeRule.onNodeWithTag(SettingsTags.APPEARANCE_LIGHT).performScrollTo().performClick()
        assertEquals(SettingsAction.SetAppearance(SettingsAppearance.Light), actions.last())
    }

    @Test
    fun notificationsRowShowsHonestComingSoonStatusWithoutActionButtonWhenPushNotEnabled() {
        val actions = mutableListOf<SettingsAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SettingsScreen(
                    state = SettingsScreenState(
                        notificationStatus = SettingsNotificationStatus.NotAsked,
                        pushDeliveryComingSoon = true,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(SettingsTags.NOTIFICATIONS_CARD).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithText("Coming soon").performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithText(
            "Background push notifications are not yet enabled in this version of amux.",
        ).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(SettingsTags.NOTIFICATIONS_ACTION).assertDoesNotExist()
        assertTrue(actions.isEmpty())
    }

    @Test
    fun notificationsRowHandlesNotAskedAndConfiguredStatesWhenPushEnabled() {
        val actions = mutableListOf<SettingsAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SettingsScreen(
                    state = SettingsScreenState(
                        notificationStatus = SettingsNotificationStatus.NotAsked,
                        pushDeliveryComingSoon = false,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(SettingsTags.NOTIFICATIONS_CARD).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithText("Enable Notifications").performScrollTo().performClick()
        assertEquals(SettingsAction.EnableNotifications, actions.last())
    }

    @Test
    fun notificationsRowOpensSystemSettingsWhenEnabledOrOffAndPushEnabled() {
        val actions = mutableListOf<SettingsAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SettingsScreen(
                    state = SettingsScreenState(
                        notificationStatus = SettingsNotificationStatus.Enabled,
                        pushDeliveryComingSoon = false,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithText("Enabled").performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(SettingsTags.NOTIFICATIONS_ACTION).performScrollTo().performClick()
        assertEquals(SettingsAction.OpenNotificationSettings, actions.last())
    }

    @Test
    fun privacyTogglesReportProblemAndDiagnosticsExportWork() {
        val actions = mutableListOf<SettingsAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SettingsScreen(
                    state = SettingsScreenState(
                        shareUsage = true,
                        autoReports = false,
                        diagnosticsExport = DiagnosticsExportState.Failed("Disk full"),
                        deviceName = "Pixel 9",
                        deviceFingerprint = "0123456789abcdef0123456789abcdef",
                        appVersion = "0.9.1 (42)",
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(SettingsTags.SHARE_USAGE_ROW).performScrollTo().performClick()
        assertEquals(SettingsAction.SetShareUsage(false), actions.last())

        composeRule.onNodeWithTag(SettingsTags.AUTO_REPORTS_ROW).performScrollTo().performClick()
        assertEquals(SettingsAction.SetAutoReports(true), actions.last())

        composeRule.onNodeWithTag(SettingsTags.REPORT_PROBLEM).performScrollTo().performClick()
        assertEquals(SettingsAction.ReportProblem, actions.last())

        composeRule.onNodeWithTag(SettingsTags.EXPORT_DIAGNOSTICS).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(SettingsTags.EXPORT_DIAGNOSTICS_STATUS, useUnmergedTree = true)
            .assertIsDisplayed()
        composeRule.onNodeWithText("Disk full", useUnmergedTree = true).assertIsDisplayed()
        composeRule.onNodeWithTag(SettingsTags.EXPORT_DIAGNOSTICS).performClick()
        assertEquals(SettingsAction.ExportDiagnostics, actions.last())

        composeRule.onNodeWithTag(SettingsTags.DEVICE_NAME_ROW).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithText("Pixel 9").assertIsDisplayed()

        composeRule.onNodeWithTag(SettingsTags.DEVICE_FINGERPRINT_ROW).performScrollTo().performClick()
        assertEquals(
            SettingsAction.CopyFingerprint("0123456789abcdef0123456789abcdef"),
            actions.last(),
        )

        composeRule.onNodeWithTag(SettingsTags.APP_VERSION_ROW).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithText("0.9.1 (42)").assertIsDisplayed()
    }

    @Test
    fun exportingDiagnosticsDisablesDuplicateClicks() {
        val actions = mutableListOf<SettingsAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Dark, photographed = true) {
                SettingsScreen(
                    state = SettingsScreenState(
                        diagnosticsExport = DiagnosticsExportState.Exporting,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(SettingsTags.EXPORT_DIAGNOSTICS)
            .performScrollTo()
            .assertIsNotEnabled()
        assertTrue(actions.isEmpty())
    }

    @Test
    fun largeFontScaleStacksSectionsCleanlyWithoutBreakingControls() {
        val actions = mutableListOf<SettingsAction>()
        composeRule.setContent {
            CompositionLocalProvider(LocalDensity provides Density(density = 1f, fontScale = 1.5f)) {
                AmuxTheme(appearance = Appearance.Light, photographed = true) {
                    SettingsScreen(
                        state = SettingsScreenState(
                            accounts = listOf(proAccount),
                            appearance = SettingsAppearance.Light,
                            notificationStatus = SettingsNotificationStatus.Off,
                            deviceName = "Pixel 9 Pro",
                            deviceFingerprint = "0123456789abcdef",
                            appVersion = "0.9.0",
                        ),
                        onAction = { actions += it },
                    )
                }
            }
        }

        composeRule.onNodeWithTag(SettingsTags.APPEARANCE_DARK).performScrollTo().performClick()
        assertEquals(SettingsAction.SetAppearance(SettingsAppearance.Dark), actions.last())

        composeRule.onNodeWithTag(SettingsTags.NOTIFICATIONS_STATUS).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(SettingsTags.NOTIFICATIONS_ACTION).assertDoesNotExist()
    }
}
