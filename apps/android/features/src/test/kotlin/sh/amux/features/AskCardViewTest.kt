package sh.amux.features

import androidx.compose.ui.test.assertTextEquals
import androidx.activity.OnBackPressedDispatcher
import androidx.activity.OnBackPressedDispatcherOwner
import androidx.activity.compose.LocalOnBackPressedDispatcherOwner
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.heightIn
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleRegistry
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.text.AnnotatedString
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
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.CardState
import sh.amux.values.Choice
import sh.amux.values.ChoiceOutcome
import sh.amux.values.FilePatch
import sh.amux.values.HunkStart
import sh.amux.values.PatchHunk
import sh.amux.values.FieldProblem
import sh.amux.values.FormField
import sh.amux.values.FormFieldKind
import sh.amux.values.FormValue
import sh.amux.values.Kind
import sh.amux.values.LineKind
import sh.amux.values.OptionView
import sh.amux.values.PatchLine
import sh.amux.values.QuestionResponse
import sh.amux.values.QuestionView
import sh.amux.values.Refusal
import sh.amux.values.RefusalReason
import sh.amux.values.Scope

@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class AskCardViewTest {
    @get:Rule
    val composeRule = createComposeRule()

    private fun card(
        body: AskBody,
        choices: List<Choice>,
        state: CardState = CardState.Open,
        position: Long = 1L,
        count: Long = 1L,
        questionNote: Boolean = false,
        questionSkip: Boolean = false,
        questionReply: Boolean = false,
        stopsTurn: Boolean = true,
    ): AskCard = AskCard(
        kind = Kind.ClaudeSdk,
        key = "ask-1",
        itemKey = "item-1",
        position = position,
        count = count,
        body = body,
        choices = choices,
        questionNote = questionNote,
        questionSkip = questionSkip,
        questionReply = questionReply,
        stopsTurn = stopsTurn,
        defaultToNo = false,
        state = state,
    )

    @Test
    fun permissionCommandShowsScopeAllowDenyWithNoteAndInterrupt() {
        val actions = mutableListOf<AskAction>()
        var interrupted = false
        val askCard = card(
            body = AskBody.Command(
                command = "cargo test -p store",
                cwd = "/Users/ada/amux",
                reason = "Verify migrations",
                description = "",
            ),
            choices = listOf(
                Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false),
                Choice(
                    ChoiceOutcome.AllowAlways(
                        subjects = listOf("cargo test"),
                        directories = emptyList(),
                        mode = "",
                        modeName = "",
                        scope = Scope.Project,
                    ),
                    primary = false,
                    takesNote = false,
                ),
                Choice(ChoiceOutcome.Deny, primary = false, takesNote = true),
            ),
            position = 1L,
            count = 3L,
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(
                    card = askCard,
                    onAction = { actions += it },
                    onInterrupt = { interrupted = true },
                )
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.ASK_POSITION).assertIsDisplayed()
        composeRule.onNodeWithText("1 of 3").assertIsDisplayed()
        composeRule.onNodeWithText("cargo test -p store").assertIsDisplayed()

        // Allow primary choice
        composeRule.onNodeWithTag(AskReviewTags.askChoice(0)).performClick()
        assertEquals(AskAction.Choose(0, null, askKey = "ask-1"), actions.last())

        // Always allow scope row
        composeRule.onNodeWithTag(AskReviewTags.askChoice(1)).performClick()
        assertEquals(AskAction.Choose(1, null, askKey = "ask-1"), actions.last())

        // Deny with note flow
        composeRule.onNodeWithTag(AskReviewTags.ASK_DENY).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_NOTE).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.ASK_NOTE).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Run only unit tests"))
        }
        composeRule.onNodeWithTag(AskReviewTags.askChoice(2)).performClick()
        assertEquals(AskAction.Choose(2, "Run only unit tests", askKey = "ask-1"), actions.last())

        // Overflow menu interrupt escape
        composeRule.onNodeWithTag(AskReviewTags.ASK_MORE).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_INTERRUPT_MENU).performClick()
        assertTrue(interrupted)
    }

    @Test
    fun permissionEditRendersDiffHunksAndLines() {
        val actions = mutableListOf<AskAction>()
        val askCard = card(
            body = AskBody.Edit(
                path = "src/main.rs",
                files = 1,
                added = 2,
                removed = 1,
                patch = listOf(
                    FilePatch(
                        hunks = listOf(
                            PatchHunk(
                                start = HunkStart(old = 10, new = 10),
                                lines = listOf(
                                    PatchLine(LineKind.Removed, "let old = 1;", number = 10),
                                    PatchLine(LineKind.Added, "let next = 2;", number = 10),
                                    PatchLine(LineKind.Added, "let more = 3;", number = 11),
                                ),
                            ),
                        ),
                    ),
                ),
                reason = "Update value",
            ),
            choices = listOf(
                Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false),
                Choice(ChoiceOutcome.Deny, primary = false, takesNote = false),
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = askCard, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithText("src/main.rs").assertIsDisplayed()
        // A hunk reads as where it starts; each line carries its sign.
        composeRule.onNodeWithText("Line 10").assertIsDisplayed()
        composeRule.onNodeWithText("+let next = 2;").assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.askChoice(0)).performClick()
        assertEquals(AskAction.Choose(0, null, askKey = "ask-1"), actions.last())
    }

    @Test
    fun singlePickQuestionWithoutPreviewsAnswersImmediatelyOnTap() {
        val actions = mutableListOf<AskAction>()
        val askCard = card(
            body = AskBody.Question(
                listOf(
                    QuestionView(
                        header = "Database",
                        question = "Which backend should we use?",
                        multiSelect = false,
                        options = listOf(
                            OptionView("SQLite", "Embedded local store", "", recommended = true),
                            OptionView("Postgres", "External server", "", recommended = false),
                        ),
                        allowOther = true,
                        secret = false,
                    ),
                ),
            ),
            choices = emptyList(),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = askCard, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithText("RECOMMENDED").assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.askOption(0)).performClick()
        assertEquals(
            AskAction.Respond(listOf(QuestionResponse(note = null, other = null, picked = listOf(0))), askKey = "ask-1"),
            actions.last(),
        )
    }

    @Test
    fun multiSelectQuestionKeepsSelectionsWhenTypingSomethingElse() {
        val actions = mutableListOf<AskAction>()
        val askCard = card(
            body = AskBody.Question(
                listOf(
                    QuestionView(
                        header = "Targets",
                        question = "Which platforms should we build?",
                        multiSelect = true,
                        options = listOf(
                            OptionView("Android", "APK build", "", recommended = false),
                            OptionView("iOS", "IPA build", "", recommended = false),
                        ),
                        allowOther = true,
                        secret = false,
                    ),
                ),
            ),
            choices = emptyList(),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = askCard, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.askOption(0)).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_OPTION_OTHER).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_OTHER_INPUT).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Linux"))
        }
        composeRule.onNodeWithTag(AskReviewTags.ASK_NEXT).performClick()

        assertEquals(
            AskAction.Respond(listOf(QuestionResponse(note = null, other = "Linux", picked = listOf(0))), askKey = "ask-1"),
            actions.last(),
        )
    }

    @Test
    fun multiQuestionStepperReviewsAndSendsAnswers() {
        val actions = mutableListOf<AskAction>()
        val askCard = card(
            body = AskBody.Question(
                listOf(
                    QuestionView(
                        header = "Style",
                        question = "Pick a format",
                        multiSelect = false,
                        options = listOf(OptionView("JSON", "", "", false)),
                        allowOther = false,
                        secret = false,
                    ),
                    QuestionView(
                        header = "Token",
                        question = "Enter secret token",
                        multiSelect = false,
                        options = emptyList(),
                        allowOther = true,
                        secret = true,
                    ),
                ),
            ),
            choices = emptyList(),
            questionSkip = true,
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = askCard, onAction = { actions += it })
            }
        }

        // Answer question 1 -> advances to question 2
        composeRule.onNodeWithTag(AskReviewTags.askOption(0)).performClick()
        // Skip question 2 -> advances to review
        composeRule.onNodeWithTag(AskReviewTags.ASK_SKIP).performClick()
        composeRule.onNodeWithText("JSON").assertIsDisplayed()
        composeRule.onNodeWithText("Skipped").assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.ASK_SEND).assertIsEnabled().performClick()

        assertEquals(
            AskAction.Respond(
                listOf(
                    QuestionResponse(note = null, other = null, picked = listOf(0)),
                    QuestionResponse(note = null, other = null, picked = emptyList()),
                ),
                askKey = "ask-1",
            ),
            actions.last(),
        )
    }

    @Test
    fun planAskSupportsModeSwitchAndSendBackWithNote() {
        val actions = mutableListOf<AskAction>()
        val askCard = card(
            body = AskBody.Plan(
                title = "Refactor cache",
                body = "1. Split read and write paths.\n2. Add unit tests.",
            ),
            choices = listOf(
                Choice(ChoiceOutcome.ApprovePlan(mode = "", modeName = ""), primary = true, takesNote = false),
                Choice(ChoiceOutcome.ApprovePlan(mode = "auto", modeName = "auto"), primary = false, takesNote = false),
                Choice(ChoiceOutcome.SendBack, primary = false, takesNote = true),
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = askCard, onAction = { actions += it })
            }
        }

        // The card is the decision; the plan itself reads in the transcript.
        composeRule.onNodeWithText("Refactor cache").assertDoesNotExist()
        // Switching on picks the approval that changes mode; the button
        // still says Approve.
        composeRule.onNodeWithTag(AskReviewTags.ASK_PLAN_SWITCH).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_PLAN_APPROVE).assertTextEquals("Approve").performClick()
        assertEquals(AskAction.Choose(1, null, askKey = "ask-1"), actions.last())

        // Send back requires a note
        composeRule.onNodeWithTag(AskReviewTags.askChoice(2)).performClick()
        composeRule.onNodeWithTag(AskReviewTags.askChoice(2)).assertIsNotEnabled()
        composeRule.onNodeWithTag(AskReviewTags.ASK_NOTE).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Keep the existing schema"))
        }
        composeRule.onNodeWithTag(AskReviewTags.askChoice(2)).assertIsEnabled().performClick()
        assertEquals(AskAction.Choose(2, "Keep the existing schema", askKey = "ask-1"), actions.last())
    }

    @Test
    fun formAskValidatesProblemsAndSubmitsValidValues() {
        val actions = mutableListOf<AskAction>()
        val fields = listOf(
            FormField(
                name = "retries",
                title = "Retries",
                description = "",
                required = true,
                kind = FormFieldKind.Number(integer = true, minimum = 1.0, maximum = 10.0),
                initial = FormValue.Text(""),
            ),
            FormField(
                name = "dry_run",
                title = "Dry run",
                description = "",
                required = false,
                kind = FormFieldKind.Toggle,
                initial = FormValue.Toggle(false),
            ),
        )
        val askCard = card(
            body = AskBody.Form(
                server = "deploy",
                message = "Configure rollout",
                fields = fields,
            ),
            choices = listOf(
                Choice(ChoiceOutcome.Submit, primary = true, takesNote = false),
                Choice(ChoiceOutcome.Decline, primary = false, takesNote = false),
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = askCard, onAction = { actions += it })
            }
        }

        // Required field empty -> submit disabled
        composeRule.onNodeWithTag(AskReviewTags.ASK_SUBMIT).assertIsNotEnabled()

        // Invalid number -> problem shown, submit still disabled
        composeRule.onNodeWithTag(AskReviewTags.askField("retries")).performTextInput("abc")
        composeRule.onNodeWithTag(AskReviewTags.askFieldProblem("retries")).assertIsDisplayed()
        composeRule.onNodeWithText("Enter a number").assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.ASK_SUBMIT).assertIsNotEnabled()

        // Fix number & toggle dry run -> submit enabled
        composeRule.onNodeWithTag(AskReviewTags.askField("retries")).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("3"))
        }
        composeRule.onNodeWithTag(AskReviewTags.askField("dry_run")).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_SUBMIT).assertIsEnabled().performClick()

        assertEquals(
            AskAction.Submit(listOf(FormValue.Text("3"), FormValue.Toggle(true)), askKey = "ask-1"),
            actions.last(),
        )
        assertEquals(listOf(FieldProblem.NotANumber), validateFormFields(fields, listOf(FormValue.Text("x"), FormValue.Toggle(false))).map { it.problem })
    }

    @Test
    fun linkAccessAndUnanswerableAsksEmitExpectedActions() {
        val actions = mutableListOf<AskAction>()
        val linkCard = card(
            body = AskBody.Link(
                server = "github",
                message = "Authorize access",
                url = "https://github.com/login/device",
                localPage = false,
            ),
            choices = listOf(
                Choice(ChoiceOutcome.OpenLink, primary = true, takesNote = false),
                Choice(ChoiceOutcome.Decline, primary = false, takesNote = false),
            ),
        )
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = linkCard, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.ASK_LINK_OPEN).performClick()
        assertEquals(AskAction.OpenUrl("https://github.com/login/device"), actions.last())
        composeRule.onNodeWithTag(AskReviewTags.askChoice(0)).performClick()
        assertEquals(AskAction.Choose(0, null, askKey = "ask-1"), actions.last())
    }

    @Test
    fun accessGrantShowsSummaryAndDispatchesTurnAndSession() {
        val actions = mutableListOf<AskAction>()
        val accessCard = card(
            body = AskBody.Access(
                reason = "Fetch build cache",
                read = listOf("/var/cache"),
                write = listOf("/tmp/out"),
                network = true,
                hosts = listOf("crates.io"),
            ),
            choices = listOf(
                Choice(ChoiceOutcome.GrantForTurn, primary = true, takesNote = false),
                Choice(ChoiceOutcome.GrantForSession, primary = false, takesNote = false),
                Choice(ChoiceOutcome.Deny, primary = false, takesNote = false),
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = accessCard, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithText("/tmp/out").assertIsDisplayed()
        composeRule.onNodeWithText("/var/cache").assertIsDisplayed()
        composeRule.onNodeWithText("crates.io").assertIsDisplayed()
        // One Grant, for as long as the How long choice says: this turn
        // until the person picks the session.
        composeRule.onNodeWithTag(AskReviewTags.ASK_GRANT).performClick()
        assertEquals(AskAction.Choose(0, null, askKey = "ask-1"), actions.last())
        composeRule.onNodeWithTag(AskReviewTags.ASK_GRANT_SESSION).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_GRANT).performClick()
        assertEquals(AskAction.Choose(1, null, askKey = "ask-1"), actions.last())
    }

    @Test
    fun accessGrantLengthSurvivesARefusal() {
        val actions = mutableListOf<AskAction>()
        val accessCard = card(
            body = AskBody.Access(
                reason = "Fetch build cache",
                read = emptyList(),
                write = listOf("/tmp/out"),
                network = false,
                hosts = emptyList(),
            ),
            choices = listOf(
                Choice(ChoiceOutcome.GrantForTurn, primary = true, takesNote = false),
                Choice(ChoiceOutcome.GrantForSession, primary = false, takesNote = false),
                Choice(ChoiceOutcome.Deny, primary = false, takesNote = false),
            ),
        )
        var currentCard by mutableStateOf(accessCard)
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = currentCard, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.ASK_GRANT_SESSION).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_GRANT).performClick()
        assertEquals(AskAction.Choose(1, null, askKey = "ask-1"), actions.last())

        // Sending swaps the body out; the refusal brings it back as it was.
        currentCard = accessCard.copy(state = CardState.Sending)
        composeRule.waitForIdle()
        currentCard = accessCard.copy(state = CardState.Rejected(Refusal(RefusalReason.HostUnreachable)))
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(AskReviewTags.ASK_GRANT).performClick()
        assertEquals(AskAction.Choose(1, null, askKey = "ask-1"), actions.last())
        assertEquals(ChoiceOutcome.GrantForSession, accessCard.choices[1].outcome)
    }

    @Test
    fun planWhoseOnlyApprovalSwitchesModeSaysSo() {
        val actions = mutableListOf<AskAction>()
        val askCard = card(
            body = AskBody.Plan(title = "Refactor cache", body = "1. Split read and write paths."),
            choices = listOf(
                Choice(ChoiceOutcome.ApprovePlan(mode = "acceptEdits", modeName = "Accept edits"), primary = true, takesNote = false),
                Choice(ChoiceOutcome.SendBack, primary = false, takesNote = true),
            ),
        )
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = askCard, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.ASK_PLAN_SWITCH).assertDoesNotExist()
        composeRule.onNodeWithTag(AskReviewTags.ASK_PLAN_SWITCHES).assertTextEquals("Switches to Accept edits")
        composeRule.onNodeWithTag(AskReviewTags.ASK_PLAN_APPROVE).assertTextEquals("Approve").performClick()
        assertEquals(AskAction.Choose(0, null, askKey = "ask-1"), actions.last())
    }

    @Test
    fun unanswerableAndCardStatesRenderCorrectly() {
        val actions = mutableListOf<AskAction>()
        val unanswerable = card(
            body = AskBody.Unanswerable("Custom provider prompt"),
            choices = emptyList(),
        )
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = unanswerable, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithText("Custom provider prompt").assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.ASK_STOP).performClick()
        assertEquals(AskAction.Interrupt, actions.last())
    }

    @Test
    fun sendingStateShrinksToQuietStatusLine() {
        val sendingCard = card(
            body = AskBody.Command("ls", "", "", ""),
            choices = listOf(Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false)),
            state = CardState.Sending,
        )
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = sendingCard, onAction = {})
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.ASK_SENDING).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.ASK_SUBJECT).assertDoesNotExist()
    }

    @Test
    fun notConfirmedAndRejectedStatesOfferRecovery() {
        val actions = mutableListOf<AskAction>()
        val notConfirmed = card(
            body = AskBody.Command("ls", "", "", ""),
            choices = listOf(Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false)),
            state = CardState.NotConfirmed,
        )
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = notConfirmed, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.ASK_NOT_CONFIRMED).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.ASK_RESEND).performClick()
        assertEquals(AskAction.Resend, actions.last())
        composeRule.onNodeWithTag(AskReviewTags.ASK_DISCARD).performClick()
        assertEquals(AskAction.Discard, actions.last())
    }

    @Test
    fun rejectedStateDisplaysReasonAndKeepsChoicesInteractive() {
        val actions = mutableListOf<AskAction>()
        val rejected = card(
            body = AskBody.Tool("github", "create_issue", "{\"title\":\"bug\"}"),
            choices = listOf(Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false)),
            state = CardState.Rejected(Refusal(RefusalReason.HostUnreachable)),
        )
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = rejected, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.ASK_REJECTED).assertIsDisplayed()
        composeRule.onNodeWithText("Not sent · its host is away").assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.askChoice(0)).performClick()
        assertEquals(AskAction.Choose(0, null, askKey = "ask-1"), actions.last())
    }

    @Test
    fun questionNoteReplyAndSecretOtherWork() {
        val actions = mutableListOf<AskAction>()
        val askCard = card(
            body = AskBody.Question(
                listOf(
                    QuestionView(
                        header = "API Key",
                        question = "Provide a token or reply in your own words",
                        multiSelect = false,
                        options = listOf(OptionView("Use env var", "", "", false)),
                        allowOther = true,
                        secret = true,
                    ),
                ),
            ),
            choices = emptyList(),
            questionNote = true,
            questionReply = true,
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = askCard, onAction = { actions += it })
            }
        }

        // Open note field and add a note, then tap option + Send
        composeRule.onNodeWithTag(AskReviewTags.askAddNote(0)).performClick()
        composeRule.onNodeWithTag(AskReviewTags.askQuestionNote(0)).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Use staging key"))
        }
        composeRule.onNodeWithTag(AskReviewTags.askOption(0)).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_NEXT).performClick()
        assertEquals(
            AskAction.Respond(listOf(QuestionResponse(note = "Use staging key", other = null, picked = listOf(0))), askKey = "ask-1"),
            actions.last(),
        )

        // Reply in own words
        composeRule.onNodeWithTag(AskReviewTags.ASK_REPLY_LINK).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_REPLY_INPUT).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Let me rotate the key first"))
        }
        composeRule.onNodeWithTag(AskReviewTags.ASK_REPLY_SEND).performClick()
        assertTrue(actions.last() is AskAction.Reply)
        assertEquals("Let me rotate the key first", (actions.last() as AskAction.Reply).text)
        assertEquals("ask-1", (actions.last() as AskAction.Reply).askKey)
    }

    @Test
    fun dismissedStateAndLocalLinkRenderAppropriately() {
        var activeCard by mutableStateOf(
            card(
                body = AskBody.Command("ls", "", "", ""),
                choices = listOf(Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false)),
                state = CardState.Dismissed,
            ),
        )
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = activeCard, onAction = {})
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.ASK_DISMISSED).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.ASK_SUBJECT).assertDoesNotExist()

        // Local-page Link ask hides Open Link button and shows local-computer notice.
        activeCard = card(
            body = AskBody.Link(
                server = "oauth",
                message = "Finish sign-in",
                url = "http://127.0.0.1:4000/auth",
                localPage = true,
            ),
            choices = listOf(
                Choice(ChoiceOutcome.OpenLink, primary = true, takesNote = false),
                Choice(ChoiceOutcome.Decline, primary = false, takesNote = false),
            ),
        )
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(AskReviewTags.ASK_LINK_OPEN).assertDoesNotExist()
        composeRule.onNodeWithText("This page is on the agent’s computer. Open the link in a browser there.")
            .assertIsDisplayed()
    }

    @Test
    fun stopsTurnFalseHidesInterruptAndWhitespaceOtherCannotSubmit() {
        val askCard = card(
            body = AskBody.Question(
                listOf(
                    QuestionView(
                        header = "Choice",
                        question = "Pick or type",
                        multiSelect = false,
                        options = emptyList(),
                        allowOther = true,
                        secret = false,
                    ),
                ),
            ),
            choices = emptyList(),
            stopsTurn = false,
        )
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = askCard, onAction = {})
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.ASK_MORE).assertDoesNotExist()
        composeRule.onNodeWithTag(AskReviewTags.ASK_OPTION_OTHER).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_OTHER_INPUT).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("   "))
        }
        composeRule.onNodeWithTag(AskReviewTags.ASK_NEXT).assertIsNotEnabled()
    }

    @Test
    fun optionalManyFormFieldAndNegativeDecimalValidateCleanly() {
        val fields = listOf(
            FormField(
                name = "tags",
                title = "Tags",
                description = "",
                required = false,
                kind = FormFieldKind.Many(options = listOf("a", "b", "c"), minItems = 2, maxItems = 3),
                initial = FormValue.Many(emptyList()),
            ),
            FormField(
                name = "offset",
                title = "Offset",
                description = "",
                required = true,
                kind = FormFieldKind.Number(integer = false, minimum = -10.0, maximum = 10.0),
                initial = FormValue.Text("-3.5"),
            ),
        )
        val problems = validateFormFields(fields, listOf(FormValue.Many(emptyList()), FormValue.Text("-3.5")))
        assertTrue("optional empty Many and negative decimal must be valid: $problems", problems.isEmpty())

        val tooFew = validateFormFields(fields, listOf(FormValue.Many(listOf(0)), FormValue.Text("-3.5")))
        assertEquals(listOf(FieldProblem.TooFew(2)), tooFew.map { it.problem })
    }

    @Test
    fun permissionNoteSurvivesSendingToRejectedAndAccessButtonsStayReachable() {
        val baseCard = card(
            body = AskBody.Command("rm -rf build", "", "Clean build", ""),
            choices = listOf(
                Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false),
                Choice(ChoiceOutcome.Deny, primary = false, takesNote = true),
            ),
            state = CardState.Open,
        )
        var currentCard by mutableStateOf(baseCard)
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Box(modifier = Modifier.heightIn(max = 520.dp)) {
                    AskCardView(card = currentCard, onAction = {})
                }
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.ASK_DENY).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_NOTE).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Keep target cache"))
        }

        // Sending -> Rejected preserves typed note & note mode for the same ask key.
        currentCard = baseCard.copy(state = CardState.Sending)
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(AskReviewTags.ASK_SENDING).assertIsDisplayed()

        currentCard = baseCard.copy(state = CardState.Rejected(Refusal(RefusalReason.HostUnreachable)))
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(AskReviewTags.ASK_NOTE).assertIsDisplayed()
        composeRule.onNodeWithText("Keep target cache").assertIsDisplayed()

        // A tall access card keeps its choice and buttons reachable under
        // its scrolling facts.
        currentCard = card(
            body = AskBody.Access(
                reason = "Read build inputs",
                read = (1..20).map { "/var/cache/module-$it/artifacts" },
                write = (1..10).map { "/tmp/output-$it" },
                network = true,
                hosts = listOf("crates.io"),
            ),
            choices = listOf(
                Choice(ChoiceOutcome.GrantForTurn, primary = true, takesNote = false),
                Choice(ChoiceOutcome.GrantForSession, primary = false, takesNote = false),
                Choice(ChoiceOutcome.Deny, primary = false, takesNote = false),
            ),
        ).copy(key = "ask-access-tall")
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(AskReviewTags.ASK_GRANT_TURN).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.ASK_GRANT_SESSION).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.ASK_GRANT).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.askChoice(2)).assertIsDisplayed()
    }

    @Test
    fun systemBackStepsBackwardInsideMultiStepQuestionAsk() {
        val backDispatcher = OnBackPressedDispatcher()
        val backOwner = object : OnBackPressedDispatcherOwner {
            private val registry = LifecycleRegistry(this).apply {
                currentState = Lifecycle.State.RESUMED
            }
            override val lifecycle: Lifecycle = registry
            override val onBackPressedDispatcher: OnBackPressedDispatcher = backDispatcher
        }
        val askCard = card(
            body = AskBody.Question(
                listOf(
                    QuestionView("Step1", "Question one?", false, listOf(OptionView("A", "", "", false)), false, false),
                    QuestionView("Step2", "Question two?", false, listOf(OptionView("B", "", "", false)), false, false),
                ),
            ),
            choices = emptyList(),
            questionReply = true,
        )

        composeRule.setContent {
            CompositionLocalProvider(LocalOnBackPressedDispatcherOwner provides backOwner) {
                AmuxTheme(appearance = Appearance.Light, photographed = true) {
                    AskCardView(card = askCard, onAction = {})
                }
            }
        }

        // Advance step 0 -> step 1 -> review
        composeRule.onNodeWithTag(AskReviewTags.askOption(0)).performClick()
        composeRule.onNodeWithText("Question two?").assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.askOption(0)).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_SEND).assertIsDisplayed()

        // Back from review -> step 1
        composeRule.runOnIdle { backDispatcher.onBackPressed() }
        composeRule.onNodeWithText("Question two?").assertIsDisplayed()

        // Back from step 1 -> step 0
        composeRule.runOnIdle { backDispatcher.onBackPressed() }
        composeRule.onNodeWithText("Question one?").assertIsDisplayed()

        // Enter reply-instead, then Back returns to question step
        composeRule.onNodeWithTag(AskReviewTags.ASK_REPLY_LINK).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_REPLY_INPUT).assertIsDisplayed()
        composeRule.runOnIdle { backDispatcher.onBackPressed() }
        composeRule.onNodeWithText("Question one?").assertIsDisplayed()
    }

    @Test
    fun freeTextOtherRejectsBlankAndPreservesSurroundingSpacesVerbatim() {
        val actions = mutableListOf<AskAction>()
        val askCard = card(
            body = AskBody.Question(
                listOf(
                    QuestionView(
                        header = "Secret",
                        question = "Enter passphrase",
                        multiSelect = false,
                        options = emptyList(),
                        allowOther = true,
                        secret = true,
                    ),
                ),
            ),
            choices = emptyList(),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = askCard, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.ASK_OPTION_OTHER).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_OTHER_INPUT).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("   "))
        }
        composeRule.onNodeWithTag(AskReviewTags.ASK_NEXT).assertIsNotEnabled()

        composeRule.onNodeWithTag(AskReviewTags.ASK_OTHER_INPUT).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("  secret  "))
        }
        composeRule.onNodeWithTag(AskReviewTags.ASK_NEXT).assertIsEnabled().performClick()
        assertEquals(
            AskAction.Respond(
                responses = listOf(QuestionResponse(note = null, other = "  secret  ", picked = emptyList())),
                askKey = "ask-1",
            ),
            actions.last(),
        )
    }
}
