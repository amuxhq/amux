package sh.amux.features

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsSelected
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

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class PaywallScreenTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val plans = listOf(
        PaywallPlanUi(
            id = "amux_pro_monthly",
            period = PaywallPlanPeriod.Monthly,
            price = "$10",
        ),
        PaywallPlanUi(
            id = "amux_pro_yearly",
            period = PaywallPlanPeriod.Yearly,
            price = "$99",
            saving = "Save 17%",
        ),
    )

    @Test
    fun plansSelectionSubscribeAndRestoreActionsWork() {
        val actions = mutableListOf<PaywallAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                PaywallScreen(
                    state = PaywallScreenState(
                        plans = plans,
                        chosen = PaywallPlanPeriod.Yearly,
                        phase = PaywallPhaseUi.Ready,
                        accountEmail = "ada@example.com",
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(PaywallTags.PLAN_YEARLY).assertIsSelected()
        composeRule.onNodeWithTag(PaywallTags.PLAN_MONTHLY).performClick()
        assertEquals(PaywallAction.ChoosePlan(PaywallPlanPeriod.Monthly), actions.last())

        composeRule.onNodeWithTag(PaywallTags.SUBSCRIBE_BUTTON).performClick()
        assertEquals(PaywallAction.Subscribe, actions.last())

        composeRule.onNodeWithTag(PaywallTags.RESTORE_BUTTON).performClick()
        assertEquals(PaywallAction.RestorePurchases, actions.last())

        composeRule.onNodeWithTag(PaywallTags.BACK).performClick()
        assertEquals(PaywallAction.Back, actions.last())
    }

    @Test
    fun entitledStateShowsStatusRenewalTextAndManageOnWebLink() {
        val actions = mutableListOf<PaywallAction>()
        val manageUrl = "https://amux.sh/account"
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                PaywallScreen(
                    state = PaywallScreenState(
                        phase = PaywallPhaseUi.Entitled(
                            source = PaywallGrantSource.Web,
                            renewalOrExpiryText = "Renews on 14 June",
                        ),
                        manageUrl = manageUrl,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(PaywallTags.ENTITLED_CARD).assertIsDisplayed()
        composeRule.onNodeWithText("Subscribed on amux.sh").assertIsDisplayed()
        composeRule.onNodeWithText("Renews on 14 June").assertIsDisplayed()

        composeRule.onNodeWithTag(PaywallTags.MANAGE_WEB_BUTTON).performClick()
        assertEquals(PaywallAction.ManageOnWeb(manageUrl), actions.last())
    }

    @Test
    fun entitledPlayStoreStateOffersBothPlayAndWebManagement() {
        val actions = mutableListOf<PaywallAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Dark, photographed = true) {
                PaywallScreen(
                    state = PaywallScreenState(
                        phase = PaywallPhaseUi.Entitled(
                            source = PaywallGrantSource.PlayStore,
                            renewalOrExpiryText = "Renews monthly",
                        ),
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(PaywallTags.MANAGE_PLAY_BUTTON).performClick()
        assertEquals(PaywallAction.ManageInStore, actions.last())

        composeRule.onNodeWithTag(PaywallTags.MANAGE_WEB_BUTTON).performClick()
        assertEquals(PaywallAction.ManageOnWeb("https://amux.sh/account"), actions.last())
    }

    @Test
    fun storeUnavailableShowsWebManageFallbackAndRetry() {
        val actions = mutableListOf<PaywallAction>()
        val manageUrl = "https://amux.sh/account"
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                PaywallScreen(
                    state = PaywallScreenState(
                        plans = emptyList(),
                        phase = PaywallPhaseUi.StoreUnavailable(),
                        manageUrl = manageUrl,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(PaywallTags.UNAVAILABLE_CARD).assertIsDisplayed()
        composeRule.onNodeWithTag(PaywallTags.MANAGE_WEB_BUTTON).performClick()
        assertEquals(PaywallAction.ManageOnWeb(manageUrl), actions.last())

        composeRule.onNodeWithTag(PaywallTags.RETRY_BUTTON).performClick()
        assertEquals(PaywallAction.Retry, actions.last())
    }

    @Test
    fun unconfirmedStateHidesPlansAndOffersRetry() {
        val actions = mutableListOf<PaywallAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                PaywallScreen(
                    state = PaywallScreenState(
                        plans = plans,
                        phase = PaywallPhaseUi.Unconfirmed(PaywallUnconfirmedReason.SwitchingOn),
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(PaywallTags.UNCONFIRMED_BANNER).assertIsDisplayed()
        composeRule.onNodeWithTag(PaywallTags.PLAN_YEARLY).assertDoesNotExist()
        composeRule.onNodeWithTag(PaywallTags.RETRY_BUTTON).performClick()
        assertEquals(PaywallAction.Retry, actions.last())
    }

    @Test
    fun failedStateDisplaysErrorBannerAndEmitsRetry() {
        val actions = mutableListOf<PaywallAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                PaywallScreen(
                    state = PaywallScreenState(
                        plans = plans,
                        phase = PaywallPhaseUi.Failed("Payment declined"),
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(PaywallTags.FAILED_BANNER).assertIsDisplayed()
        composeRule.onNodeWithText("Payment declined").assertIsDisplayed()
        composeRule.onNodeWithTag(PaywallTags.RETRY_BUTTON).performClick()
        assertEquals(PaywallAction.Retry, actions.last())
    }

    @Test
    fun awaitingApprovalShowsPendingBannerAndWaitingLine() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                PaywallScreen(
                    state = PaywallScreenState(
                        plans = plans,
                        phase = PaywallPhaseUi.AwaitingApproval,
                    ),
                    onAction = {},
                )
            }
        }

        composeRule.onNodeWithTag(PaywallTags.PENDING_BANNER).assertIsDisplayed()
        composeRule.onNodeWithTag(PaywallTags.WAITING_LINE).assertIsDisplayed()
    }
}
