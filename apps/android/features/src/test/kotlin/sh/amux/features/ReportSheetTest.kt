package sh.amux.features

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
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
class ReportSheetTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun offerBannerEmitsOpenAndDismissCallbacks() {
        var opened = false
        var dismissed = false
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReportOfferBanner(
                    onOpen = { opened = true },
                    onDismiss = { dismissed = true },
                )
            }
        }

        composeRule.onNodeWithTag(ReportTags.OFFER_BANNER).assertIsDisplayed()
        composeRule.onNodeWithText("Screenshot captured").assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.OFFER_OPEN).performClick()
        assertTrue(opened)

        composeRule.onNodeWithTag(ReportTags.OFFER_DISMISS).performClick()
        assertTrue(dismissed)
    }

    @Test
    fun readyOverlayShowsFallbackFrameNoteEntryAndDiagnosticDumpStatus() {
        val actions = mutableListOf<ReportAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReportOverlayContent(
                    state = ReportUiState(
                        imageBytes = null,
                        note = "",
                        signedIn = true,
                        diagnosticDumpAttached = true,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(ReportTags.OVERLAY).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.FRAME).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.FRAME_FALLBACK).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.DUMP_STATUS).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.SEND).assertIsEnabled()

        composeRule.onNodeWithTag(ReportTags.NOTE).performTextInput("Scroll jumped to top")
        assertEquals(ReportAction.SetNote("Scroll jumped to top"), actions.last())

        composeRule.onNodeWithTag(ReportTags.SEND).performClick()
        assertEquals(ReportAction.Send, actions.last())
    }

    @Test
    fun signedOutOverlayExplainsAndDisablesSend() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Dark, photographed = true) {
                ReportOverlayContent(
                    state = ReportUiState(
                        signedIn = false,
                        diagnosticDumpAttached = false,
                    ),
                    onAction = {},
                )
            }
        }

        composeRule.onNodeWithTag(ReportTags.SIGNED_OUT).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.SEND).assertIsNotEnabled()
        composeRule.onNodeWithText("Includes this screenshot. No diagnostic dump is attached.").assertIsDisplayed()
    }

    @Test
    fun markListSupportsEditingMarkNotesAndRemovingMarks() {
        val actions = mutableListOf<ReportAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReportOverlayContent(
                    state = ReportUiState(
                        marks = listOf(
                            ReportMarkUi(left = 0.1f, top = 0.2f, right = 0.6f, bottom = 0.4f, note = ""),
                            ReportMarkUi(left = 0.2f, top = 0.5f, right = 0.8f, bottom = 0.7f, note = "Clipped"),
                        ),
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(ReportTags.markRow(0)).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.markNote(0)).performScrollTo().performTextInput("Wrong badge")
        assertEquals(ReportAction.UpdateMarkNote(0, "Wrong badge"), actions.last())

        composeRule.onNodeWithTag(ReportTags.markRow(1)).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.markRemove(1)).performScrollTo().performClick()
        assertEquals(ReportAction.RemoveMark(1), actions.last())
    }

    @Test
    fun sendingShowsStatusWithoutSpinnerAndFailedOffersRetry() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReportOverlayContent(
                    state = ReportUiState(
                        delivery = ReportDeliveryUi.Sending,
                    ),
                    onAction = {},
                )
            }
        }

        composeRule.onNodeWithTag(ReportTags.SENDING).assertIsDisplayed()
        composeRule.onNodeWithText("Sending…").assertIsDisplayed()
    }

    @Test
    fun failedStateShowsRefusalReasonAndRetriesOnTap() {
        val actions = mutableListOf<ReportAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReportOverlayContent(
                    state = ReportUiState(
                        delivery = ReportDeliveryUi.Failed("network unavailable"),
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(ReportTags.REFUSAL).assertIsDisplayed()
        composeRule.onNodeWithText("Not sent: network unavailable").assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.RETRY).performClick()
        assertEquals(ReportAction.Send, actions.last())
    }

    @Test
    fun sentReceiptDisplaysReferenceCopyActionAndDone() {
        val actions = mutableListOf<ReportAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReportOverlayContent(
                    state = ReportUiState(
                        delivery = ReportDeliveryUi.Sent(referenceId = "rep_01J9XK2M"),
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(ReportTags.SENT).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.RECEIPT_ID).assertTextContains("rep_01J9XK2M")

        composeRule.onNodeWithTag(ReportTags.COPY_RECEIPT).performClick()
        assertEquals(ReportAction.CopyReference("rep_01J9XK2M"), actions.last())
        composeRule.onNodeWithText("Copied").assertIsDisplayed()

        composeRule.onNodeWithTag(ReportTags.DONE).performClick()
        assertEquals(ReportAction.Dismiss, actions.last())
    }

    @Test
    fun normalizedMarkClampsAndOrdersReversedCoordinates() {
        val mark = ReportMarkUi.normalized(x1 = 1.2f, y1 = 0.8f, x2 = -0.1f, y2 = 0.25f, note = "Edge")
        assertEquals(0f, mark.left, 0.0001f)
        assertEquals(0.25f, mark.top, 0.0001f)
        assertEquals(1f, mark.right, 0.0001f)
        assertEquals(0.8f, mark.bottom, 0.0001f)
        assertEquals("Edge", mark.note)
    }
}
