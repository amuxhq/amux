package sh.amux.features

import android.graphics.Bitmap
import android.graphics.Color
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsNotSelected
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.swipe
import java.io.ByteArrayOutputStream
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
@GraphicsMode(GraphicsMode.Mode.NATIVE)
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
                        dumpReady = true,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(ReportTags.OVERLAY).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.FRAME).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.FRAME_FALLBACK).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.DUMP_STATUS).assertIsDisplayed()
        composeRule.onNodeWithText(
            "Includes this phone’s diagnostic dump. Diagnostic dumps never include your conversations.",
        ).assertIsDisplayed()
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
                        imageBytes = byteArrayOf(1, 2, 3),
                        signedIn = false,
                        dumpReady = false,
                    ),
                    onAction = {},
                )
            }
        }

        composeRule.onNodeWithTag(ReportTags.SIGNED_OUT).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.SEND).assertIsNotEnabled()
        composeRule.onNodeWithText(
            "Includes this screenshot (with any hidden areas blacked out). No diagnostic dump is attached.",
        ).assertIsDisplayed()
    }

    @Test
    fun markListSupportsEditingNotesTogglingRedactionAndRemovingMarks() {
        val actions = mutableListOf<ReportAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReportOverlayContent(
                    state = ReportUiState(
                        imageBytes = byteArrayOf(1, 2, 3),
                        marks = listOf(
                            ReportMarkUi(left = 0.1f, top = 0.2f, right = 0.6f, bottom = 0.4f, note = "", redact = false),
                            ReportMarkUi(left = 0.2f, top = 0.5f, right = 0.8f, bottom = 0.7f, note = "Clipped", redact = true),
                        ),
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithText(
            "Includes this screenshot (with any hidden areas blacked out) and this phone’s diagnostic dump. Diagnostic dumps never include your conversations.",
        ).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.markRow(0)).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.markNote(0)).performScrollTo().performTextInput("Wrong badge")
        assertEquals(ReportAction.UpdateMarkNote(0, "Wrong badge"), actions.last())

        composeRule.onNodeWithTag(ReportTags.markRedact(0)).performScrollTo().performClick()
        assertEquals(ReportAction.ToggleMarkRedact(0), actions.last())

        composeRule.onNodeWithTag(ReportTags.markRow(1)).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.markRemove(1)).performScrollTo().performClick()
        assertEquals(ReportAction.RemoveMark(1), actions.last())
    }

    @Test
    fun sendingShowsStatusWithoutSpinner() {
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

    @Test
    fun pendingScreenshotDisablesSendAndPendingDumpIsNotClaimedAsAttached() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReportOverlayContent(
                    state = ReportUiState(
                        imageBytes = null,
                        signedIn = true,
                        screenshotPending = true,
                        screenshotPresent = false,
                        dumpPending = true,
                        dumpReady = false,
                    ),
                    onAction = {},
                )
            }
        }

        composeRule.onNodeWithTag(ReportTags.FRAME_PREPARING).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.FRAME_FALLBACK).assertDoesNotExist()
        composeRule.onNodeWithText("Preparing screenshot and diagnostic dump…").assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.SEND).assertIsNotEnabled()
    }

    @Test
    fun fittedImageRectMapsPillarboxedAndLetterboxedBoundsAccurately() {
        val pillarboxed = computeFittedImageRect(
            containerWidth = 300f,
            containerHeight = 300f,
            imageWidth = 100f,
            imageHeight = 300f,
        )
        assertEquals(100f, pillarboxed.left, 0.01f)
        assertEquals(0f, pillarboxed.top, 0.01f)
        assertEquals(200f, pillarboxed.right, 0.01f)
        assertEquals(300f, pillarboxed.bottom, 0.01f)

        val letterboxed = computeFittedImageRect(
            containerWidth = 300f,
            containerHeight = 300f,
            imageWidth = 300f,
            imageHeight = 100f,
        )
        assertEquals(0f, letterboxed.left, 0.01f)
        assertEquals(100f, letterboxed.top, 0.01f)
        assertEquals(300f, letterboxed.right, 0.01f)
        assertEquals(200f, letterboxed.bottom, 0.01f)
    }

    @Test
    fun dragOnPillarboxedScreenshotMapsCoordinatesToFittedImageBounds() {
        val narrowPng = encodeSolidPng(width = 40, height = 400)
        val addedMarks = mutableListOf<ReportMarkUi>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReportOverlayContent(
                    state = ReportUiState(
                        imageBytes = narrowPng,
                        frameAspectRatio = 40f / 400f, // clamped to 0.3f -> pillarboxed in center 1/3
                        signedIn = true,
                        dumpReady = true,
                    ),
                    onAction = { action ->
                        if (action is ReportAction.AddMark) addedMarks += action.mark
                    },
                )
            }
        }

        val frameNode = composeRule.onNodeWithTag(ReportTags.FRAME)
        val layoutSize = frameNode.fetchSemanticsNode().size
        val w = layoutSize.width.toFloat()
        val h = layoutSize.height.toFloat()

        // Drag purely in the empty left pillarbox margin (0%..20% of canvas) -> must not create a mark.
        frameNode.performTouchInput {
            swipe(
                start = Offset(w * 0.05f, h * 0.10f),
                end = Offset(w * 0.20f, h * 0.35f),
            )
        }
        composeRule.waitForIdle()
        assertTrue("drag outside fitted image margin must not add a mark", addedMarks.isEmpty())

        // Drag across the pillarboxed image from the left margin (10% canvas) to the right margin (80% canvas).
        // Because the fitted image occupies the center third (~33%..67%), clamping to fitted bounds must yield [0..1].
        frameNode.performTouchInput {
            down(Offset(w * 0.10f, h * 0.10f))
            moveTo(Offset(w * 0.30f, h * 0.10f))
            moveTo(Offset(w * 0.80f, h * 0.35f))
            up()
        }
        composeRule.waitForIdle()
        assertEquals(1, addedMarks.size)
        val mark = addedMarks.single()
        assertEquals(0.0f, mark.left, 0.02f)
        assertEquals(1.0f, mark.right, 0.02f)
        assertEquals(0.10f, mark.top, 0.03f)
        assertEquals(0.35f, mark.bottom, 0.03f)
    }

    @Test
    fun sequentialDragsSelectNewlyDrawnMarkEachTime() {
        val png = encodeSolidPng(width = 200, height = 400)
        composeRule.setContent {
            var uiState by remember {
                mutableStateOf(
                    ReportUiState(
                        imageBytes = png,
                        frameAspectRatio = 0.5f,
                        signedIn = true,
                        dumpReady = true,
                    ),
                )
            }
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReportOverlayContent(
                    state = uiState,
                    onAction = { action ->
                        if (action is ReportAction.AddMark) {
                            uiState = uiState.copy(marks = uiState.marks + action.mark)
                        }
                    },
                )
            }
        }

        val frameNode = composeRule.onNodeWithTag(ReportTags.FRAME)
        val bounds = frameNode.fetchSemanticsNode().boundsInRoot
        val w = bounds.width
        val h = bounds.height

        frameNode.performTouchInput {
            swipe(
                start = Offset(w * 0.1f, h * 0.1f),
                end = Offset(w * 0.4f, h * 0.35f),
            )
        }
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(ReportTags.markRow(0)).performScrollTo().assertIsSelected()

        frameNode.performScrollTo()
        frameNode.performTouchInput {
            swipe(
                start = Offset(w * 0.5f, h * 0.5f),
                end = Offset(w * 0.85f, h * 0.8f),
            )
        }
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(ReportTags.markRow(0)).performScrollTo().assertIsNotSelected()
        composeRule.onNodeWithTag(ReportTags.markRow(1)).performScrollTo().assertIsSelected()
    }

    private fun encodeSolidPng(width: Int, height: Int): ByteArray {
        val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        bitmap.eraseColor(Color.LTGRAY)
        return try {
            ByteArrayOutputStream().use { out ->
                bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
                out.toByteArray()
            }
        } finally {
            bitmap.recycle()
        }
    }
}
