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
import sh.amux.values.AgentKey
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.Attention
import sh.amux.values.CardState
import sh.amux.values.ChangeTotals
import sh.amux.values.Changeability
import sh.amux.values.Changeable
import sh.amux.values.ChatFrame
import sh.amux.values.Choice
import sh.amux.values.ChoiceOutcome
import sh.amux.values.Composer
import sh.amux.values.ComposerView
import sh.amux.values.Connection
import sh.amux.values.ControlsSummary
import sh.amux.values.FamilyHeader
import sh.amux.values.Filing
import sh.amux.values.FleetCard
import sh.amux.values.Kind
import sh.amux.values.ModelChoice
import sh.amux.values.Overview
import sh.amux.values.PhaseView
import sh.amux.values.Reach
import sh.amux.values.RouteKind
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Segment
import sh.amux.values.SettingsView
import sh.amux.values.TaskLine
import sh.amux.values.TaskMark
import sh.amux.values.TasksView

@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class ChatScreenTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val agentKey = AgentKey(host = List(16) { 1 }, agent = List(16) { 2 })
    private val parentKey = AgentKey(host = List(16) { 1 }, agent = List(16) { 1 })
    private val childKey = AgentKey(host = List(16) { 1 }, agent = List(16) { 3 })

    private fun baseFrame(
        name: String = "refactor-auth",
        phase: PhaseView = PhaseView.Idle,
        mode: Composer = Composer.Send,
        arrivalsHeld: Boolean = false,
        ended: String? = null,
    ): ChatFrame = ChatFrame(
        agent = agentKey,
        name = name,
        kind = Kind.ClaudeSdk,
        phase = phase,
        composer = ComposerView(mode = mode, switchedAway = false, activity = null),
        controls = ControlsSummary(effort = null, mode = null, model = "Opus 4.6", permission = null),
        connection = Connection.Live,
        caughtUp = true,
        hasOlder = false,
        arrivalsHeld = arrivalsHeld,
        queue = emptyList(),
        underway = emptyList(),
        refused = emptyList(),
        askInput = null,
        context = null,
        ended = ended,
        git = null,
        signIn = null,
    )

    private fun sampleRows(): List<Row> = listOf(
        Row(
            id = "r1",
            order = 1L,
            atMs = 1_000L,
            kind = RowKind.Prompt(text = listOf(Segment.Text("Fix the retry loop.")), steered = false),
            collapsed = false,
            attention = false,
        ),
        Row(
            id = "r2",
            order = 2L,
            atMs = 2_000L,
            kind = RowKind.Prose(
                text = listOf(Segment.Text("Updated backoff logic in `retry.rs`.")),
                streaming = false,
                workingNote = false,
            ),
            collapsed = false,
            attention = false,
        ),
    )

    private fun sampleSettings(): SettingsView = SettingsView(
        models = listOf(
            ModelChoice("opus", "Opus 4.6", "Deep reasoning", listOf("high"), current = true, unlisted = false),
        ),
        efforts = emptyList(),
        permissions = emptyList(),
        modes = emptyList(),
        changeable = Changeability(
            model = Changeable.Pick,
            effort = Changeable.NotOffered,
            permission = Changeable.NotOffered,
            mode = Changeable.NotOffered,
        ),
        commands = emptyList(),
    )

    private fun sampleFleetCard(agent: AgentKey, name: String, attention: Attention): FleetCard = FleetCard(
        agent = agent,
        name = name,
        handle = name,
        kind = Kind.ClaudeSdk,
        attention = attention,
        cwd = "/Users/ada/source/amux",
        phaseSinceMs = 1_000L,
        host = "desk",
        hostReach = Reach.Online(RouteKind.Direct),
        children = 0,
        familyAttention = attention,
        members = 1,
        descendants = 0,
        backgroundJobs = 0,
        closeHeld = false,
        filing = Filing.Open,
        topLevel = false,
        native = false,
    )

    @Test
    fun rendersHeaderTranscriptChangesChipNewActivityPillAndBanners() {
        var jumpedToNewest = false
        var openedReview = false
        var cancelledStop = false
        val rows = sampleRows()
        val state = ChatScreenState(
            subject = ChatSubject(
                name = "refactor-auth",
                host = "desk",
                directory = "/Users/ada/source/amux",
                closeHeld = true,
            ),
            keys = rows.map { it.id },
            frame = baseFrame(arrivalsHeld = true, ended = "This agent was deleted on desk."),
            following = false,
            changes = ChangeTotals(files = 2, added = 18, removed = 4),
            paging = ChatPagingUi.Fetching,
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatScreen(
                    state = state,
                    rowSource = StaticChatRowSource(rows),
                    callbacks = ChatScreenCallbacks(
                        onJumpToNewest = { jumpedToNewest = true },
                        onOpenReview = { openedReview = true },
                        onCancelSettle = { cancelledStop = true },
                    ),
                )
            }
        }

        composeRule.onNodeWithTag(TestTags.SCREEN_CONVERSATION).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatScreenTags.TITLE).assertIsDisplayed()
        composeRule.onNodeWithText("refactor-auth").assertIsDisplayed()
        composeRule.onNodeWithTag(ChatScreenTags.HELD_STOP).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatScreenTags.ENDED_BANNER).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatScreenTags.PAGING).assertIsDisplayed()
        composeRule.onNodeWithText("Loading earlier…").assertIsDisplayed()
        composeRule.onNodeWithText("Fix the retry loop.").assertIsDisplayed()

        // New activity pill jumps to newest
        composeRule.onNodeWithTag(ChatScreenTags.NEW_ACTIVITY).assertIsDisplayed().performClick()
        assertTrue(jumpedToNewest)

        // Held stop banner cancel button
        composeRule.onNodeWithTag(ChatScreenTags.HELD_STOP_CANCEL).performClick()
        assertTrue(cancelledStop)

        // Changes chip opens review
        composeRule.onNodeWithTag(ChatScreenTags.CHANGES).performClick()
        assertTrue(openedReview)
    }

    @Test
    fun overflowMenuSupportsOverviewSettingsRenameSettleAndOmitsNativeRestrictedItems() {
        var copiedAddress: String? = null
        var renamedTo: String? = null
        var settled = false
        val rows = sampleRows()
        val state = ChatScreenState(
            subject = ChatSubject(
                name = "refactor-auth",
                host = "studio",
                directory = "/Users/ada/source/amux",
                filing = Filing.Open,
                topLevel = true,
                native = false,
            ),
            keys = rows.map { it.id },
            frame = baseFrame(phase = PhaseView.Working),
            overview = Overview(
                jobs = emptyList(),
                failedServers = emptyList(),
                changes = null,
                tasks = TasksView(1, 2, "Verify", listOf(TaskLine("Verify", TaskMark.Current))),
                usageNearLimit = null,
            ),
            settings = sampleSettings(),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatScreen(
                    state = state,
                    rowSource = StaticChatRowSource(rows),
                    callbacks = ChatScreenCallbacks(
                        onCopyAddress = { copiedAddress = it },
                        onRename = { renamedTo = it },
                        onSettle = { settled = true },
                    ),
                )
            }
        }

        // Copy Address from overflow menu
        composeRule.onNodeWithTag(ChatScreenTags.MORE).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.MENU_COPY_ADDRESS).performClick()
        assertEquals("refactor-auth/studio", copiedAddress)

        // Rename dialog validates invalid names and confirms valid name
        composeRule.onNodeWithTag(ChatScreenTags.MORE).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.MENU_RENAME).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.RENAME_DIALOG).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatScreenTags.RENAME_INPUT).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Bad Name!"))
        }
        composeRule.onNodeWithTag(ChatScreenTags.RENAME_PROBLEM).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatScreenTags.RENAME_CONFIRM).assertIsNotEnabled()
        composeRule.onNodeWithTag(ChatScreenTags.RENAME_INPUT).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("auth-v2"))
        }
        composeRule.onNodeWithTag(ChatScreenTags.RENAME_CONFIRM).assertIsEnabled().performClick()
        assertEquals("auth-v2", renamedTo)

        // Settling a working agent asks confirmation first
        composeRule.onNodeWithTag(ChatScreenTags.MORE).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.MENU_SETTLE).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.SETTLE_DIALOG).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatScreenTags.SETTLE_CONFIRM).performClick()
        assertTrue(settled)
    }

    @Test
    fun nativeSubagentMenuOmitsCopyAddressAndDelete() {
        val state = ChatScreenState(
            subject = ChatSubject(
                name = "sub-1",
                host = "desk",
                filing = Filing.Closed,
                topLevel = false,
                native = true,
            ),
            family = FamilyHeader(
                children = emptyList(),
                attention = Attention.NeedsYou,
                needYou = 1,
                parent = sampleFleetCard(parentKey, "parent-agent", Attention.NeedsYou),
            ),
            frame = baseFrame(name = "sub-1"),
        )

        val opened = mutableListOf<AgentKey>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatScreen(
                    state = state,
                    rowSource = StaticChatRowSource(emptyList()),
                    callbacks = ChatScreenCallbacks(onOpenAgent = { opened += it }),
                )
            }
        }

        composeRule.onNodeWithTag(ChatScreenTags.FAMILY_PARENT).assertIsDisplayed().performClick()
        assertEquals(listOf(parentKey), opened)

        composeRule.onNodeWithTag(ChatScreenTags.MORE).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.MENU_UNSETTLE).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatScreenTags.MENU_COPY_ADDRESS).assertDoesNotExist()
        composeRule.onNodeWithTag(ChatScreenTags.MENU_DELETE).assertDoesNotExist()
    }

    @Test
    fun familyChildrenHeaderExpandsAndOpensChildAgent() {
        val opened = mutableListOf<AgentKey>()
        val state = ChatScreenState(
            subject = ChatSubject(name = "head-agent", host = "desk"),
            family = FamilyHeader(
                children = listOf(sampleFleetCard(childKey, "worker-a", Attention.NeedsYou)),
                attention = Attention.NeedsYou,
                needYou = 1,
                parent = null,
            ),
            frame = baseFrame(name = "head-agent"),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatScreen(
                    state = state,
                    rowSource = StaticChatRowSource(emptyList()),
                    callbacks = ChatScreenCallbacks(onOpenAgent = { opened += it }),
                )
            }
        }

        composeRule.onNodeWithTag(ChatScreenTags.FAMILY_CHILDREN).assertIsDisplayed().performClick()
        composeRule.onNodeWithTag(ChatScreenTags.familyChild("worker-a")).assertIsDisplayed().performClick()
        assertEquals(listOf(childKey), opened)
    }

    @Test
    fun askCardTakesOverComposerAreaAndEmitsAnswer() {
        val askActions = mutableListOf<AskAction>()
        val rows = sampleRows()
        val ask = AskCard(
            kind = Kind.ClaudeSdk,
            key = "ask-1",
            itemKey = "item-1",
            position = 1L,
            count = 1L,
            body = AskBody.Command("cargo test", "", "Verify changes", ""),
            choices = listOf(
                Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false),
                Choice(ChoiceOutcome.Deny, primary = false, takesNote = false),
            ),
            questionNote = false,
            questionSkip = false,
            questionReply = false,
            stopsTurn = true,
            defaultToNo = false,
            state = CardState.Open,
        )
        val state = ChatScreenState(
            subject = ChatSubject(name = "refactor-auth", host = "desk"),
            keys = rows.map { it.id },
            frame = baseFrame(phase = PhaseView.NeedsYou),
            ask = ask,
            draft = "kept draft text",
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatScreen(
                    state = state,
                    rowSource = StaticChatRowSource(rows),
                    callbacks = ChatScreenCallbacks(onAskAction = { askActions += it }),
                )
            }
        }

        // Transcript stays visible above ask card, composer is hidden
        composeRule.onNodeWithText("Fix the retry loop.").assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.ASK_CARD).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatComposerTags.ROOT).assertDoesNotExist()

        composeRule.onNodeWithTag(AskReviewTags.askChoice(0)).performClick()
        assertEquals(listOf(AskAction.Choose(0, null)), askActions)
    }

    @Test
    fun openedAttachmentRendersFullScreenPreviewDialog() {
        var cleared = false
        val state = ChatScreenState(
            subject = ChatSubject(name = "refactor-auth", host = "desk"),
            frame = baseFrame(),
            openedAttachment = ChatOpenedAttachmentUi(name = "trace.txt", bytes = null, isImage = false),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatScreen(
                    state = state,
                    rowSource = StaticChatRowSource(emptyList()),
                    callbacks = ChatScreenCallbacks(onClearOpenedAttachment = { cleared = true }),
                )
            }
        }

        composeRule.onNodeWithTag(ChatScreenTags.PREVIEW_DIALOG).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatScreenTags.PREVIEW_CLOSE).performClick()
        assertTrue(cleared)
    }
}
