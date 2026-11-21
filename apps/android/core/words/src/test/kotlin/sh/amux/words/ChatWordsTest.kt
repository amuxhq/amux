package sh.amux.words

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.ActivityKind
import sh.amux.values.AgentKey
import sh.amux.values.AgentNameProblem
import sh.amux.values.AnswerView
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.AttachmentView
import sh.amux.values.Attention
import sh.amux.values.BlobRef
import sh.amux.values.BoundaryKind
import sh.amux.values.CallPhase
import sh.amux.values.CardState
import sh.amux.values.Changeability
import sh.amux.values.Changeable
import sh.amux.values.Choice
import sh.amux.values.ChoiceOutcome
import sh.amux.values.CommandView
import sh.amux.values.Composer
import sh.amux.values.ContextView
import sh.amux.values.ControlsSummary
import sh.amux.values.Decision
import sh.amux.values.DecisionView
import sh.amux.values.DiffLine
import sh.amux.values.ExitCause
import sh.amux.values.ExploreVerb
import sh.amux.values.FieldProblem
import sh.amux.values.FileChangeView
import sh.amux.values.FileRow
import sh.amux.values.Filing
import sh.amux.values.FleetCard
import sh.amux.values.GroupState
import sh.amux.values.JobRow
import sh.amux.values.JobStop
import sh.amux.values.Kind
import sh.amux.values.LineKind
import sh.amux.values.ModeChoice
import sh.amux.values.Offline
import sh.amux.values.OpenedAgent
import sh.amux.values.PermissionChoice
import sh.amux.values.PermissionGrant
import sh.amux.values.PhaseView
import sh.amux.values.QueuedRow
import sh.amux.values.Reach
import sh.amux.values.Resolution
import sh.amux.values.ReviewStatus
import sh.amux.values.RouteKind
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Run
import sh.amux.values.RunCounts
import sh.amux.values.Scope
import sh.amux.values.Segment
import sh.amux.values.ServerView
import sh.amux.values.SettingsView
import sh.amux.values.SignInState
import sh.amux.values.SignInView
import sh.amux.values.TaskLine
import sh.amux.values.TaskMark
import sh.amux.values.TasksView
import sh.amux.values.Underway
import sh.amux.values.UsageLabel
import sh.amux.values.UsageState
import sh.amux.values.UsageView
import sh.amux.values.UsageWindowView
import sh.amux.values.Waiting
import sh.amux.values.BackgroundKind
import java.time.Instant
import java.time.ZoneOffset
import java.util.Locale

class ChatWordsTest {
    private fun row(
        kind: RowKind,
        id: String = "k",
        attention: Boolean = false,
        decision: Decision? = null,
        parent: String? = null,
        run: Run? = null,
    ): Row = Row(
        id = id,
        order = 1L,
        atMs = 0L,
        kind = kind,
        collapsed = false,
        attention = attention,
        decision = decision,
        group = null,
        opens = null,
        parent = parent,
        run = run,
        turn = null,
    )

    @Test
    fun durationsAndCountsReadTheWayTheVocabularyWritesThem() {
        assertEquals("1m 42s", ChatWords.duration(102_000L))
        assertEquals("8s", ChatWords.duration(8_000L))
        assertEquals("4s", ChatWords.duration(3_300L))
        assertEquals("1s", ChatWords.duration(120L))
        assertEquals("0s", ChatWords.duration(0L))
        assertEquals("1m", ChatWords.duration(59_400L))
        assertEquals("2h", ChatWords.duration(7_200_000L))
        assertEquals("2h 5m", ChatWords.duration(7_500_000L))
        assertEquals("148k", ChatWords.tokens(148_000L))
        assertEquals("812", ChatWords.tokens(812L))
        assertEquals("1.2M", ChatWords.tokens(1_200_000L))
        assertEquals("900 B", ChatWords.bytes(900L))
        assertEquals("12 KB", ChatWords.bytes(12L * 1_024L))
        assertEquals("3.5 MB", ChatWords.bytes((3.5 * 1_048_576.0).toLong()))
        assertEquals(
            "Compacted · 148k \u2192 22k",
            ChatWords.compaction(before = 148_000L, after = 22_000L, automatic = false),
        )
        assertEquals(
            "Compacted while idle · 248k \u2192 31k",
            ChatWords.compaction(before = 248_000L, after = 31_000L, automatic = false, idle = true),
        )
        assertEquals(
            "Compacted while idle",
            ChatWords.compaction(before = null, after = null, automatic = true, idle = true),
        )
        assertEquals(
            "Compacted · automatic",
            ChatWords.compaction(before = null, after = null, automatic = true, idle = false),
        )
        assertEquals("Worked for 1m 42s", ChatWords.turnEnd(durationMs = 102_000L, failed = false))
        assertEquals("Turn ended · failed", ChatWords.turnEnd(durationMs = null, failed = true))
        assertEquals("Thought for 8s", ChatWords.thinking(open = false, durationMs = 8_000L))
        assertEquals("Thinking", ChatWords.thinking(open = true, durationMs = 8_000L))
        assertEquals("Thought", ChatWords.thinking(open = false, durationMs = null))
    }

    @Test
    fun aRunSaysWhatItDidAndThatItContinuesBelow() {
        val run = Run(
            first = "a",
            last = "b",
            steps = 6,
            live = false,
            openBelow = false,
            liveShown = false,
            unresolvedFailure = false,
            open = false,
            counts = RunCounts(
                commands = 0,
                edits = 0,
                reads = 4,
                searches = 2,
                lists = 0,
                fetches = 0,
                subagents = 0,
                other = 0,
            ),
            hidden = null,
            recent = null,
        )
        assertEquals("Read 4 files, searched 2 times", ChatWords.run(run, counts = run.counts))

        val singleSearch = run.copy(
            counts = RunCounts(0, 0, 1, 1, 0, 0, 0, 0),
        )
        assertEquals("Read 1 file, searched once", ChatWords.run(singleSearch, counts = singleSearch.counts))

        val open = Run(
            first = "a",
            last = "b",
            steps = 41,
            live = false,
            openBelow = true,
            liveShown = false,
            unresolvedFailure = false,
            open = false,
            counts = RunCounts(
                commands = 0,
                edits = 0,
                reads = 40,
                searches = 0,
                lists = 0,
                fetches = 0,
                subagents = 0,
                other = 1,
            ),
            hidden = null,
            recent = null,
        )
        assertEquals("Read 40+ files, used 1+ tools", ChatWords.run(open, counts = open.counts))
        assertEquals("Took 41+ steps", ChatWords.run(open, counts = null))
    }

    @Test
    fun runPlacementHandlesOwnOpenLiveFoldedAndFailed() {
        val counts = RunCounts(commands = 1, edits = 0, reads = 2, searches = 0, lists = 0, fetches = 0, subagents = 0, other = 0)
        val hiddenCounts = RunCounts(commands = 0, edits = 0, reads = 1, searches = 0, lists = 0, fetches = 0, subagents = 0, other = 0)

        assertEquals(RunPlacement.Own, RunPlacement.of(row(RowKind.Stopped), open = false) { null })

        val openRun = Run(
            first = "r1",
            last = "r3",
            steps = 3,
            live = false,
            openBelow = false,
            liveShown = false,
            unresolvedFailure = false,
            open = true,
            counts = counts,
            hidden = null,
            recent = 0,
        )
        assertEquals(
            RunPlacement.Step(RunPlacement.Header("Read 2 files, ran 1 command", open = true)),
            RunPlacement.of(row(RowKind.Stopped, id = "r1", run = openRun), open = true) { counts },
        )
        assertEquals(
            RunPlacement.Own,
            RunPlacement.of(row(RowKind.Stopped, id = "r2", run = openRun), open = true) { counts },
        )

        val liveRun = openRun.copy(live = true, open = false, hidden = hiddenCounts)
        assertEquals(
            RunPlacement.Step(RunPlacement.Header("Read 1 file", open = false)),
            RunPlacement.of(row(RowKind.Stopped, id = "r2", run = liveRun), open = false) { counts },
        )
        assertEquals(
            RunPlacement.Own,
            RunPlacement.of(row(RowKind.Stopped, id = "r3", run = liveRun.copy(hidden = null)), open = false) { counts },
        )

        val foldedRun = openRun.copy(live = false, open = false, recent = 0)
        assertEquals(
            RunPlacement.Folded("Read 2 files, ran 1 command"),
            RunPlacement.of(row(RowKind.Stopped, id = "r3", run = foldedRun), open = false) { counts },
        )

        val failedRun = openRun.copy(live = false, open = false, recent = 1)
        assertEquals(
            RunPlacement.Failed,
            RunPlacement.of(row(RowKind.Stopped, id = "r2", run = failedRun), open = false) { counts },
        )
    }

    @Test
    fun aDescribedCommandReadsAsTheAgentsWordsOverTheCommand() {
        assertEquals("Run the quick checks", ChatWords.described("Run the quick checks\nand more"))
        assertNull("Codex says nothing of the kind", ChatWords.described("  "))
        assertEquals("$ just premerge", ChatWords.commandLine("just premerge\n| tail"))
        assertEquals("running", ChatWords.describedState(CallPhase.Running, decided = false))
        assertNull("the exit code says it", ChatWords.describedState(CallPhase.Failed, decided = false))
        assertNull(ChatWords.describedState(CallPhase.Succeeded, decided = false))
    }

    @Test
    fun aCallsVerbSaysWhetherItAsksRunsWasRefusedOrRan() {
        fun words(phase: CallPhase, row: Row): String =
            ChatWords.verb(phase, row, wants = "Wants to run", doing = "Running", done = "Ran")

        assertEquals("Wants to run", words(CallPhase.Asking, row(RowKind.Stopped, attention = true)))
        assertEquals("Running", words(CallPhase.Pending, row(RowKind.Stopped)))
        assertEquals("Running", words(CallPhase.Running, row(RowKind.Stopped)))
        assertEquals("Ran", words(CallPhase.Succeeded, row(RowKind.Stopped)))
        assertEquals("Cancelled", words(CallPhase.Cancelled, row(RowKind.Stopped)))
        val denied = Decision(outcome = DecisionView.Denied, elsewhere = false, granted = null, note = "Use cargo clean")
        assertEquals("Denied", words(CallPhase.Denied, row(RowKind.Stopped, decision = denied)))
        val allowed = Decision(outcome = DecisionView.Allowed, elsewhere = false, granted = PermissionGrant.Session, note = null)
        assertEquals("Allowed", words(CallPhase.Succeeded, row(RowKind.Stopped, decision = allowed)))
        assertEquals("Running", words(CallPhase.Running, row(RowKind.Stopped, decision = allowed)))
        assertEquals("Ran", words(CallPhase.Failed, row(RowKind.Stopped, decision = allowed)))
        val autoApproved = Decision(outcome = DecisionView.AutoApproved, elsewhere = false)
        assertEquals("Auto-approved", words(CallPhase.Succeeded, row(RowKind.Stopped, decision = autoApproved)))
    }

    @Test
    fun aVerbThatNamesTheOutcomeLeavesItOutOfTheMeta() {
        val allowed = Decision(outcome = DecisionView.Allowed, elsewhere = false, granted = PermissionGrant.Session, note = null)
        assertEquals(
            "12s · this session",
            ChatWords.meta(listOf("12s"), row(RowKind.Stopped, decision = allowed), verbPhase = CallPhase.Succeeded),
        )
        val denied = Decision(outcome = DecisionView.Denied, elsewhere = false, granted = null, note = "Use cargo clean")
        assertEquals(
            "",
            ChatWords.meta(emptyList(), row(RowKind.Stopped, decision = denied), verbPhase = CallPhase.Denied, note = false),
        )
        assertNull("the decision says it", ChatWords.state(CallPhase.Denied, decided = true))
        assertEquals("waiting", ChatWords.state(CallPhase.Asking, decided = false))
    }

    @Test
    fun theRailRunsBetweenGridRowsAndBreaksAtProse() {
        val tool = row(RowKind.Background(command = "npm run dev", description = "", running = true, durationMs = null))
        val prose = row(RowKind.Prose(text = listOf(Segment.Text("Done.")), streaming = false, workingNote = false))
        assertEquals(RailJoin(continues = true), RailJoin.of(tool, next = tool))
        assertEquals(RailJoin(continues = false), RailJoin.of(tool, next = prose))
        assertEquals(RailJoin(continues = false), RailJoin.of(tool, next = null))
        assertEquals(RailJoin.NONE, RailJoin.of(prose, next = tool))
        val step = tool.copy(parent = "agent")
        assertEquals(RailJoin(continues = true, nested = true), RailJoin.of(step, next = tool))
    }

    @Test
    fun aDecisionIsMetaWithScopeNoteAndWhereItWasAnswered() {
        val allowed = Decision(outcome = DecisionView.Allowed, elsewhere = false, granted = PermissionGrant.Session, note = null)
        assertEquals(
            "12s · allowed · this session",
            ChatWords.meta(listOf("12s"), row(RowKind.Stopped, decision = allowed)),
        )
        val denied = Decision(outcome = DecisionView.Denied, elsewhere = false, granted = null, note = "Use cargo clean\nplease")
        assertEquals(
            "\u201CUse cargo clean\u201D",
            ChatWords.meta(emptyList(), row(RowKind.Stopped, decision = denied), verbPhase = CallPhase.Denied),
        )
        val elsewhere = Decision(outcome = DecisionView.Allowed, elsewhere = true, granted = null, note = null)
        assertEquals("allowed · in the terminal", ChatWords.decision(elsewhere))
    }

    @Test
    fun aDecidedPermissionSaysWhatWasGranted() {
        val always = Decision(
            outcome = DecisionView.Allowed,
            elsewhere = false,
            granted = PermissionGrant.Claude(
                subjects = listOf("cargo test"),
                directories = emptyList(),
                mode = "",
                modeName = "",
                destination = Scope.Project,
            ),
            note = null,
        )
        assertEquals("allowed · always cargo test in this project", ChatWords.decision(always))
        val switched = Decision(
            outcome = DecisionView.Allowed,
            elsewhere = false,
            granted = PermissionGrant.Claude(
                subjects = emptyList(),
                directories = emptyList(),
                mode = "acceptEdits",
                modeName = "Accept edits",
                destination = Scope.Session,
            ),
            note = null,
        )
        assertEquals("allowed · switched to Accept edits", ChatWords.decision(switched))
        val prefix = Decision(
            outcome = DecisionView.Allowed,
            elsewhere = false,
            granted = PermissionGrant.CommandPrefix(words = listOf("curl", "-s")),
            note = null,
        )
        assertEquals("allowed · commands starting with curl -s", ChatWords.decision(prefix))
        val hosts = Decision(
            outcome = DecisionView.Allowed,
            elsewhere = false,
            granted = PermissionGrant.NetworkHosts(hosts = listOf("example.com")),
            note = null,
        )
        assertEquals("allowed · network access to example.com", ChatWords.decision(hosts))
    }

    @Test
    fun permissionsAndModesAreWordedFromTheCatalogue() {
        fun permission(
            value: String,
            name: String,
            custom: Boolean = false,
            unlisted: Boolean = false,
            neverAsks: Boolean = false,
            settable: Boolean = true,
            current: Boolean = false,
        ) = PermissionChoice(
            value = value,
            displayName = name,
            custom = custom,
            current = current,
            unlisted = unlisted,
            normal = false,
            neverAsks = neverAsks,
            settable = settable,
        )
        assertEquals("Accept edits", ChatWords.permission(permission("acceptEdits", "Accept edits")))
        assertEquals("Custom", ChatWords.permission(permission("", "", custom = true)))
        assertEquals("", ChatWords.permissionDetail(permission("auto", "Auto")))
        assertEquals("Acts without asking", ChatWords.permissionDetail(permission("full", "Full", neverAsks = true)))
        assertEquals("Reported by the agent", ChatWords.permissionDetail(permission("odd", "odd", unlisted = true)))
        assertEquals("Can\u2019t be picked from here", ChatWords.permissionDetail(permission("auto", "Auto", settable = false)))
        assertEquals(
            "Plan",
            ChatWords.mode(ModeChoice("plan", "Plan", current = true, unlisted = false, normal = false, settable = true)),
        )
        val switching = Choice(
            outcome = ChoiceOutcome.AllowAlways(
                subjects = emptyList(),
                directories = emptyList(),
                mode = "acceptEdits",
                modeName = "Accept edits",
                scope = Scope.Session,
            ),
            primary = false,
            takesNote = false,
        )
        assertEquals("Switch to Accept edits", ChatWords.choice(switching))
        assertEquals("Switch to Accept edits", ChatWords.scopeRow(switching).title)
        assertEquals(
            "Permissions · Accept edits",
            ChatWords.permissionsItem(
                SettingsView(
                    models = emptyList(),
                    efforts = emptyList(),
                    permissions = listOf(permission("acceptEdits", "Accept edits", current = true)),
                    modes = emptyList(),
                    changeable = Changeability(Changeable.Pick, Changeable.Pick, Changeable.Pick, Changeable.NotOffered),
                    commands = emptyList(),
                ),
            ),
        )
    }

    @Test
    fun choicesAreStatedAsOutcomes() {
        val always = Choice(
            outcome = ChoiceOutcome.AllowAlways(
                subjects = listOf("cargo test"),
                directories = emptyList(),
                mode = "",
                modeName = "",
                scope = Scope.Project,
            ),
            primary = false,
            takesNote = false,
        )
        assertEquals("Always allow cargo test in this project", ChatWords.choice(always))
        assertEquals(
            "Allow commands starting with curl -s",
            ChatWords.choice(Choice(ChoiceOutcome.AllowSimilar(listOf("curl", "-s")), primary = false, takesNote = false)),
        )
        assertEquals(
            "Deny\u2026",
            ChatWords.choice(Choice(ChoiceOutcome.Deny, primary = false, takesNote = true)),
        )
        assertEquals(
            "Allow network access to localhost",
            ChatWords.choice(Choice(ChoiceOutcome.AllowNetwork(listOf("localhost")), primary = false, takesNote = false)),
        )
        assertEquals(
            "Allow",
            ChatWords.button(Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false)),
        )
        assertEquals(
            "Deny",
            ChatWords.button(Choice(ChoiceOutcome.Deny, primary = false, takesNote = true)),
        )
        val approveSwitch = Choice(ChoiceOutcome.ApprovePlan("auto", "Auto"), primary = true, takesNote = false)
        assertEquals("Approve and switch to Auto", ChatWords.choice(approveSwitch))
        assertEquals("Switch to Auto", ChatWords.planSwitch(approveSwitch))
        assertEquals("I\u2019m done", ChatWords.choice(Choice(ChoiceOutcome.OpenLink, primary = true, takesNote = false)))
    }

    @Test
    fun theActivityLineNamesWhatIsHappeningAndForHowLong() {
        assertEquals(
            "Running cargo test · 12s",
            ChatWords.activity(ActivityKind.Running(key = "k"), elapsedMs = 12_400L, subject = "cargo test"),
        )
        assertEquals("Working · 24s", ChatWords.activity(ActivityKind.Working, elapsedMs = 24_000L, subject = null))
        assertEquals("Thinking", ChatWords.activity(ActivityKind.Thinking, elapsedMs = 24_000L, subject = null))
        assertEquals(
            "3 subagents working",
            ChatWords.activity(ActivityKind.Subagents(count = 3), elapsedMs = 5_000L, subject = null),
        )
        val waiting = GroupState(
            agents = 3,
            running = 2,
            needsYou = 1,
            idle = 0,
            finished = 0,
            ended = 0,
            failed = 0,
            unknown = 0,
        )
        assertEquals(
            "3 subagents · 2 running · 1 waiting on you",
            ChatWords.activity(ActivityKind.Subagents(count = 3), elapsedMs = 5_000L, subject = null, subagents = waiting),
        )
        val working = GroupState(
            agents = 3,
            running = 3,
            needsYou = 0,
            idle = 0,
            finished = 0,
            ended = 0,
            failed = 0,
            unknown = 0,
        )
        assertEquals(
            "3 subagents working",
            ChatWords.activity(ActivityKind.Subagents(count = 3), elapsedMs = 5_000L, subject = null, subagents = working),
        )
        assertEquals("Compacting", ChatWords.activity(ActivityKind.Compacting, elapsedMs = 5_000L, subject = null))
        assertEquals("Running safety checks", ChatWords.activity(ActivityKind.SafetyCheck, elapsedMs = 5_000L, subject = null))
        assertEquals(
            "Retrying · attempt 2 of 10",
            ChatWords.activity(ActivityKind.Retrying(2, 10, null), elapsedMs = 0L, subject = null),
        )
        assertEquals(
            "Retrying · attempt 2 of 10 · in 9s",
            ChatWords.activity(ActivityKind.Retrying(2, 10, 20_000L), elapsedMs = 1_000L, subject = null, sinceMs = 10_000L),
        )
    }

    @Test
    fun theActivityLineComesInWordsSubjectAndTime() {
        val parts = ChatWords.activityParts(ActivityKind.Running(key = "k"), elapsedMs = 12_400L, subject = "cargo test")
        assertEquals("Running", parts.words)
        assertEquals("cargo test", parts.subject)
        assertEquals("12s", parts.time)
        assertNull(ChatWords.activityParts(ActivityKind.Running(key = "k"), elapsedMs = 0L, subject = "").subject)
    }

    @Test
    fun theTaskChipNamesTheTaskInProgress() {
        val tasks = TasksView(
            done = 1,
            total = 3,
            current = "Splitting the lexer",
            entries = listOf(
                TaskLine(subject = "Read the parser", mark = TaskMark.Done),
                TaskLine(subject = "Split the lexer", mark = TaskMark.Current),
                TaskLine(subject = "Run the tests", mark = TaskMark.Todo),
            ),
        )
        assertEquals("Split the lexer", ChatWords.headTask(tasks))
        val waiting = tasks.copy(
            entries = listOf(
                TaskLine(subject = "Read the parser", mark = TaskMark.Done),
                TaskLine(subject = "Split the lexer", mark = TaskMark.Done),
                TaskLine(subject = "Run the tests", mark = TaskMark.Todo),
            ),
        )
        assertEquals("Run the tests", ChatWords.headTask(waiting))
    }

    @Test
    fun theEmptyFieldSaysReplyOrWhySendingWaits() {
        assertEquals(
            "Reply",
            ChatWords.placeholder(Composer.Send, host = "Studio", reach = Reach.Online(RouteKind.Direct), working = false),
        )
        assertEquals(
            "Queue a message",
            ChatWords.placeholder(Composer.Send, host = "Studio", reach = Reach.Online(RouteKind.Direct), working = true),
        )
        assertEquals(
            "Studio is out of reach · your draft is kept",
            ChatWords.placeholder(Composer.Waiting(Waiting.Detached), host = "Studio", reach = Reach.Away, working = false),
        )
        assertEquals(
            "Studio unpaired this phone · your draft is kept",
            ChatWords.placeholder(Composer.Waiting(Waiting.Detached), host = "Studio", reach = Reach.Offline(Offline.Revoked), working = false),
        )
        assertEquals(
            "This phone is signed out · your draft is kept",
            ChatWords.placeholder(Composer.Waiting(Waiting.Detached), host = "Studio", reach = Reach.Offline(Offline.SignedOut), working = false),
        )
        assertEquals(
            "Catching up · your draft is kept",
            ChatWords.placeholder(Composer.Waiting(Waiting.CatchingUp), host = "Studio", reach = Reach.Online(RouteKind.Direct), working = false),
        )
        assertEquals(
            "Reconnecting · your draft is kept",
            ChatWords.placeholder(Composer.Waiting(Waiting.Reconnecting), host = "Studio", reach = Reach.Online(RouteKind.Direct), working = false),
        )
        assertEquals(
            "Reply",
            ChatWords.placeholder(Composer.Waiting(Waiting.Detached), host = "Studio", reach = Reach.Away, connecting = true, working = false),
        )
        assertEquals(
            "Reply",
            ChatWords.placeholder(Composer.Resume, host = "Studio", reach = Reach.Online(RouteKind.Direct), working = false),
        )
    }

    @Test
    fun aSettledAgentsHeadSaysSettledUnlessItFailed() {
        assertEquals("Stopped", ChatWords.exited(ExitCause.Ended))
        assertEquals("Finished", ChatWords.exited(ExitCause.Finished))
        assertEquals("Settled", ChatWords.exited(ExitCause.Ended, settled = true))
        assertEquals("Settled", ChatWords.exited(ExitCause.Finished, settled = true))
        assertEquals("Stopped · code 1", ChatWords.exited(ExitCause.Failed("code 1"), settled = true))
    }

    @Test
    fun closeCutsNamesWhatSettlingStops() {
        assertEquals(
            "desk-1 is working and has 2 background jobs and 1 subagent.",
            ChatWords.closeCuts("desk-1", Attention.Working, jobs = 2, subAgents = 1),
        )
        assertEquals(
            "desk-1 needs you mid-turn.",
            ChatWords.closeCuts("desk-1", PhaseView.NeedsYou, jobs = 0, subAgents = 0),
        )
        assertEquals(
            "desk-1 has 1 background job.",
            ChatWords.closeCuts("desk-1", PhaseView.Idle, jobs = 1, subAgents = 0),
        )
        assertNull(ChatWords.closeCuts("desk-1", Attention.Idle, jobs = 0, subAgents = 0))
    }

    @Test
    fun aReachedLimitIsNamedWithItsResetInAComposerThatStillSends() {
        val nowMs = 1_800_000_000_000L
        val now = Instant.ofEpochMilli(nowMs)
        val usage = UsageView(
            blocked = true,
            windows = listOf(
                UsageWindowView(UsageLabel.FiveHour, UsageState.Blocked, resetsAtMs = nowMs + 7_200_000L, usedPercent = 100.0),
                UsageWindowView(UsageLabel.Weekly(null), UsageState.Ok, resetsAtMs = nowMs + 259_200_000L, usedPercent = 64.0),
            ),
            credits = null,
        )
        val reached = ChatWords.limitReached(usage, now = now, zone = ZoneOffset.UTC, locale = Locale.US)
        assertTrue(reached, reached.startsWith("5-hour limit reached · resets "))
        assertEquals(
            reached,
            ChatWords.placeholder(
                Composer.Send,
                host = "Studio",
                reach = Reach.Online(RouteKind.Direct),
                working = false,
                limit = reached,
            ),
        )
        val past = usage.copy(
            windows = listOf(usage.windows[0].copy(resetsAtMs = nowMs - 1L), usage.windows[1]),
        )
        assertEquals(
            "5-hour limit reached",
            ChatWords.limitReached(past, now = now, zone = ZoneOffset.UTC, locale = Locale.US),
        )
        assertEquals(
            "Usage limit reached",
            ChatWords.limitReached(UsageView(blocked = true, windows = emptyList(), credits = null), now = now),
        )
    }

    @Test
    fun aWindowOfUnknownUseReadsADashAndIsNeverTheFullest() {
        val now = Instant.ofEpochMilli(1_800_000_000_000L)
        val known = UsageWindowView(UsageLabel.FiveHour, UsageState.NearLimit, resetsAtMs = null, usedPercent = 60.0)
        val unknown = UsageWindowView(UsageLabel.Named("overage"), UsageState.Unspecified, resetsAtMs = null, usedPercent = null)
        assertEquals("\u2014", ChatWords.used(unknown))
        assertEquals(
            "5-hour limit reached",
            ChatWords.limitReached(UsageView(blocked = true, windows = listOf(unknown, known), credits = null), now = now),
        )
    }

    @Test
    fun signingInIsAskedForOnTheAgentsHost() {
        assertEquals("Claude needs you to sign in", ChatWords.needsSignIn(Kind.ClaudeSdk))
        assertEquals("Run claude and sign in with /login on desk.", ChatWords.signInSteps(Kind.ClaudeSdk, host = "desk"))
        assertEquals("Codex needs you to sign in", ChatWords.needsSignIn(Kind.Codex))
        assertEquals("Run codex login on desk.", ChatWords.signInSteps(Kind.Codex, host = "desk"))
    }

    @Test
    fun theSignInNoticeEndsSayingSendingTriesAgain() {
        val failed = SignInView(state = SignInState.Failed, account = "ada@example.com", message = "OAuth token has expired.")
        assertEquals(
            "Sign-in failed · ada@example.com\n" +
                "OAuth token has expired.\n" +
                "Run claude and sign in with /login on desk.\n" +
                "Signed in elsewhere? Send a message to try again.",
            ChatWords.signInDetail(failed, kind = Kind.ClaudeSdk, host = "desk"),
        )
        val signedOut = SignInView(state = SignInState.SignedOut, account = "", message = "")
        assertEquals(
            "Signed out\nRun codex login on desk.\nSigned in elsewhere? Send a message to try again.",
            ChatWords.signInDetail(signedOut, kind = Kind.Codex, host = "desk"),
        )
    }

    @Test
    fun aQueuedPromptSaysWhetherItWasSteered() {
        val queued = QueuedRow(
            inputId = listOf(1),
            text = emptyList(),
            steered = false,
            canWithdraw = true,
            canSendNow = true,
            fromAgent = null,
        )
        assertEquals("queued", ChatWords.queued(queued))
        assertEquals("steered", ChatWords.queued(queued.copy(steered = true)))
        assertEquals("queued from worker", ChatWords.queued(queued.copy(fromAgent = "worker")))
        assertEquals("may not have arrived", ChatWords.underway(Underway.MayNotHaveArrived, host = "Studio"))
        assertEquals("waiting for Studio\u2026", ChatWords.underway(Underway.Sending(waiting = true), host = "Studio"))
        assertEquals("sending", ChatWords.underway(Underway.Sending(waiting = false), host = "Studio"))
    }

    @Test
    fun aSecretAnswerIsNeverShownAndSkippedReadsSkipped() {
        assertEquals(
            "answered (hidden)",
            ChatWords.answer(AnswerView(emptyList(), hidden = true, skipped = false, note = null, other = null)),
        )
        assertEquals(
            "macOS, \u201CBSD\u201D",
            ChatWords.answer(AnswerView(listOf("macOS"), hidden = false, skipped = false, note = null, other = "BSD")),
        )
        assertEquals(
            "Skipped",
            ChatWords.answer(AnswerView(emptyList(), hidden = false, skipped = true, note = "later", other = null)),
        )
    }

    @Test
    fun everyFieldProblemAndNameProblemHasWords() {
        val problems = listOf(
            FieldProblem.Required,
            FieldProblem.NotANumber,
            FieldProblem.NotWholeNumber,
            FieldProblem.BelowMinimum(minimum = 1.0),
            FieldProblem.AboveMaximum(maximum = 2.5),
            FieldProblem.TooShort(minLength = 1),
            FieldProblem.TooLong(maxLength = 80),
            FieldProblem.TooFew(minItems = 2),
            FieldProblem.TooMany(maxItems = 3),
        )
        for (problem in problems) {
            assertTrue(problem.toString(), ChatWords.fieldProblem(problem).isNotEmpty())
        }
        assertEquals("At least 1", ChatWords.fieldProblem(FieldProblem.BelowMinimum(1.0)))
        assertEquals("At least 1 character", ChatWords.fieldProblem(FieldProblem.TooShort(1)))
        assertEquals("At most 2.5", ChatWords.fieldProblem(FieldProblem.AboveMaximum(2.5)))
        for (problem in AgentNameProblem.entries) {
            assertTrue(problem.toString(), ChatWords.nameProblem(problem).isNotEmpty())
        }
    }

    @Test
    fun theReviewSaysHowMuchChangedAndWhatAttachingCarries() {
        assertEquals("1 file, 18 added, 2 removed", ChatWords.changes(files = 1, added = 18, removed = 2))
        assertEquals("4 files, 0 added, 28 removed", ChatWords.changes(files = 4, added = 0, removed = 28))
        assertEquals("Attach Review · 1 comment", ChatWords.attachReview(1))
        assertEquals("Attach Review · 3 comments", ChatWords.attachReview(3))
        assertEquals("Comment on 1 line", ChatWords.commentOn(1))
        assertEquals("Comment on 5 lines", ChatWords.commentOn(5))
        assertEquals("Review · 2 comments", ChatWords.chip(AttachmentView.Review(comments = 2, patch = null)))
        assertEquals(
            "Added line 11, Refused(String),",
            ChatWords.spoken(DiffLine(LineKind.Added, "    Refused(String),", emptyList(), newLine = 11, oldLine = null)),
        )
        assertEquals(
            "Removed line 13, Busy,",
            ChatWords.spoken(DiffLine(LineKind.Removed, "    Busy,", emptyList(), newLine = null, oldLine = 13)),
        )
    }

    @Test
    fun theModelChipSaysTheSummaryTheViewHandsOver() {
        val permission = PermissionChoice(
            value = "acceptEdits",
            displayName = "Accept edits",
            custom = false,
            current = true,
            unlisted = false,
            normal = false,
            neverAsks = false,
            settable = true,
        )
        val plan = ModeChoice(
            value = "plan",
            displayName = "Plan",
            current = true,
            unlisted = false,
            normal = false,
            settable = true,
        )
        val summary = ControlsSummary(effort = "high", mode = null, model = "Sonnet 5", permission = null)
        val chip = ChatWords.chip(summary)
        assertEquals("Sonnet 5", chip?.model)
        assertEquals("high", chip?.detail)
        assertEquals(emptyList<String>(), chip?.unusual)

        val unusual = summary.copy(permission = permission, mode = plan)
        assertEquals(listOf("Accept edits", "Plan"), ChatWords.chip(unusual)?.unusual)

        val custom = permission.copy(custom = true, value = "", displayName = "")
        assertEquals("Custom", ChatWords.permission(custom))
        assertNull(ChatWords.chip(ControlsSummary(effort = null, mode = null, model = null, permission = null)))
    }

    @Test
    fun aContextNearItsEndIsSaidOnTheModelPillAndOnlyThen() {
        val summary = ControlsSummary(effort = "high", mode = null, model = "Sonnet 5", permission = null)
        val full = ContextView(usedTokens = 184_000L, nearFull = true, percent = 92L, windowTokens = 200_000L)
        assertEquals(listOf("92% context"), ChatWords.chip(summary, context = full)?.unusual)
        val roomy = full.copy(nearFull = false)
        assertEquals(emptyList<String>(), ChatWords.chip(summary, context = roomy)?.unusual)
        assertEquals("184k of 200k tokens", ChatWords.contextUsed(full))
        assertEquals("184k tokens", ChatWords.contextUsed(full.copy(windowTokens = null)))
    }

    @Test
    fun typedChangesNameTheAgentsCommands() {
        val typed = Changeability(
            model = Changeable.ByTyping("/model"),
            effort = Changeable.ByTyping("/effort"),
            permission = Changeable.Cycle,
            mode = Changeable.NotOffered,
        )
        assertEquals(
            "To change the model or effort, type /model <name> or /effort <level> in the composer.",
            ChatWords.byTyping(typed),
        )
        val picked = Changeability(Changeable.Pick, Changeable.Pick, Changeable.Pick, Changeable.NotOffered)
        assertNull(ChatWords.byTyping(picked))
        assertEquals("1 line", ChatWords.lines(1))
        assertEquals("3 lines", ChatWords.lines(3))
    }

    @Test
    fun anEditCardThatCreatesItsFileSaysSo() {
        fun card(created: Boolean?, files: Int = 1, count: Long = 1L) = AskCard(
            kind = Kind.Codex,
            key = "ask:1",
            itemKey = "",
            position = 1L,
            count = count,
            body = AskBody.Edit(
                path = "/work/notes.txt",
                files = files,
                added = 1,
                removed = 0,
                lines = emptyList(),
                reason = "",
                created = created,
                line = 1,
            ),
            choices = emptyList(),
            questionNote = false,
            questionSkip = false,
            questionReply = false,
            stopsTurn = false,
            defaultToNo = false,
            state = CardState.Open,
        )
        assertEquals("Wants to edit a file", ChatWords.headline(card(false)))
        assertEquals("Wants to create a file", ChatWords.headline(card(true)))
        assertEquals("Wants to edit 3 files", ChatWords.headline(card(false, files = 3)))
        assertNull(ChatWords.position(card(false, count = 1L)))
        assertEquals("1 of 3", ChatWords.position(card(false, count = 3L)))
    }

    @Test
    fun aSubagentStandsAsItsFleetRowSays() {
        fun opened(phase: PhaseView) = OpenedAgent(
            agent = AgentKey(host = listOf(1), agent = listOf(2)),
            name = "Survey",
            kind = Kind.ClaudeSdk,
            phase = phase,
        )
        assertEquals("running", AgentStanding.of(opened(PhaseView.Working), CallPhase.Succeeded).words)
        assertTrue(AgentStanding.of(opened(PhaseView.Working), CallPhase.Succeeded).live)
        assertEquals("waiting on you", AgentStanding.of(opened(PhaseView.NeedsYou), CallPhase.Running).words)
        assertFalse(AgentStanding.of(opened(PhaseView.NeedsYou), CallPhase.Running).live)
        assertEquals("done", AgentStanding.of(opened(PhaseView.Exited(ExitCause.Finished)), CallPhase.Running).words)
        assertEquals("stopped", AgentStanding.of(opened(PhaseView.Exited(ExitCause.Ended)), CallPhase.Failed).words)
        assertEquals("failed", AgentStanding.of(opened(PhaseView.Exited(ExitCause.Failed("code 1"))), CallPhase.Succeeded).words)
        assertEquals("done", AgentStanding.of(null, CallPhase.Succeeded).words)
        assertEquals("running", AgentStanding.of(null, CallPhase.Pending).words)
    }

    @Test
    fun aGroupSaysWhatItsSubagentsComeTo() {
        val state = GroupState(
            agents = 4,
            running = 2,
            needsYou = 1,
            idle = 0,
            finished = 1,
            ended = 0,
            failed = 0,
            unknown = 0,
        )
        assertEquals("4 subagents", ChatWords.subagentGroup(4))
        assertEquals("1 subagent", ChatWords.subagentGroup(1))
        assertEquals("2 running · 1 waiting on you · 1 done", ChatWords.subagentGroupState(state))
        assertEquals(AgentStanding.NeedsYou, ChatWords.subagentGroupStanding(state))

        val ended = GroupState(
            agents = 3,
            running = 0,
            needsYou = 0,
            idle = 0,
            finished = 1,
            ended = 1,
            failed = 0,
            unknown = 1,
        )
        assertEquals("1 done · 1 stopped", ChatWords.subagentGroupState(ended))
        assertEquals(AgentStanding.Done, ChatWords.subagentGroupStanding(ended))
        assertEquals(0, EMPTY_GROUP_STATE.agents)
    }

    @Test
    fun aSubagentsComposerNamesWhoRunsIt() {
        assertEquals("Claude runs this subagent", ChatWords.nativeRunner(Kind.ClaudeSdk))
        assertEquals("Claude runs this subagent", ChatWords.nativeRunner(Kind.ClaudePty))
        assertEquals("Codex runs this subagent", ChatWords.nativeRunner(Kind.Codex))
        assertEquals("Its provider runs this subagent", ChatWords.nativeRunner(null))
        assertEquals("It takes no messages.", ChatWords.nativeTakesNoMessages)
        assertEquals(
            "",
            ChatWords.placeholder(
                Composer.Native(stop = JobStop.Unavailable),
                host = "desk",
                reach = Reach.Online(RouteKind.Direct),
                working = true,
            ),
        )
        assertEquals(JobStop.Unavailable, Composer.Native(JobStop.Unavailable).nativeStop)
        assertNull(Composer.Send.nativeStop)
    }

    @Test
    fun miscRowAndOverviewHelpersFormatCleanly() {
        assertEquals("Cleared · user", ChatWords.boundary(BoundaryKind.Cleared, "user"))
        assertEquals("amux stopped", ChatWords.boundary(BoundaryKind.DaemonLost, ""))
        assertEquals("Auto-approved", ChatWords.autoReview(ReviewStatus.Approved, ""))
        assertEquals("Auto-reviewed custom", ChatWords.autoReview(ReviewStatus.Unspecified, "custom"))
        assertEquals("Searched the web", ChatWords.explore(ExploreVerb.WebSearch))
        assertEquals("a.kt (new file)", ChatWords.fileHeader(FileRow("a.kt", FileChangeView.Created(10), 10, 0)))
        assertEquals("a.kt \u2192 b.kt", ChatWords.fileHeader(FileRow("a.kt", FileChangeView.Moved("b.kt"), 0, 0)))
        assertEquals(
            "Stop cargo test?",
            ChatWords.stopJob(JobRow("cargo test\n--all", 0L, BackgroundKind.Shell, JobStop.Unavailable)),
        )
        assertEquals("github needs sign-in", ChatWords.failed(ServerView("github", "401", needsAuth = true)))
        assertEquals("linear failed to start", ChatWords.failed(ServerView("linear", "boom", needsAuth = false)))
        assertEquals(
            "hello [img.png · 2 KB]",
            ChatWords.text(
                listOf(
                    Segment.Text("hello "),
                    Segment.Attachment(AttachmentView.Image(BlobRef(emptyList(), "img.png", "image/png", 2048L))),
                ),
            ),
        )
        assertEquals(ChatWords.AttachmentKind.Empty, ChatWords.chipKind(AttachmentView.Empty))
        assertEquals("Replied instead", ChatWords.resolution(Resolution.Replied, "Done"))
        assertEquals("CODEX PERMISSIONS", ChatWords.permissionsHeading(Kind.Codex))
        assertEquals("CLAUDE PERMISSIONS", ChatWords.permissionsHeading(Kind.ClaudeSdk))
        assertEquals(
            "/review, from user",
            ChatWords.command(CommandView("/review", "Review code", "", "user", "/", false)),
        )
        val card = FleetCard(
            agent = AgentKey(listOf(1), listOf(2)),
            name = "child",
            handle = "child",
            kind = Kind.Codex,
            attention = Attention.Working,
            cwd = "~/src",
            phaseSinceMs = 0L,
            host = "desk",
            hostReach = Reach.Away,
            children = 0,
            familyAttention = Attention.Working,
            members = 1,
            descendants = 0,
            backgroundJobs = 0,
            closeHeld = false,
            filing = Filing.Open,
            topLevel = false,
            native = false,
        )
        assertEquals("desk away", ChatWords.childState(card))
        assertEquals("Started 1 agent", ChatWords.started(1))
        assertEquals("Started 3 agents", ChatWords.started(3))
    }
}
