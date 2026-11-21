package sh.amux.features

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTextInputSelection
import androidx.compose.ui.test.performTextReplacement
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
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
import sh.amux.values.Activity
import sh.amux.values.ActivityKind
import sh.amux.values.AttachmentView
import sh.amux.values.BlobRef
import sh.amux.values.CommandMatch
import sh.amux.values.CommandView
import sh.amux.values.Composer
import sh.amux.values.ComposerView
import sh.amux.values.ControlsSummary
import sh.amux.values.ExitCause
import sh.amux.values.GroupState
import sh.amux.values.JobStop
import sh.amux.values.Kind
import sh.amux.values.Lands
import sh.amux.values.Offline
import sh.amux.values.PhaseView
import sh.amux.values.QueuedRow
import sh.amux.values.Reach
import sh.amux.values.Refusal
import sh.amux.values.RefusalReason
import sh.amux.values.RefusedPrompt
import sh.amux.values.Segment
import sh.amux.values.SentPrompt
import sh.amux.values.SignInState
import sh.amux.values.SignInView
import sh.amux.values.Underway
import sh.amux.values.Waiting

@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class ChatComposerTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun sendDisabledWhenEmptyAndEnabledWithDraft() {
        var sentCount = 0
        val emptyState = ChatComposerState(
            composer = ComposerView(mode = Composer.Send, switchedAway = false),
            draft = "",
            canSend = true,
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatComposer(
                    state = emptyState,
                    callbacks = ChatComposerCallbacks(onSend = { sentCount++ }),
                )
            }
        }

        composeRule.onNodeWithTag(ChatComposerTags.SEND_BUTTON).assertIsNotEnabled()
    }

    @Test
    fun sendEnabledWithDraftDispatchesOnSend() {
        var sentCount = 0
        val state = ChatComposerState(
            composer = ComposerView(mode = Composer.Send, switchedAway = false),
            draft = "Run the migration tests",
            canSend = true,
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatComposer(
                    state = state,
                    callbacks = ChatComposerCallbacks(onSend = { sentCount++ }),
                )
            }
        }

        composeRule.onNodeWithTag(ChatComposerTags.SEND_BUTTON).assertIsEnabled().performClick()
        assertEquals(1, sentCount)
    }

    @Test
    fun workingWithoutDraftShowsInterruptAndActivityWithSubagents() {
        var interrupted = false
        val state = ChatComposerState(
            composer = ComposerView(
                mode = Composer.Send,
                switchedAway = false,
                activity = Activity(
                    kind = ActivityKind.Subagents(count = 3),
                    sinceMs = 1_000L,
                    elapsedMs = 12_000L,
                    turnElapsedMs = 12_000L,
                ),
            ),
            phase = PhaseView.Working,
            draft = "",
            liveSubagents = GroupState(
                agents = 3,
                running = 2,
                needsYou = 1,
                idle = 0,
                finished = 0,
                ended = 0,
                failed = 0,
                unknown = 0,
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatComposer(
                    state = state,
                    callbacks = ChatComposerCallbacks(onInterrupt = { interrupted = true }),
                )
            }
        }

        composeRule.onNodeWithTag(ChatComposerTags.ACTIVITY).assertIsDisplayed()
        composeRule.onNodeWithText("3 subagents · 2 running · 1 waiting on you").assertIsDisplayed()
        composeRule.onNodeWithTag(ChatComposerTags.INTERRUPT_BUTTON).assertIsDisplayed().performClick()
        assertTrue(interrupted)
    }

    @Test
    fun resumeModeAllowsEmptyDraftAndDraftAndDispatchesOnResume() {
        var resumeCount = 0
        val emptyState = ChatComposerState(
            composer = ComposerView(mode = Composer.Resume, switchedAway = false),
            phase = PhaseView.Exited(ExitCause.Finished),
            draft = "",
            canResume = true,
            host = "desk",
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatComposer(
                    state = emptyState,
                    callbacks = ChatComposerCallbacks(onResume = { resumeCount += 1 }),
                )
            }
        }

        composeRule.onNodeWithTag(ChatComposerTags.EXITED_HEAD).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatComposerTags.RESUME_BUTTON).assertIsEnabled().performClick()
        assertEquals(1, resumeCount)
    }

    @Test
    fun resumeModeWithDraftDispatchesOnResume() {
        var resumed = false
        val state = ChatComposerState(
            composer = ComposerView(mode = Composer.Resume, switchedAway = false),
            phase = PhaseView.Exited(ExitCause.Finished),
            draft = "Pick up from step 3",
            canResume = true,
            host = "desk",
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatComposer(
                    state = state,
                    callbacks = ChatComposerCallbacks(onResume = { resumed = true }),
                )
            }
        }

        composeRule.onNodeWithTag(ChatComposerTags.EXITED_HEAD).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatComposerTags.RESUME_BUTTON).assertIsEnabled().performClick()
        assertTrue(resumed)
    }

    @Test
    fun waitingModeDisablesSendAndShowsKeptDraftPlaceholder() {
        val state = ChatComposerState(
            composer = ComposerView(mode = Composer.Waiting(Waiting.Detached), switchedAway = false),
            draft = "",
            host = "desk",
            reach = Reach.Offline(Offline.Plain),
            canSend = true,
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatComposer(state = state)
            }
        }

        composeRule.onNodeWithText("desk is out of reach · your draft is kept").assertIsDisplayed()
        composeRule.onNodeWithTag(ChatComposerTags.SEND_BUTTON).assertIsNotEnabled()
    }

    @Test
    fun nativeSubagentComposerShowsRunnerStopAndRefusal() {
        val stoppedKeys = mutableListOf<String>()
        val state = ChatComposerState(
            composer = ComposerView(
                mode = Composer.Native(
                    stop = JobStop.Refused(key = "sub-7", error = "Subagent already finishing"),
                ),
                switchedAway = false,
            ),
            kind = Kind.Codex,
            phase = PhaseView.Working,
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatComposer(
                    state = state,
                    callbacks = ChatComposerCallbacks(onStopNative = { stoppedKeys += it }),
                )
            }
        }

        composeRule.onNodeWithTag(ChatComposerTags.NATIVE_COMPOSER).assertIsDisplayed()
        composeRule.onNodeWithText("Codex runs this subagent").assertIsDisplayed()
        composeRule.onNodeWithText("It takes no messages.").assertIsDisplayed()
        composeRule.onNodeWithTag(ChatComposerTags.NATIVE_REFUSED).assertIsDisplayed()
        assertEquals(0, composeRule.onAllNodesWithTag(ChatComposerTags.INPUT_FIELD).fetchSemanticsNodes().size)
        composeRule.onNodeWithTag(ChatComposerTags.NATIVE_STOP).performClick()
        assertEquals(listOf("sub-7"), stoppedKeys)
    }

    @Test
    fun queuedUnderwayAndRefusedPromptsDispatchActions() {
        val sentNow = mutableListOf<QueuedRow>()
        val withdrawn = mutableListOf<QueuedRow>()
        val resent = mutableListOf<List<Int>>()
        val discarded = mutableListOf<List<Int>>()
        val dismissedRefused = mutableListOf<List<Int>>()

        val queuedRow = QueuedRow(
            inputId = listOf(1),
            text = listOf(Segment.Text("Check lint too")),
            steered = true,
            canWithdraw = true,
            canSendNow = true,
        )
        val underwayRow = SentPrompt(
            inputId = listOf(2),
            text = listOf(Segment.Text("Uncertain message")),
            lands = Lands.Queue,
            underway = Underway.MayNotHaveArrived,
        )
        val refusedRow = RefusedPrompt(
            inputId = listOf(3),
            reason = Refusal(reason = RefusalReason.HostUnreachable, detail = null),
        )

        val state = ChatComposerState(
            composer = ComposerView(mode = Composer.Send, switchedAway = false),
            queued = listOf(queuedRow),
            underway = listOf(underwayRow),
            refused = listOf(refusedRow),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatComposer(
                    state = state,
                    callbacks = ChatComposerCallbacks(
                        onSendNow = { sentNow += it },
                        onWithdraw = { withdrawn += it },
                        onResend = { resent += it },
                        onDiscard = { discarded += it },
                        onDismissRefused = { dismissedRefused += it },
                    ),
                )
            }
        }

        composeRule.onNodeWithText("Check lint too").assertIsDisplayed()
        composeRule.onNodeWithText("steered").assertIsDisplayed()
        composeRule.onNodeWithTag(ChatComposerTags.queuedSendNow(0)).performClick()
        composeRule.onNodeWithTag(ChatComposerTags.queuedWithdraw(0)).performClick()
        composeRule.onNodeWithTag(ChatComposerTags.underwayResend(0)).performClick()
        composeRule.onNodeWithTag(ChatComposerTags.underwayDiscard(0)).performClick()
        composeRule.onNodeWithTag(ChatComposerTags.refusedDismiss(0)).performClick()

        assertEquals(listOf(queuedRow), sentNow)
        assertEquals(listOf(queuedRow), withdrawn)
        assertEquals(listOf(listOf(2)), resent)
        assertEquals(listOf(listOf(2)), discarded)
        assertEquals(listOf(listOf(3)), dismissedRefused)
    }

    @Test
    fun slashPopupLimitsToFiveItemsAndSelectsCommand() {
        val picked = mutableListOf<CommandMatch>()
        val matches = (1..7).map { idx ->
            CommandMatch(
                command = CommandView(
                    name = "cmd$idx",
                    description = "Run command $idx",
                    argumentHint = "<arg>",
                    source = "built-in",
                    sigil = "/",
                    anywhere = false,
                ),
                picked = "/cmd$idx ",
            )
        }
        val state = ChatComposerState(
            composer = ComposerView(mode = Composer.Send, switchedAway = false),
            draft = "/cmd",
            slashMatches = matches,
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatComposer(
                    state = state,
                    callbacks = ChatComposerCallbacks(onPickCommand = { picked += it }),
                )
            }
        }

        composeRule.onNodeWithTag(ChatComposerTags.SLASH_POPUP).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatComposerTags.slashMatch("cmd5")).assertIsDisplayed()
        assertEquals(0, composeRule.onAllNodesWithTag(ChatComposerTags.slashMatch("cmd6")).fetchSemanticsNodes().size)
        composeRule.onNodeWithTag(ChatComposerTags.slashMatch("cmd2")).performClick()
        assertEquals(listOf(matches[1]), picked)
    }

    @Test
    fun switchedAwaySignInAttachmentsAndControlsStripWork() {
        var switchedBack = false
        var openedSettings = false
        var openedOverview = false
        val removed = mutableListOf<Int>()
        val attached = mutableListOf<AttachChoice>()

        val blob = BlobRef(hash = listOf(4, 5), name = "screenshot.png", mime = "image/png", size = 512_000L)
        val state = ChatComposerState(
            composer = ComposerView(mode = Composer.Send, switchedAway = true),
            host = "studio",
            kind = Kind.ClaudePty,
            signIn = SignInView(state = SignInState.SignedOut, account = "ada@example.com", message = "Session expired."),
            attachments = listOf(ComposerAttachmentUi(id = "att-1", view = AttachmentView.Image(blob))),
            notice = "File exceeds the 64 MB limit.",
            controls = ControlsSummary(model = "Sonnet 4.6", effort = "high"),
            showOverviewChip = true,
            overviewSummary = "3/7 tasks",
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatComposer(
                    state = state,
                    callbacks = ChatComposerCallbacks(
                        onSwitchBack = { switchedBack = true },
                        onRemoveAttachment = { removed += it },
                        onOpenSettings = { openedSettings = true },
                        onOpenOverview = { openedOverview = true },
                        onAttach = { attached += it },
                    ),
                )
            }
        }

        composeRule.onNodeWithTag(ChatComposerTags.SWITCHED_AWAY).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatComposerTags.SWITCH_BACK).performClick()
        assertTrue(switchedBack)

        composeRule.onNodeWithTag(ChatComposerTags.SIGN_IN_NOTICE).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatComposerTags.NOTICE).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatComposerTags.ATTACHMENTS_ROW).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatRowTags.ATTACHMENT_REMOVE).performClick()
        assertEquals(listOf(0), removed)

        composeRule.onNodeWithTag(ChatComposerTags.MODEL_CHIP).performClick()
        composeRule.onNodeWithTag(ChatComposerTags.OVERVIEW_CHIP).performClick()
        assertTrue(openedSettings)
        assertTrue(openedOverview)

        composeRule.onNodeWithTag(ChatComposerTags.ATTACH_BUTTON).performClick()
        composeRule.onNodeWithTag(ChatComposerTags.ATTACH_MENU_PHOTO).performClick()
        assertEquals(listOf(AttachChoice.Photo), attached)
    }

    /**
     * Stands in for the chat model's typing: a long run of inserted text
     * becomes a chip and leaves the draft as it was around it, as
     * `ChatModel.type` does. [answering] off holds answers back, as a model
     * whose state has not reached the screen yet.
     */
    private class TypingModel(initial: String) {
        var draft by mutableStateOf(initial)
        var edits by mutableIntStateOf(0)
        var answering = true
        var heard = initial
        val chips = mutableListOf<String>()

        fun type(text: String) {
            val old = heard
            var head = 0
            while (head < old.length && head < text.length && old[head] == text[head]) head++
            var tail = 0
            while (tail < old.length - head && tail < text.length - head &&
                old[old.length - 1 - tail] == text[text.length - 1 - tail]
            ) {
                tail++
            }
            val inserted = text.substring(head, text.length - tail)
            heard = if (inserted.length >= 40) {
                chips += inserted
                text.substring(0, head) + text.substring(text.length - tail)
            } else {
                text
            }
            if (answering) answer()
        }

        fun answer() {
            draft = heard
            edits += 1
        }
    }

    private fun composerOver(model: TypingModel) {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatComposer(
                    state = ChatComposerState(
                        composer = ComposerView(mode = Composer.Send, switchedAway = false),
                        draft = model.draft,
                        draftEdits = model.edits,
                    ),
                    callbacks = ChatComposerCallbacks(onDraftChange = model::type),
                )
            }
        }
    }

    private fun fieldShows(text: String) {
        composeRule.onNodeWithTag(ChatComposerTags.INPUT_FIELD)
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.EditableText, AnnotatedString(text)))
    }

    private val longPaste = "a pasted log line that runs on well past forty characters"

    @Test
    fun aLongPasteIntoAnEmptyFieldLeavesOnlyTheChip() {
        val model = TypingModel("")
        composerOver(model)

        composeRule.onNodeWithTag(ChatComposerTags.INPUT_FIELD).performTextInput(longPaste)
        composeRule.waitForIdle()

        assertEquals(listOf(longPaste), model.chips)
        fieldShows("")
    }

    @Test
    fun aLongPasteMidSentenceLeavesTheWordsAroundTheChip() {
        val model = TypingModel("hello world")
        composerOver(model)

        val input = composeRule.onNodeWithTag(ChatComposerTags.INPUT_FIELD)
        input.performTextInputSelection(TextRange(6))
        input.performTextInput(longPaste)
        composeRule.waitForIdle()

        assertEquals(listOf(longPaste), model.chips)
        fieldShows("hello world")
        // Typing goes on where the chip was put.
        input.performTextInput("big ")
        composeRule.waitForIdle()
        fieldShows("hello big world")
        assertEquals("hello big world", model.draft)
    }

    @Test
    fun aRewriteSeenAfterTheFieldMovedOnDoesNotUndoTyping() {
        val model = TypingModel("hello")
        composerOver(model)
        val input = composeRule.onNodeWithTag(ChatComposerTags.INPUT_FIELD)
        input.performTextInputSelection(TextRange(5))

        // The person types while the model's answers have not reached the
        // screen; meanwhile an older rewrite (a send clearing the draft) does.
        composeRule.runOnIdle { model.answering = false }
        input.performTextInput("x")
        composeRule.runOnIdle { model.draft = "" }
        composeRule.waitForIdle()
        fieldShows("hellox")

        // Then the answer arrives, and the person deletes what they typed.
        composeRule.runOnIdle {
            model.answering = true
            model.answer()
        }
        composeRule.waitForIdle()
        input.performTextReplacement("hello")
        composeRule.waitForIdle()

        fieldShows("hello")
        assertEquals("hello", model.draft)
        assertEquals("hello", model.heard)
    }
}
