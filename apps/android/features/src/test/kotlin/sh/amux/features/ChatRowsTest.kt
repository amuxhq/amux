package sh.amux.features

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.hasText
import androidx.compose.foundation.layout.Column
import androidx.compose.ui.test.assertHasNoClickAction
import androidx.compose.ui.test.assertIsDisplayed
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
import sh.amux.values.AgentKey
import sh.amux.values.AnswerView
import sh.amux.values.AskRow
import sh.amux.values.AttachmentView
import sh.amux.values.BlobRef
import sh.amux.values.BoundaryKind
import sh.amux.values.CallPhase
import sh.amux.values.ChildAction
import sh.amux.values.Decision
import sh.amux.values.DecisionView
import sh.amux.values.EnvelopeKind
import sh.amux.values.ExploreVerb
import sh.amux.values.FileChangeView
import sh.amux.values.FileRow
import sh.amux.values.Granted
import sh.amux.values.GroupState
import sh.amux.values.Kind
import sh.amux.values.OpenedAgent
import sh.amux.values.PermissionGrant
import sh.amux.values.PhaseView
import sh.amux.values.PlanVerdict
import sh.amux.values.QuestionView
import sh.amux.values.Resolution
import sh.amux.values.ReviewStatus
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Run
import sh.amux.values.RunCounts
import sh.amux.values.Segment
import sh.amux.values.SendState
import sh.amux.values.SubagentGroup
import sh.amux.words.RunPlacement
import sh.amux.values.ProsePart
import sh.amux.testsupport.plainMarkdown
import sh.amux.testsupport.OptionView
import sh.amux.testsupport.QuestionView
import sh.amux.testsupport.AnswerView
import sh.amux.testsupport.prosePart
import sh.amux.testsupport.MarkdownFixtures

@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class ChatRowsTest {
    @get:Rule
    val composeRule = createComposeRule()

    private fun makeRow(
        id: String,
        kind: RowKind,
        collapsed: Boolean = false,
        attention: Boolean = false,
        decision: Decision? = null,
        group: SubagentGroup? = null,
        opens: List<Int>? = null,
        run: Run? = null,
    ): Row = Row(
        id = id,
        order = 1L,
        atMs = 1_750_000_000_000L,
        kind = kind,
        collapsed = collapsed,
        attention = attention,
        decision = decision,
        group = group,
        opens = opens,
        run = run,
    )

    @Test
    fun rendersPromptWithAttachmentAndSteeredTag() {
        val fileBlob = BlobRef(hash = listOf(1, 2, 3), name = "spec.md", mime = "text/markdown", size = 4_096L)
        val openedAttachments = mutableListOf<AttachmentView>()
        val promptRow = makeRow(
            id = "p1",
            kind = RowKind.Prompt(
                text = listOf(
                    ProsePart.Text(plainMarkdown("Update the parser")),
                    ProsePart.Attachment(AttachmentView.File(fileBlob)),
                ),
                steered = true,
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatRowView(
                    row = promptRow,
                    callbacks = ChatRowCallbacks(onOpenAttachment = { openedAttachments += it }),
                )
            }
        }

        composeRule.onNodeWithText("Update the parser").assertIsDisplayed()
        composeRule.onNodeWithText("steered").assertIsDisplayed()
        composeRule.onNodeWithTag(ChatRowTags.ATTACHMENT_CHIP).performClick()
        assertEquals(listOf(AttachmentView.File(fileBlob)), openedAttachments)
    }

    @Test
    fun rendersProseAndThinkingRows() {
        val toggled = mutableListOf<String>()
        val prose = makeRow(
            id = "prose1",
            kind = RowKind.Prose(
                text = listOf(ProsePart.Text(plainMarkdown("Everything compiles."))),
                streaming = true,
                workingNote = true,
            ),
        )
        val thinking = makeRow(
            id = "think1",
            kind = RowKind.Thinking(text = "Checking edge cases.", open = false, durationMs = 8_000L),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Column {
                    ChatRowView(row = prose)
                    ChatRowView(
                        row = thinking,
                        expanded = true,
                        callbacks = ChatRowCallbacks(onToggleExpand = { toggled += it }),
                    )
                }
            }
        }

        composeRule.onNodeWithText("Everything compiles.").assertIsDisplayed()
        composeRule.onNodeWithText("Thought for 8s").assertIsDisplayed()
        composeRule.onNodeWithText("Checking edge cases.").assertIsDisplayed()
        composeRule.onNodeWithTag(ChatRowTags.kind("thinking")).performClick()
        assertEquals(listOf("think1"), toggled)
    }

    @Test
    fun rendersFoldedAndExpandedExploreRunAndUnresolvedFailure() {
        val toggledRuns = mutableListOf<String>()
        val counts = RunCounts(
            commands = 1,
            edits = 0,
            reads = 3,
            searches = 0,
            lists = 0,
            fetches = 0,
            subagents = 0,
            other = 0,
        )
        val foldedRun = Run(
            first = "e1",
            last = "e4",
            steps = 4,
            live = false,
            openBelow = false,
            liveShown = false,
            unresolvedFailure = true,
            open = false,
            counts = counts,
            recent = 0,
        )
        val exploreFailure = makeRow(
            id = "e4",
            kind = RowKind.Explore(
                verb = ExploreVerb.Read,
                subject = "src/main.rs",
                state = CallPhase.Failed,
                result = "permission denied",
            ),
            run = foldedRun,
        )
        val placement = RunPlacement.of(exploreFailure, open = false) { counts }

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatRowView(
                    row = exploreFailure,
                    placement = placement,
                    callbacks = ChatRowCallbacks(onToggleRun = { toggledRuns += it }),
                )
            }
        }

        composeRule.onNodeWithText("Read 3 files, ran 1 command").assertIsDisplayed()
        composeRule.onNodeWithText("src/main.rs").assertIsDisplayed()
        composeRule.onNodeWithTag(ChatRowTags.RUN_HEADER).performClick()
        assertEquals(listOf("e4"), toggledRuns)
    }

    @Test
    fun rendersCommandWithOutputTrimmedMoreLinesAndDecision() {
        val commandRow = makeRow(
            id = "cmd1",
            kind = RowKind.Command(
                command = "cargo test -p store",
                description = "Run store unit tests",
                state = CallPhase.Failed,
                outputHead = listOf("running 3 tests", "test fail ... FAILED"),
                moreLines = 12L,
                outputTrimmed = true,
                outputTail = listOf("summary: 1 failed"),
                durationMs = 4_200L,
                exitCode = 101,
            ),
            decision = Decision(
                outcome = DecisionView.Allowed,
                elsewhere = false,
                granted = PermissionGrant.Session,
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatRowView(row = commandRow, expanded = false)
            }
        }

        composeRule.onNodeWithText("Run store unit tests").assertIsDisplayed()
        composeRule.onNodeWithText("$ cargo test -p store").assertIsDisplayed()
        composeRule.onNodeWithTag(ChatRowTags.COMMAND_OUTPUT_TRIMMED, useUnmergedTree = true).assertIsDisplayed()
        composeRule.onNodeWithText("running 3 tests").assertIsDisplayed()
        composeRule.onNodeWithTag(ChatRowTags.COMMAND_MORE_LINES, useUnmergedTree = true).assertIsDisplayed()
        composeRule.onNodeWithText("··· earlier output trimmed").assertIsDisplayed()
        composeRule.onNodeWithText("··· 12 more lines").assertIsDisplayed()
    }

    @Test
    fun rendersFileChangeRowsListedOnly() {
        val edited = FileRow(path = "src/lib.rs", change = FileChangeView.Edited, added = 14, removed = 3)
        val created = FileRow(path = "src/new.rs", change = FileChangeView.Created(lines = 42), added = 42, removed = 0)
        val row = makeRow(
            id = "fc1",
            kind = RowKind.FileChange(files = listOf(edited, created), state = CallPhase.Succeeded),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatRowView(
                    row = row,
                )
            }
        }

        composeRule.onNodeWithText("src/lib.rs").assertIsDisplayed()
        composeRule.onNodeWithText("src/new.rs").assertIsDisplayed()
        composeRule.onNodeWithText("Created").assertIsDisplayed()
        // A landed edit's files open nothing: the review opens from the chat's header.
        composeRule.onAllNodesWithTag(ChatRowTags.FILE_CHANGE_ITEM)[0].assertHasNoClickAction()
    }

    @Test
    fun subagentKeepsOpenAgentSeparateFromExpandAnswerAndHandlesFoldedGroup() {
        val openedAgents = mutableListOf<AgentKey>()
        val expandedRows = mutableListOf<String>()
        val toggledGroups = mutableListOf<String>()

        val agentBytes = listOf(9, 8, 7)
        val agentKey = AgentKey(host = listOf(1, 2), agent = agentBytes)
        val opened = OpenedAgent(
            agent = agentKey,
            name = "audit-pass",
            kind = Kind.ClaudeSdk,
            phase = PhaseView.NeedsYou,
        )

        val subagentRow = makeRow(
            id = "sub1",
            kind = RowKind.Subagent(
                description = "Audit error handling",
                continued = false,
                state = CallPhase.Succeeded,
                answer = "Found two unchecked unwraps.",
                durationMs = 15_000L,
            ),
            opens = agentBytes,
        )

        val groupRow = makeRow(
            id = "sub-group-1",
            kind = RowKind.Subagent(
                description = "Subagent A",
                continued = false,
                state = CallPhase.Running,
                answer = "",
            ),
            group = SubagentGroup(
                first = "sub-group-1",
                agents = listOf(listOf(1), listOf(2), listOf(3), listOf(4)),
                open = false,
            ),
        )
        val groupState = GroupState(
            agents = 4,
            running = 2,
            needsYou = 1,
            idle = 0,
            finished = 1,
            ended = 0,
            failed = 0,
            unknown = 0,
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Column {
                    ChatRowView(
                        row = subagentRow,
                        openedAgents = mapOf(agentBytes to opened),
                        callbacks = ChatRowCallbacks(
                            onOpenAgent = { openedAgents += it },
                            onToggleExpand = { expandedRows += it },
                        ),
                    )
                    ChatRowView(
                        row = groupRow,
                        groupState = groupState,
                        callbacks = ChatRowCallbacks(onToggleGroup = { toggledGroups += it }),
                    )
                }
            }
        }

        // Open agent and toggle answer are distinct targets.
        composeRule.onNodeWithTag(ChatRowTags.SUBAGENT_OPEN_AGENT).performClick()
        assertEquals(listOf(agentKey), openedAgents)
        assertTrue(expandedRows.isEmpty())

        composeRule.onNodeWithTag(ChatRowTags.SUBAGENT_TOGGLE_ANSWER).performClick()
        assertEquals(listOf("sub1"), expandedRows)
        assertEquals(1, openedAgents.size)

        // Folded subagent group row toggles group cleanly.
        composeRule.onNodeWithText("4 subagents").assertIsDisplayed()
        composeRule.onNodeWithTag(ChatRowTags.SUBAGENT_GROUP).performClick()
        assertEquals(listOf("sub-group-1"), toggledGroups)
    }

    @Test
    fun talkBackReadsAPlansStepsAndTheAnswersGivenWithTheirRow() {
        val rows = listOf(
            makeRow(
                id = "ask-plan",
                kind = RowKind.Ask(
                    AskRow.Plan(
                        body = MarkdownFixtures.parsed("1. Split the read path.\n2. Run the spec suite."),
                        verdict = PlanVerdict.Open,
                        mode = "",
                        modeName = "",
                        writing = false,
                        note = null,
                        title = "Refactor cache",
                    ),
                ),
            ),
            makeRow(
                id = "ask-q",
                kind = RowKind.Ask(
                    AskRow.Questions(
                        questions = listOf(
                            QuestionView(
                                header = "Storage",
                                question = "Which backend?",
                                multiSelect = false,
                                options = emptyList(),
                                allowOther = true,
                                secret = false,
                            ),
                        ),
                        answers = listOf(
                            AnswerView(picked = listOf("SQLite"), hidden = false, skipped = false, note = null),
                        ),
                        skipped = 0,
                        resolution = Resolution.Answered,
                    ),
                ),
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Column {
                    rows.forEach { row -> ChatRowView(row = row, callbacks = ChatRowCallbacks()) }
                }
            }
        }

        // Each row is one stop whose words carry what it shows under its head,
        // and no label stands in for them.
        composeRule.onNode(
            hasText("Plan proposed") and hasText("Run the spec suite.", substring = true) and
                !SemanticsMatcher.keyIsDefined(SemanticsProperties.ContentDescription),
        ).assertExists()
        composeRule.onNode(
            hasText("Answered") and hasText("SQLite") and
                !SemanticsMatcher.keyIsDefined(SemanticsProperties.ContentDescription),
        ).assertExists()
    }

    @Test
    fun rendersAgentMessageAskRowsErrorCompactionTurnEndAndUnrecognized() {
        val openedImages = mutableListOf<Pair<List<Int>, String>>()
        val rows = listOf(
            makeRow(
                id = "am1",
                kind = RowKind.AgentMessage(
                    from = "worker-1",
                    kind = EnvelopeKind.Finished,
                    text = listOf(ProsePart.Text(plainMarkdown("Completed migration."))),
                    to = "",
                    sent = SendState.Sent,
                    rejection = "",
                ),
            ),
            makeRow(
                id = "ask-q",
                kind = RowKind.Ask(
                    AskRow.Questions(
                        questions = listOf(
                            QuestionView(
                                header = "Storage",
                                question = "Which backend?",
                                multiSelect = false,
                                options = emptyList(),
                                allowOther = true,
                                secret = false,
                            ),
                        ),
                        answers = listOf(
                            AnswerView(
                                picked = listOf("SQLite"),
                                hidden = false,
                                skipped = false,
                                note = "Keep bundled",
                            ),
                        ),
                        skipped = 0,
                        resolution = Resolution.Answered,
                    ),
                ),
            ),
            makeRow(
                id = "ask-grant",
                kind = RowKind.Ask(
                    AskRow.Grant(
                        reason = "Write cache",
                        read = emptyList(),
                        write = listOf("target/"),
                        network = false,
                        hosts = emptyList(),
                        resolution = Resolution.Answered,
                        granted = Granted(read = emptyList(), write = listOf("target/"), network = false, forSession = false),
                    ),
                ),
            ),
            makeRow(
                id = "err1",
                kind = RowKind.Error(
                    errorKind = "Rate limit",
                    message = "Quota exhausted for this window.",
                    attempts = 3,
                    gaveUp = true,
                ),
                attention = true,
            ),
            makeRow(
                id = "cmp1",
                kind = RowKind.Compaction(
                    automatic = true,
                    idle = false,
                    tokensBefore = 148_000L,
                    tokensAfter = 22_000L,
                ),
            ),
            makeRow(
                id = "te1",
                kind = RowKind.TurnEnd(failed = false, durationMs = 102_000L),
            ),
            makeRow(
                id = "unrec1",
                kind = RowKind.Unrecognized(what = "future.v2", summary = "unknown payload"),
            ),
            makeRow(
                id = "img1",
                kind = RowKind.Image(
                    path = "diagram.png",
                    generated = true,
                    image = BlobRef(hash = listOf(7, 7), name = "diagram.png", mime = "image/png", size = 1024L),
                ),
            ),
            makeRow(
                id = "tc1",
                kind = RowKind.ToolCall(
                    server = "github",
                    tool = "create_issue",
                    fact = "#42",
                    state = CallPhase.Succeeded,
                    result = "created",
                ),
            ),
            makeRow(
                id = "child1",
                kind = RowKind.Child(
                    action = ChildAction.Start,
                    name = "payment-tests",
                    host = "build",
                    kind = "Codex",
                    state = CallPhase.Succeeded,
                    result = "",
                ),
            ),
            makeRow(
                id = "ar1",
                kind = RowKind.AutoReview(
                    status = ReviewStatus.Approved,
                    statusWord = "approved",
                    risk = "low",
                    rationale = "Safe refactor",
                    subject = "diff",
                ),
            ),
            makeRow(
                id = "bnd1",
                kind = RowKind.Boundary(kind = BoundaryKind.Cleared, cause = ""),
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Column {
                    rows.forEach { row ->
                        ChatRowView(
                            row = row,
                            callbacks = ChatRowCallbacks(
                                onOpenImage = { hash, label -> openedImages += hash to label },
                            ),
                        )
                    }
                }
            }
        }

        composeRule.onNodeWithText("worker-1").assertIsDisplayed()
        composeRule.onNodeWithText("SQLite").assertIsDisplayed()
        composeRule.onNodeWithText("Granted").assertIsDisplayed()
        composeRule.onNodeWithTag(ChatRowTags.ERROR).assertIsDisplayed()
        composeRule.onNodeWithText("Rate limit").assertIsDisplayed()
        composeRule.onNodeWithText("Compacted · 148k → 22k · automatic").assertIsDisplayed()
        composeRule.onNodeWithText("Worked for 1m 42s").assertIsDisplayed()
        composeRule.onNodeWithTag(ChatRowTags.UNRECOGNIZED).assertIsDisplayed()
        composeRule.onNodeWithText("future.v2").assertIsDisplayed()
        composeRule.onNodeWithText("github · create_issue").assertIsDisplayed()
        composeRule.onNodeWithText("payment-tests on build").assertIsDisplayed()
        composeRule.onNodeWithText("Auto-approved").assertIsDisplayed()
        composeRule.onNodeWithText("Cleared").assertIsDisplayed()

        composeRule.onNodeWithTag(ChatRowTags.IMAGE_THUMBNAIL).performClick()
        assertEquals(listOf(listOf(7, 7) to "diagram.png"), openedImages)
    }

    @Test
    fun hiddenAndCollapsedRowsEmitNothing() {
        val hidden = makeRow(id = "h1", kind = RowKind.Hidden)
        val collapsed = makeRow(id = "c1", kind = RowKind.Stopped, collapsed = true)

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Column {
                    ChatRowView(row = hidden)
                    ChatRowView(row = collapsed)
                }
            }
        }

        assertEquals(0, composeRule.onAllNodesWithTag(ChatRowTags.row("h1")).fetchSemanticsNodes().size)
        assertEquals(0, composeRule.onAllNodesWithTag(ChatRowTags.row("c1")).fetchSemanticsNodes().size)
    }

    @Test
    fun expandedLegacySubagentChildStepAndFormFieldNamesRender() {
        val childStep = makeRow(
            id = "legacy-child",
            kind = RowKind.Explore(
                verb = ExploreVerb.Read,
                subject = "src/lib.rs",
                state = CallPhase.Succeeded,
                result = "",
            ),
            collapsed = true,
        ).copy(parent = "legacy-subagent")
        val formRow = makeRow(
            id = "form-1",
            kind = RowKind.Ask(
                AskRow.Form(
                    server = "deploy",
                    message = "Release settings",
                    fields = listOf("version", "dry_run"),
                    resolution = Resolution.Answered,
                ),
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Column {
                    ChatRowView(row = childStep, shown = true)
                    ChatRowView(row = formRow)
                }
            }
        }

        composeRule.onNodeWithTag(ChatRowTags.row("legacy-child")).assertIsDisplayed()
        composeRule.onNodeWithText("src/lib.rs").assertIsDisplayed()
        composeRule.onNodeWithTag(ChatRowTags.row("form-1")).assertIsDisplayed()
        composeRule.onNodeWithText("version, dry_run").assertIsDisplayed()
    }
}
