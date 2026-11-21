package sh.amux.features

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextReplacement
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
class AccountSheetsTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun signInOfferReceivedShowsAcceptDeclineAndFailureNotice() {
        val actions = mutableListOf<SignInOfferAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SignInOfferSheetContent(
                    state = SignInOfferSheetState(
                        prompt = SignInOfferPromptUi.Offered(
                            hostName = "studio",
                            email = "ada@example.com",
                        ),
                        failure = SignInOfferFailureUi.Unreachable(hostName = "studio"),
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_OFFER_TITLE).assertIsDisplayed()
        composeRule.onNodeWithText("Sign in as ada@example.com?").assertIsDisplayed()
        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_OFFER_FAILURE).assertIsDisplayed()

        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_OFFER_ACCEPT).performClick()
        assertEquals(SignInOfferAction.Accept, actions.last())

        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_OFFER_DECLINE).performClick()
        assertEquals(SignInOfferAction.Decline, actions.last())
    }

    @Test
    fun signInOfferOutgoingShowsOfferNotNowAndWorkingState() {
        val actions = mutableListOf<SignInOfferAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Dark, photographed = true) {
                SignInOfferSheetContent(
                    state = SignInOfferSheetState(
                        prompt = SignInOfferPromptUi.Offering(
                            hostName = "laptop",
                            email = "ada@example.com",
                        ),
                        working = true,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithText("Sign in laptop as ada@example.com too?").assertIsDisplayed()
        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_OFFER_WORKING).assertIsDisplayed()
        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_OFFER_DECLINE).assertIsNotEnabled()
    }

    @Test
    fun signInResultMismatchedShowsExpectedAndSignedInEmailsAndEmitsActions() {
        val actions = mutableListOf<SignInResultAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SignInResultDialogContent(
                    result = SignInResultUi.Mismatched(
                        expectedEmail = "ada@example.com",
                        signedInEmail = "other@example.com",
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_RESULT_BODY).assertIsDisplayed()
        composeRule.onNodeWithText("You signed in as other@example.com, not ada@example.com.")
            .assertIsDisplayed()

        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_RESULT_TRY_AGAIN).performClick()
        assertEquals(SignInResultAction.TryAgain, actions.last())

        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_RESULT_KEEP).performClick()
        assertEquals(SignInResultAction.KeepSignedInAccount, actions.last())

        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_RESULT_DISMISS).performClick()
        assertEquals(SignInResultAction.Dismiss, actions.last())
    }

    @Test
    fun signInResultFailedDisplaysReasonAndEmitsTryAgainAndDismiss() {
        val actions = mutableListOf<SignInResultAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SignInResultDialogContent(
                    result = SignInResultUi.Failed(reason = "amux.sh did not answer"),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithText("amux.sh did not answer").assertIsDisplayed()
        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_RESULT_TRY_AGAIN).performClick()
        assertEquals(SignInResultAction.TryAgain, actions.last())

        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_RESULT_DISMISS).performClick()
        assertEquals(SignInResultAction.Dismiss, actions.last())
    }

    @Test
    fun removeAccountDialogExplainsLocalRetentionAndEmitsConfirmCancel() {
        val actions = mutableListOf<RemoveAccountAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                RemoveAccountDialogContent(
                    state = RemoveAccountState(
                        accountId = "sub-1",
                        email = "ada@example.com",
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithText("Remove ada@example.com from this phone?").assertIsDisplayed()
        composeRule.onNodeWithTag(AccountSheetTags.REMOVE_DETAIL).assertIsDisplayed()

        composeRule.onNodeWithTag(AccountSheetTags.REMOVE_CONFIRM).performClick()
        assertEquals(RemoveAccountAction.Confirm("sub-1"), actions.last())

        composeRule.onNodeWithTag(AccountSheetTags.REMOVE_CANCEL).performClick()
        assertEquals(RemoveAccountAction.Cancel, actions.last())
    }

    @Test
    fun deleteAccountDisablesConfirmUntilTypedEmailMatchesCaseInsensitively() {
        val actions = mutableListOf<DeleteAccountAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                DeleteAccountSheetContent(
                    state = DeleteAccountSheetState(
                        accountId = "sub-1",
                        email = "ada@example.com",
                        typedEmail = "wrong@example.com",
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(AccountSheetTags.DELETE_CONFIRM)
            .performScrollTo()
            .assertIsNotEnabled()
        composeRule.onNodeWithTag(AccountSheetTags.DELETE_EMAIL_INPUT)
            .performScrollTo()
            .performTextReplacement("ada@example.com")
        assertEquals(DeleteAccountAction.TypeEmail("ada@example.com"), actions.last())
    }

    @Test
    fun deleteAccountEnablesConfirmWhenEmailMatchesCaseInsensitively() {
        val actions = mutableListOf<DeleteAccountAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                DeleteAccountSheetContent(
                    state = DeleteAccountSheetState(
                        accountId = "sub-1",
                        email = "ada@example.com",
                        typedEmail = "  ADA@Example.COM ",
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(AccountSheetTags.DELETE_CONFIRM)
            .performScrollTo()
            .assertIsEnabled()
            .performClick()
        assertEquals(DeleteAccountAction.ConfirmDelete, actions.last())
    }

    @Test
    fun deleteAccountBlocked409ShowsBillingPortalAndTryAgain() {
        val actions = mutableListOf<DeleteAccountAction>()
        val portalUrl = "https://billing.stripe.com/p/session/test_123"
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                DeleteAccountSheetContent(
                    state = DeleteAccountSheetState(
                        accountId = "sub-1",
                        email = "ada@example.com",
                        typedEmail = "ada@example.com",
                        phase = DeleteAccountPhaseUi.Blocked(
                            providerPlace = "amux.sh",
                            manageUrl = portalUrl,
                        ),
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(AccountSheetTags.DELETE_BLOCKED).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(AccountSheetTags.DELETE_EMAIL_INPUT).assertDoesNotExist()

        composeRule.onNodeWithTag(AccountSheetTags.DELETE_BILLING_PORTAL).performScrollTo().performClick()
        assertEquals(DeleteAccountAction.OpenBillingPortal(portalUrl), actions.last())

        composeRule.onNodeWithTag(AccountSheetTags.DELETE_TRY_AGAIN).performScrollTo().performClick()
        assertEquals(DeleteAccountAction.TryAgain, actions.last())

        composeRule.onNodeWithTag(AccountSheetTags.DELETE_CANCEL).performScrollTo().performClick()
        assertEquals(DeleteAccountAction.Cancel, actions.last())
    }

    @Test
    fun deleteAccountWorkingAndFailedStatesRenderCorrectly() {
        val actions = mutableListOf<DeleteAccountAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                DeleteAccountSheetContent(
                    state = DeleteAccountSheetState(
                        accountId = "sub-1",
                        email = "ada@example.com",
                        typedEmail = "ada@example.com",
                        phase = DeleteAccountPhaseUi.Failed("Service unavailable"),
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(AccountSheetTags.DELETE_FAILED).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithText("Service unavailable").assertIsDisplayed()
        composeRule.onNodeWithTag(AccountSheetTags.DELETE_TRY_AGAIN).performScrollTo().performClick()
        assertEquals(DeleteAccountAction.TryAgain, actions.last())
    }

    @Test
    fun deleteAccountDeletingStateShowsWorkingRowAndDisablesCancel() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Dark, photographed = true) {
                DeleteAccountSheetContent(
                    state = DeleteAccountSheetState(
                        accountId = "sub-1",
                        email = "ada@example.com",
                        typedEmail = "ada@example.com",
                        phase = DeleteAccountPhaseUi.Deleting,
                    ),
                    onAction = {},
                )
            }
        }

        composeRule.onNodeWithTag(AccountSheetTags.DELETE_WORKING).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(AccountSheetTags.DELETE_CANCEL).performScrollTo().assertIsNotEnabled()
    }
}
