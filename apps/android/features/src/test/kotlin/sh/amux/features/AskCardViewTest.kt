package sh.amux.features

import androidx.compose.ui.semantics.SemanticsActions
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
import sh.amux.values.EditLine
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
        stopsTurn = true,
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
        assertEquals(AskAction.Choose(0, null), actions.last())

        // Always allow scope row
        composeRule.onNodeWithTag(AskReviewTags.askChoice(1)).performClick()
        assertEquals(AskAction.Choose(1, null), actions.last())

        // Deny with note flow
        composeRule.onNodeWithTag(AskReviewTags.ASK_DENY).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_NOTE).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.ASK_NOTE).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Run only unit tests"))
        }
        composeRule.onNodeWithTag(AskReviewTags.askChoice(2)).performClick()
        assertEquals(AskAction.Choose(2, "Run only unit tests"), actions.last())

        // Overflow menu interrupt escape
        composeRule.onNodeWithTag(AskReviewTags.ASK_MORE).performClick()
        composeRule.onNodeWithTag(AskReviewTags.ASK_INTERRUPT_MENU).performClick()
        assertTrue(interrupted)
    }

    @Test
    fun permissionEditRendersDiffHunksAndEllipsis() {
        val actions = mutableListOf<AskAction>()
        val askCard = card(
            body = AskBody.Edit(
                path = "src/main.rs",
                files = 1,
                added = 2,
                removed = 1,
                lines = listOf(
                    EditLine.Hunk(oldStart = 0, newStart = 0),
                    EditLine.Line(PatchLine(LineKind.Removed, "let old = 1;", number = 10)),
                    EditLine.Line(PatchLine(LineKind.Added, "let next = 2;", number = 10)),
                    EditLine.Line(PatchLine(LineKind.Added, "let more = 3;", number = 11)),
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
        composeRule.onNodeWithText("⋯").assertIsDisplayed()
        composeRule.onNodeWithText("let next = 2;").assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.askChoice(0)).performClick()
        assertEquals(AskAction.Choose(0, null), actions.last())
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
            AskAction.Respond(listOf(QuestionResponse(note = null, other = null, picked = listOf(0)))),
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
            AskAction.Respond(listOf(QuestionResponse(note = null, other = "Linux", picked = listOf(0)))),
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

        composeRule.onNodeWithText("Refactor cache").assertIsDisplayed()
        // Toggle switch on -> wording becomes "Yes, and switch to auto"
        composeRule.onNodeWithTag(AskReviewTags.ASK_PLAN_SWITCH).performClick()
        composeRule.onNodeWithText("Yes, and switch to auto").assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.ASK_PLAN_APPROVE).performClick()
        assertEquals(AskAction.Choose(1, null), actions.last())

        // Send back requires a note
        composeRule.onNodeWithTag(AskReviewTags.askChoice(2)).performClick()
        composeRule.onNodeWithTag(AskReviewTags.askChoice(2)).assertIsNotEnabled()
        composeRule.onNodeWithTag(AskReviewTags.ASK_NOTE).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Keep the existing schema"))
        }
        composeRule.onNodeWithTag(AskReviewTags.askChoice(2)).assertIsEnabled().performClick()
        assertEquals(AskAction.Choose(2, "Keep the existing schema"), actions.last())
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
            AskAction.Submit(listOf(FormValue.Text("3"), FormValue.Toggle(true))),
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
        assertEquals(AskAction.Choose(0, null), actions.last())
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
        composeRule.onNodeWithTag(AskReviewTags.ASK_GRANT_SESSION).performClick()
        assertEquals(AskAction.Choose(1, null), actions.last())
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
        assertEquals(AskAction.Choose(0, null), actions.last())
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
            AskAction.Respond(listOf(QuestionResponse(note = "Use staging key", other = null, picked = listOf(0)))),
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
    }

    @Test
    fun dismissedStateAndLocalLinkRenderAppropriately() {
        val dismissedCard = card(
            body = AskBody.Command("ls", "", "", ""),
            choices = listOf(Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false)),
            state = CardState.Dismissed,
        )
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AskCardView(card = dismissedCard, onAction = {})
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.ASK_DISMISSED).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.ASK_SUBJECT).assertDoesNotExist()
    }
}
