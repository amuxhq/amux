package sh.amux.features

import androidx.compose.ui.unit.dp
import androidx.compose.ui.test.getBoundsInRoot
import androidx.compose.ui.unit.height
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.semantics.SemanticsActions
import java.io.File
import sh.amux.values.CommandView
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToIndex
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
import sh.amux.values.Offline
import sh.amux.values.Overview
import sh.amux.values.PhaseView
import sh.amux.values.Reach
import sh.amux.values.RouteKind
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Segment
import sh.amux.values.SentReviewRef
import sh.amux.values.SettingsView
import sh.amux.values.TaskLine
import sh.amux.values.TaskMark
import sh.amux.values.TasksView
import sh.amux.values.Waiting
import sh.amux.testsupport.DoorReading
import sh.amux.values.ProsePart
import sh.amux.testsupport.plainMarkdown
import sh.amux.testsupport.prosePart
import sh.amux.testsupport.MarkdownFixtures
import sh.amux.testsupport.FixtureProviders

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
        provider = FixtureProviders.of(Kind.ClaudeSdk).first,
        capabilities = FixtureProviders.of(Kind.ClaudeSdk).second,
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
            kind = RowKind.Prompt(text = listOf(ProsePart.Text(plainMarkdown("Fix the retry loop."))), steered = false),
            collapsed = false,
            attention = false,
        ),
        Row(
            id = "r2",
            order = 2L,
            atMs = 2_000L,
            kind = RowKind.Prose(
                text = listOf(prosePart("Updated backoff logic in `retry.rs`.")),
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
        provider = "claude",
        providerName = "Claude",
        attachable = false,
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

        composeRule.onNodeWithTag(TestTags.SCREEN_CHAT).assertIsDisplayed()
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

    /**
     * The journeys check that a chat is current by its header, and read its
     * name after a rename by its title, as on the other clients.
     */
    @Test
    fun titleAndHeaderReadAsTheJourneysCheckThem() {
        var subject by mutableStateOf(ChatSubject(name = "helper", host = "desk", directory = "/Users/ada/source/amux"))
        var mode by mutableStateOf<Composer>(Composer.Send)
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatScreen(
                    state = ChatScreenState(
                        subject = subject,
                        keys = emptyList(),
                        frame = baseFrame(name = "helper", mode = mode),
                    ),
                    rowSource = StaticChatRowSource(emptyList()),
                    callbacks = ChatScreenCallbacks(),
                )
            }
        }
        fun read(tag: String) = composeRule.onNodeWithTag(tag).fetchSemanticsNode()
            .let { DoorReading.label(it) to DoorReading.value(it) }

        composeRule.onNodeWithTag(TestTags.SCREEN_CHAT).assertIsDisplayed()
        assertEquals("helper" to "helper", read(ChatScreenTags.TITLE))
        assertEquals("live", read(ChatScreenTags.HEADER).second)

        subject = subject.copy(reach = Reach.Offline(Offline.Plain))
        composeRule.waitForIdle()
        assertEquals("desk · away" to "away", read(ChatScreenTags.HEADER))

        subject = subject.copy(reach = Reach.Online(RouteKind.Direct))
        mode = Composer.Waiting(Waiting.Detached)
        composeRule.waitForIdle()
        assertEquals("detached", read(ChatScreenTags.HEADER).second)
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

        // Overview from overflow menu
        composeRule.onNodeWithTag(ChatScreenTags.MORE).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.MENU_OVERVIEW).performClick()
        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_SHEET).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_CLOSE).performClick()

        // Settings from overflow menu
        composeRule.onNodeWithTag(ChatScreenTags.MORE).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.MENU_SETTINGS).performClick()
        composeRule.onNodeWithTag(AskReviewTags.CHAT_SETTINGS_SHEET).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.CHAT_SETTINGS_CLOSE).performClick()

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
    fun dockHeadUnfoldsStartedAgentsAndOpensOne() {
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

        // The agents this one started sit in the dock, folded under its head.
        composeRule.onNodeWithTag(ChatScreenTags.familyChild("worker-a")).assertDoesNotExist()
        composeRule.onNodeWithTag(ChatComposerTags.TASKS).assertIsDisplayed().performClick()
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
            replyInstead = false,
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
        assertEquals(listOf(AskAction.Choose(0, null, askKey = "ask-1")), askActions)
    }

    private fun accessAsk(paths: Int): AskCard = AskCard(
        kind = Kind.ClaudeSdk,
        key = "ask-access",
        itemKey = "item-access",
        position = 1L,
        count = 1L,
        body = AskBody.Access(
            reason = "Read build inputs",
            read = (1..paths).map { "/var/cache/module-$it/artifacts" },
            write = emptyList(),
            network = false,
            hosts = emptyList(),
        ),
        choices = listOf(
            Choice(ChoiceOutcome.GrantForTurn, primary = true, takesNote = false),
            Choice(ChoiceOutcome.Deny, primary = false, takesNote = false),
        ),
        questionNote = false,
        questionSkip = false,
        questionReply = false,
        replyInstead = false,
        stopsTurn = true,
        defaultToNo = false,
        state = CardState.Open,
    )

    @Test
    fun tallAskLeavesAGlimpseOfTheTranscriptUnderBannersAndANotice() {
        val rows = sampleRows()
        val state = ChatScreenState(
            subject = ChatSubject(name = "refactor-auth", host = "desk", closeHeld = true),
            keys = rows.map { it.id },
            frame = baseFrame(phase = PhaseView.NeedsYou),
            ask = accessAsk(paths = 60),
            notice = "File exceeds the 64 MB limit.",
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatScreen(state = state, rowSource = StaticChatRowSource(rows), callbacks = ChatScreenCallbacks())
            }
        }

        val feed = composeRule.onNodeWithTag(ChatScreenTags.FEED).getBoundsInRoot()
        assertTrue("feed is ${feed.height}", feed.height >= 48.dp)
        composeRule.onNodeWithTag(AskReviewTags.ASK_GRANT).assertIsDisplayed()
    }

    @Test
    fun dockStaysUnfoldedWhileAnAskComesAndGoes() {
        val ask = accessAsk(paths = 1)
        var state by mutableStateOf(
            ChatScreenState(
                subject = ChatSubject(name = "refactor-auth", host = "desk", key = agentKey),
                frame = baseFrame(),
                overview = Overview(
                    jobs = emptyList(),
                    failedServers = emptyList(),
                    changes = null,
                    tasks = TasksView(
                        done = 1,
                        total = 2,
                        current = "Split the lexer",
                        entries = listOf(
                            TaskLine("Read the parser", TaskMark.Done),
                            TaskLine("Split the lexer", TaskMark.Current),
                        ),
                    ),
                    usageNearLimit = null,
                ),
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatScreen(state = state, rowSource = StaticChatRowSource(emptyList()), callbacks = ChatScreenCallbacks())
            }
        }

        composeRule.onNodeWithTag(ChatComposerTags.TASKS).performClick()
        composeRule.onNodeWithTag(ChatComposerTags.task(1)).assertIsDisplayed()

        state = state.copy(ask = ask)
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(ChatComposerTags.DOCK).assertDoesNotExist()

        state = state.copy(ask = null)
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(ChatComposerTags.task(1)).assertIsDisplayed()
    }

    @Test
    fun openedAttachmentRendersFullScreenPreviewDialogWithOpenAndShare() {
        var cleared = false
        val openedExternal = mutableListOf<File>()
        val shared = mutableListOf<File>()
        val localFile = File("/tmp/trace.txt")
        val state = ChatScreenState(
            subject = ChatSubject(name = "refactor-auth", host = "desk"),
            frame = baseFrame(),
            openedAttachment = ChatOpenedAttachmentUi(
                name = "trace.txt",
                bytes = null,
                isImage = false,
                file = localFile,
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatScreen(
                    state = state,
                    rowSource = StaticChatRowSource(emptyList()),
                    callbacks = ChatScreenCallbacks(
                        onOpenAttachmentExternally = { openedExternal += it },
                        onShareAttachment = { shared += it },
                        onClearOpenedAttachment = { cleared = true },
                    ),
                )
            }
        }

        composeRule.onNodeWithTag(ChatScreenTags.PREVIEW_DIALOG).assertIsDisplayed()
        composeRule.onNodeWithTag(ChatScreenTags.PREVIEW_OPEN).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.PREVIEW_SHARE).performClick()
        assertEquals(listOf(localFile), openedExternal)
        assertEquals(listOf(localFile), shared)
        composeRule.onNodeWithTag(ChatScreenTags.PREVIEW_CLOSE).performClick()
        assertTrue(cleared)
    }

    @Test
    fun scrollableTranscriptDoesNotEmitFrameZeroReadingOrPagingAndFollowsCappedTail() {
        val readingEvents = mutableListOf<Boolean>()
        val reachedTopEvents = mutableListOf<Int>()

        fun makeRows(range: IntRange): List<Row> = range.map { i ->
            Row(
                id = "m$i",
                order = i.toLong(),
                atMs = i * 1_000L,
                kind = RowKind.Prose(
                    text = listOf(ProsePart.Text(plainMarkdown("Transcript message number $i with enough text to occupy vertical space."))),
                    streaming = false,
                    workingNote = false,
                ),
                collapsed = false,
                attention = false,
            )
        }

        var rows by mutableStateOf(makeRows(1..50))
        var screenState by mutableStateOf(
            ChatScreenState(
                subject = ChatSubject(name = "long-chat", host = "desk", key = agentKey),
                keys = rows.map { it.id },
                frame = baseFrame(name = "long-chat").copy(hasOlder = true),
                following = true,
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatScreen(
                    state = screenState,
                    rowSource = StaticChatRowSource(rows),
                    callbacks = ChatScreenCallbacks(
                        onReadingChange = { readingEvents += it },
                        onReachedTop = { reachedTopEvents += it },
                    ),
                )
            }
        }
        composeRule.waitForIdle()

        assertTrue("frame-0 must never report reading=false: $readingEvents", readingEvents.isEmpty())
        assertTrue("frame-0 while following must never trigger reachedTop: $reachedTopEvents", reachedTopEvents.isEmpty())
        composeRule.onNodeWithText("Transcript message number 50 with enough text to occupy vertical space.")
            .assertIsDisplayed()

        // Cap-scrolling: drop oldest (m1) and append newest (m51) -> same key count (50), new tail key.
        rows = makeRows(2..51)
        screenState = screenState.copy(keys = rows.map { it.id })
        composeRule.waitForIdle()

        assertTrue("cap-scroll must not spuriously unfollow: $readingEvents", readingEvents.isEmpty())
        composeRule.onNodeWithText("Transcript message number 51 with enough text to occupy vertical space.")
            .assertIsDisplayed()

        // Non-drag scroll (e.g. accessibility scroll / mouse wheel) updates reading=false and enables paging.
        composeRule.onNodeWithTag(ChatScreenTags.FEED).performScrollToIndex(0)
        composeRule.waitForIdle()
        assertEquals(listOf(false), readingEvents)
        assertTrue("scrolling to top via non-drag input must trigger paging", reachedTopEvents.isNotEmpty())
    }

    @Test
    fun failedPagingOffersRetryAndSettingsCommandPreservesNonBlankDraft() {
        val retried = mutableListOf<Int>()
        val draftChanges = mutableListOf<String>()
        val settingsWithCmd = sampleSettings().copy(
            commands = listOf(
                CommandView("compact", "Compact history", "", "built-in", "/", false),
            ),
        )
        val state = ChatScreenState(
            subject = ChatSubject(name = "refactor-auth", host = "desk", key = agentKey),
            keys = emptyList(),
            frame = baseFrame().copy(hasOlder = true),
            paging = ChatPagingUi.Failed("timeout"),
            settings = settingsWithCmd,
            draft = "important unsent prompt",
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatScreen(
                    state = state,
                    rowSource = StaticChatRowSource(emptyList()),
                    callbacks = ChatScreenCallbacks(
                        onReachedTop = { retried += it },
                        onDraftChange = { text, _ -> draftChanges += text },
                    ),
                )
            }
        }

        // Failed paging notice offers Retry button
        composeRule.onNodeWithTag(ChatScreenTags.PAGING_RETRY).assertIsDisplayed().performClick()
        assertEquals(listOf(0), retried)

        // Selecting a command from Settings while draft is non-blank asks confirmation first
        composeRule.onNodeWithTag(ChatScreenTags.MORE).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.MENU_SETTINGS).performClick()
        composeRule.onNodeWithTag(AskReviewTags.chatSettingsCommand("compact")).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.COMMAND_REPLACE_DIALOG).assertIsDisplayed()
        assertTrue("draft must not be overwritten before confirmation", draftChanges.isEmpty())

        composeRule.onNodeWithTag(ChatScreenTags.COMMAND_REPLACE_KEEP).performClick()
        assertTrue(draftChanges.isEmpty())

        // Confirming replaces the draft with the command prefix
        composeRule.onNodeWithTag(ChatScreenTags.MORE).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.MENU_SETTINGS).performClick()
        composeRule.onNodeWithTag(AskReviewTags.chatSettingsCommand("compact")).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.COMMAND_REPLACE_CONFIRM).performClick()
        assertEquals(listOf("/compact "), draftChanges)
    }

    @Test
    fun settingsCommandPreservesExistingCommandArgumentsAndConfirmsDifferentPrefix() {
        val draftChanges = mutableListOf<String>()
        val settingsWithCmd = sampleSettings().copy(
            commands = listOf(
                CommandView("compact", "Compact history", "", "built-in", "/", false),
            ),
        )
        var screenState by mutableStateOf(
            ChatScreenState(
                subject = ChatSubject(name = "refactor-auth", host = "desk", key = agentKey),
                keys = emptyList(),
                frame = baseFrame(),
                settings = settingsWithCmd,
                draft = "/compact keep context",
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatScreen(
                    state = screenState,
                    rowSource = StaticChatRowSource(emptyList()),
                    callbacks = ChatScreenCallbacks(
                        onDraftChange = { text, _ -> draftChanges += text },
                    ),
                )
            }
        }

        // Same command with arguments stays untouched without opening a confirmation dialog.
        composeRule.onNodeWithTag(ChatScreenTags.MORE).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.MENU_SETTINGS).performClick()
        composeRule.onNodeWithTag(AskReviewTags.chatSettingsCommand("compact")).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.COMMAND_REPLACE_DIALOG).assertDoesNotExist()
        assertTrue("existing arguments for the same command must not be overwritten", draftChanges.isEmpty())

        // Different command sharing a prefix prompts confirmation before overwriting.
        screenState = screenState.copy(draft = "/compact-other")
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(ChatScreenTags.MORE).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.MENU_SETTINGS).performClick()
        composeRule.onNodeWithTag(AskReviewTags.chatSettingsCommand("compact")).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.COMMAND_REPLACE_DIALOG).assertIsDisplayed()
        assertTrue(draftChanges.isEmpty())
    }

    /**
     * A round raised from its chip is the screen's once raised: the model
     * lets it go, the sheet stays, and a chat left and come back to (a tab,
     * a link) does not raise it again.
     */
    @Test
    fun aRaisedRoundGoesWithTheScreen() {
        val sent = SentReviewRef(reviewId = listOf(1), round = 1, inputId = listOf(7))
        var state by mutableStateOf(
            ChatScreenState(
                subject = ChatSubject(name = "refactor-auth", host = "desk", directory = "/Users/ada/source/amux"),
                frame = baseFrame(),
                round = OpenedRound(sent = sent, text = "Name it."),
            ),
        )
        var raised = 0
        var shown by mutableStateOf(true)
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                if (shown) {
                    ChatScreen(
                        state = state,
                        rowSource = StaticChatRowSource(emptyList()),
                        callbacks = ChatScreenCallbacks(
                            onRoundRaised = {
                                raised += 1
                                state = state.copy(round = null)
                            },
                        ),
                    )
                }
            }
        }
        composeRule.waitForIdle()
        assertEquals(1, raised)
        composeRule.onNodeWithTag(ReviewTags.ROUND).assertIsDisplayed()

        shown = false
        composeRule.waitForIdle()
        shown = true
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(ReviewTags.ROUND).assertDoesNotExist()
        assertEquals("nothing raised it again", 1, raised)
    }
}
