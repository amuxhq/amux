package sh.amux.testsupport

import sh.amux.features.ChatSubject
import sh.amux.values.Activity
import sh.amux.values.TaskMark
import sh.amux.values.TaskLine
import sh.amux.values.TasksView
import sh.amux.values.ActivityKind
import sh.amux.values.AgentKey
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.AskRow
import sh.amux.values.Attention
import sh.amux.values.BackgroundKind
import sh.amux.values.CallPhase
import sh.amux.values.CardState
import sh.amux.values.ChangeTotals
import sh.amux.values.ChangedFile
import sh.amux.values.Changeability
import sh.amux.values.Changeable
import sh.amux.values.Changes
import sh.amux.values.ChatFrame
import sh.amux.values.Choice
import sh.amux.values.ChoiceOutcome
import sh.amux.values.Composer
import sh.amux.values.Decision
import sh.amux.values.DecisionView
import sh.amux.values.Directory
import sh.amux.values.EffortChoice
import sh.amux.values.ExitCause
import sh.amux.values.ExploreVerb
import sh.amux.values.FamilyHeader
import sh.amux.values.FileChangeView
import sh.amux.values.FileRow
import sh.amux.values.FileStatus
import sh.amux.values.PatchHunk
import sh.amux.values.HunkStart
import sh.amux.values.FilePatch
import sh.amux.values.Filing
import sh.amux.values.FleetCard
import sh.amux.values.Folder
import sh.amux.values.FormField
import sh.amux.values.FormFieldKind
import sh.amux.values.FormValue
import sh.amux.values.Git
import sh.amux.values.GroupState
import sh.amux.values.HostView
import sh.amux.values.JobRow
import sh.amux.values.JobStop
import sh.amux.values.Kind
import sh.amux.values.LineKind
import sh.amux.values.Markdown
import sh.amux.values.ModelChoice
import sh.amux.values.Offline
import sh.amux.values.OpenedAgent
import sh.amux.values.OptionView
import sh.amux.values.PatchLine
import sh.amux.values.PendingPair
import sh.amux.values.PermissionChoice
import sh.amux.values.PermissionGrant
import sh.amux.values.PhaseView
import sh.amux.values.PlanVerdict
import sh.amux.values.Platform
import sh.amux.values.Provider
import sh.amux.values.ProviderSignIn
import sh.amux.values.QuestionView
import sh.amux.values.QueuedRow
import sh.amux.values.Reach
import sh.amux.values.Refusal
import sh.amux.values.RefusalReason
import sh.amux.values.RouteKind
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Scope
import sh.amux.values.Segment
import sh.amux.values.ServerView
import sh.amux.values.SettingsView
import sh.amux.values.SignInState
import sh.amux.values.SignInView
import sh.amux.values.UsageLabel
import sh.amux.values.UsageState
import sh.amux.values.UsageView
import sh.amux.values.UsageWindowView
import sh.amux.values.TokenKind
import sh.amux.values.SentReviewRef
import sh.amux.values.ReviewViewTab
import sh.amux.values.ReviewView
import sh.amux.values.ReviewRun
import sh.amux.values.ReviewRowOf
import sh.amux.values.ReviewRow
import sh.amux.values.ReviewPage
import sh.amux.values.ReviewHunkHead
import sh.amux.values.ReviewHeader
import sh.amux.values.ReviewGap
import sh.amux.values.ReviewFileHead
import sh.amux.values.ReviewFileFlag
import sh.amux.values.ReviewEmpty
import sh.amux.values.ReviewContextAsk
import sh.amux.values.ReviewCommentState
import sh.amux.values.ReviewCommentRow
import sh.amux.values.ReviewCommentAt
import sh.amux.values.ReviewCodeLine
import sh.amux.values.ReviewBanner
import sh.amux.values.ReviewAnchor

/**
 * The iOS catalogue's invented content, value for value: the chats, asks
 * and machines of `CatalogFixtures` in `apps/apple/Amux/Debug/ComponentCatalog.swift`.
 *
 * A story here and its namesake in the iOS catalogue are compared side by
 * side, so they must draw the same names, ages, words and rows; any
 * difference left in a picture is then the app's, not the data's. A change to
 * one side belongs in the other.
 */
internal object CatalogFixtures {
    /** The catalogue's clock: every age and reset is read against it. */
    const val NOW_MS: Long = 1_790_000_000_000L

    private const val MINUTE_MS: Long = 60_000L
    private const val DAY_MS: Long = 86_400_000L

    // --- Chat ---------------------------------------------------------------

    val subject = ChatSubject(
        name = "refactor-auth",
        host = "Studio",
        directory = "/Users/ada/src/amux",
        branch = "ada/refactor-auth",
        reach = Reach.Online(RouteKind.Direct),
    )
    val closedSubject = subject.copy(filing = Filing.Closed)
    val awaySubject = ChatSubject(
        name = "refactor-auth",
        host = "Studio",
        directory = "/Users/ada/src/amux",
        reach = Reach.Away,
    )

    val git = Git(
        baseBranch = "main",
        branch = "refactor-auth",
        onBranch = ChangeTotals(files = 6, added = 120, removed = 30),
        uncommitted = ChangeTotals(files = 5, added = 56, removed = 20),
    )

    val changes = Changes(
        totals = ChangeTotals(files = 5, added = 56, removed = 20),
        folders = listOf(
            Folder(
                path = "",
                files = listOf(
                    ChangedFile("README.md", "README.md", added = 2, removed = 0, status = FileStatus.Modified, binary = false, language = "markdown"),
                ),
            ),
            Folder(
                path = "crates/amux-ui/src/",
                files = listOf(
                    ChangedFile("crates/amux-ui/src/errors.rs", "errors.rs", added = 20, removed = 0, status = FileStatus.Added, binary = false, language = "rust"),
                    ChangedFile("crates/amux-ui/src/pairing.rs", "pairing.rs", added = 25, removed = 8, status = FileStatus.Modified, binary = false, language = "rust"),
                    ChangedFile("crates/amux-ui/src/strip.rs", "strip.rs", added = 0, removed = 12, status = FileStatus.Deleted, binary = false, language = "rust"),
                ),
            ),
            Folder(
                path = "docs/",
                files = listOf(
                    ChangedFile("docs/PAIRING.md", "PAIRING.md", added = 9, removed = 0, status = FileStatus.Added, binary = false, language = "markdown"),
                ),
            ),
        ),
    )

    /**
     * A busy chat's surroundings: two jobs, five changed files, a tool
     * server that needs signing in and three usage windows.
     */
    val surroundings: ScriptedChat.Surroundings = ScriptedChat.strip(
        usage = UsageView(
            blocked = false,
            windows = listOf(
                UsageWindowView(UsageLabel.FiveHour, UsageState.NearLimit, resetsAtMs = null, usedPercent = 91.0),
                UsageWindowView(UsageLabel.Weekly(model = null), UsageState.Ok, resetsAtMs = NOW_MS + 4 * DAY_MS, usedPercent = 40.0),
                UsageWindowView(UsageLabel.Weekly(model = "Fable"), UsageState.NearLimit, resetsAtMs = NOW_MS + 4 * DAY_MS, usedPercent = 84.0),
            ),
            credits = null,
        ),
        failedServers = listOf(ServerView(name = "github", error = "The token expired.", needsAuth = true)),
        git = git,
        changes = changes,
    ).let { strip ->
        strip.copy(
            overview = strip.overview.copy(
                jobs = listOf(
                    JobRow(
                        command = "npm run dev -- --port 5173",
                        startedAtMs = NOW_MS - 14 * MINUTE_MS,
                        kind = BackgroundKind.Shell,
                        stop = JobStop.Offered("b1"),
                    ),
                    JobRow(
                        command = "cargo watch -x 'test -p amux-ui'",
                        startedAtMs = NOW_MS - 40_000L,
                        kind = BackgroundKind.Shell,
                        stop = JobStop.Stopping,
                    ),
                ),
                changes = changes,
            ),
        )
    }

    /**
     * Prompts behind a running turn: one that can be withdrawn or sent now,
     * one steering, one from another agent.
     */
    val queue = listOf(
        QueuedRow(listOf(1), listOf(Segment.Text("Then run the Windows check.")), steered = false, canWithdraw = true, canSendNow = true),
        QueuedRow(listOf(2), listOf(Segment.Text("Use the new error table instead.")), steered = true, canWithdraw = false, canSendNow = false),
        QueuedRow(listOf(3), listOf(Segment.Text("worker-2 finished the specs.")), steered = false, canWithdraw = false, canSendNow = true, fromAgent = "worker-2"),
    )

    val failedSignIn = SignInView(state = SignInState.Failed, account = "ada@example.com", message = "OAuth token has expired.")

    private fun r(
        id: String,
        order: Long,
        kind: RowKind,
        attention: Boolean = false,
        decision: Decision? = null,
    ): Row = ScriptedChat.row(id, order, kind, attention = attention, decision = decision)

    private fun command(
        command: String,
        state: CallPhase,
        exit: Int? = 0,
        ms: Long? = null,
        output: List<String> = emptyList(),
        more: Long = 0L,
        description: String = "",
    ): RowKind = RowKind.Command(
        command = command,
        description = description,
        state = state,
        outputHead = output,
        moreLines = more,
        outputTrimmed = false,
        outputTail = emptyList(),
        durationMs = ms,
        exitCode = exit,
    )

    private fun edit(path: String, added: Int, removed: Int): RowKind = RowKind.FileChange(
        files = listOf(FileRow(path = path, change = FileChangeView.Edited, added = added, removed = removed)),
        state = CallPhase.Succeeded,
    )

    private fun prompt(text: String): RowKind = RowKind.Prompt(text = listOf(prosePart(text)), steered = false)

    private fun prose(text: String): RowKind =
        RowKind.Prose(text = listOf(prosePart(text)), streaming = false, workingNote = false)

    private fun explore(verb: ExploreVerb, subject: String, state: CallPhase = CallPhase.Succeeded): RowKind =
        RowKind.Explore(verb = verb, subject = subject, state = state, result = "")

    /**
     * A finished turn: the prompt, one run of steps the agent took (a failed
     * check it fixed, a lint it left failing, a command the person refused)
     * and its answer.
     */
    val conversation: List<Row> = listOf(
        r("c02", 2, prompt("Collapse the pairing errors onto one string.")),
    ) + ScriptedChat.run(
        listOf(
            r("c03", 3, RowKind.Thinking(text = "", open = false, durationMs = 8_000L)),
            r("c04", 4, explore(ExploreVerb.Search, "INVALID_PIN")),
            r("c05", 5, explore(ExploreVerb.Read, "crates/wire/src/codes.rs")),
            r("c06", 6, explore(ExploreVerb.Read, "crates/amux-ui/src/pairing.rs")),
            r("c07", 7, edit("crates/amux-ui/src/pairing.rs", 9, 14)),
            r("c08", 8, command("cargo check -p amux-ui", CallPhase.Failed, exit = 101, ms = 4_200L, output = listOf("error[E0308]: mismatched types"), more = 214L)),
            r("c09", 9, edit("crates/amux-ui/src/pairing.rs", 2, 1)),
            r("c10", 10, command("cargo check -p amux-ui", CallPhase.Succeeded, ms = 3_900L)),
            r(
                "c11",
                11,
                RowKind.FileChange(
                    files = listOf(
                        FileRow(
                            path = "crates/amux-ui/tests/spec/pairing_copy.rs",
                            change = FileChangeView.Created(lines = 38),
                            added = 38,
                            removed = 0,
                        ),
                    ),
                    state = CallPhase.Succeeded,
                ),
            ),
            r(
                "c12",
                12,
                command("cargo test -p amux-ui", CallPhase.Succeeded, ms = 12_000L),
                decision = Decision(outcome = DecisionView.Allowed, elsewhere = false, granted = PermissionGrant.Session),
            ),
            r("c13", 13, command("cargo clippy -p amux-ui", CallPhase.Failed, exit = 1, ms = 6_100L, output = listOf("warning: unused import: `PinError`"), more = 12L)),
            r(
                "c14",
                14,
                command("rm -rf target", CallPhase.Denied, exit = null),
                decision = Decision(outcome = DecisionView.Denied, elsewhere = false, note = "Use cargo clean instead"),
            ),
        ),
        failed = setOf("c13"),
    ) + listOf(
        r("c15", 15, prose("Done. The three arms are one now, and the new test asserts on the single string. Clippy still flags an unused import I left for you to decide on.")),
        r("c16", 16, RowKind.TurnEnd(failed = false, durationMs = 102_000L)),
    )

    /** The finished turn, then a second one the agent is still at: five steps in, its newest running. */
    val working: List<Row> = conversation + listOf(
        r("w01", 20, prompt("Drop the unused import and run the whole suite.")),
    ) + ScriptedChat.run(
        listOf(
            r("w02", 21, explore(ExploreVerb.Read, "crates/amux-ui/src/pairing.rs")),
            r(
                "w03",
                22,
                RowKind.FileChange(
                    files = listOf(FileRow(path = "crates/amux-ui/src/pairing.rs", change = FileChangeView.Edited, added = 0, removed = 1)),
                    state = CallPhase.Succeeded,
                ),
            ),
            r("w04", 23, command("cargo clippy -p amux-ui", CallPhase.Succeeded, ms = 5_800L)),
            r("w05", 24, explore(ExploreVerb.Search, "pairing_copy")),
            r("w06", 25, command("cargo test --workspace", CallPhase.Running, exit = null, description = "Run the whole suite")),
        ),
        live = true,
    )

    /** The turn's newest step, the command whose ask stands in the composer's place. */
    val askedCommand: Row = r("x6", 60, command("cargo test -p amux-ui", CallPhase.Asking, exit = null), attention = true)

    /** The working turn's activity: the whole suite running for 33 seconds. */
    val workingActivity = Activity(kind = ActivityKind.Running("w06"), sinceMs = 0L, elapsedMs = 33_000L, turnElapsedMs = 33_000L)

    const val PLAN_TITLE = "Collapse the pairing failures onto one message"
    const val PLAN_SOURCE = "The client maps gRPC statuses onto distinct strings in three places. The protocol refuses " +
        "to distinguish them, so the client must not either.\n\n" +
        "1. One arm, one string\n" +
        "2. Update the three specs"
    val PLAN_BODY: Markdown = MarkdownFixtures.parsed(PLAN_SOURCE)

    /** A prompt, the agent's look around, and the plan it proposed. */
    val planned: List<Row> = listOf(
        r("p1", 1, prompt("Plan how to collapse the pairing errors.")),
        r("e1", 2, explore(ExploreVerb.Search, "INVALID_PIN")),
        r("e2", 3, explore(ExploreVerb.Read, "wire/src/codes.rs")),
        r(
            "l1",
            4,
            RowKind.Ask(
                AskRow.Plan(
                    body = PLAN_BODY,
                    verdict = PlanVerdict.Open,
                    mode = "",
                    modeName = "",
                    writing = false,
                    note = null,
                    title = PLAN_TITLE,
                ),
            ),
        ),
    )

    // --- Subagents ----------------------------------------------------------

    /** An agent its parent's provider runs, as the fleet keys it on Studio. */
    fun subagent(index: Int): AgentKey = AgentKey(host = List(16) { 1 }, agent = List(16) { index })

    private fun opened(index: Int, title: String, phase: PhaseView): OpenedAgent =
        OpenedAgent(agent = subagent(index), name = title, kind = Kind.ClaudeSdk, phase = phase)

    /** The Agent call that started subagent [agent]. */
    private fun call(id: String, order: Long, description: String, agent: Int, state: CallPhase): Row = ScriptedChat.row(
        id,
        order,
        RowKind.Subagent(description = description, continued = false, state = state, answer = "", durationMs = null),
        opens = subagent(agent).agent,
    )

    /** The four subagents the delegating turn starts, by where each stands. */
    private val delegated: List<Triple<Int, String, PhaseView>> = listOf(
        Triple(21, "Survey the relay’s retry path", PhaseView.Working),
        Triple(22, "Check the Windows build", PhaseView.NeedsYou),
        Triple(23, "Find every INVALID_PIN reader", PhaseView.Exited(ExitCause.Finished)),
        Triple(24, "Read the pairing spec", PhaseView.Working),
    )

    val delegatedAgents: Map<List<Int>, OpenedAgent> =
        delegated.associate { (index, title, phase) -> subagent(index).agent to opened(index, title, phase) }

    /** The group row's key: its first call. */
    const val DELEGATED_GROUP = "d04"

    val delegatedState = GroupState(agents = 4, running = 2, needsYou = 1, idle = 0, finished = 1, ended = 0, failed = 0, unknown = 0)

    /** What the activity line says of the subagents still at work. */
    val delegatedLive = GroupState(agents = 3, running = 2, needsYou = 1, idle = 0, finished = 0, ended = 0, failed = 0, unknown = 0)

    /** A turn that hands its work to four subagents at once, its calls folded into one group. */
    val delegating: List<Row> = conversation + listOf(
        r("d01", 30, prompt("Split the pairing audit four ways and report back.")),
        r("d02", 31, RowKind.Thinking(text = "", open = false, durationMs = 3_000L)),
        r("d03", 32, prose("Starting four subagents, one per area.")),
    ) + ScriptedChat.group(
        delegated.mapIndexed { index, (agent, title, _) ->
            call("d0${index + 4}", 33L + index, title, agent = agent, state = CallPhase.Running)
        },
    )

    val delegatingActivity = Activity(kind = ActivityKind.Subagents(3), sinceMs = 0L, elapsedMs = 41_000L, turnElapsedMs = 41_000L)

    /** Subagent 21's own chat: the task its parent gave it, its thinking, the tools it ran and its answer so far. */
    val subagentWork: List<Row> = listOf(
        r("s01", 1, prompt("Survey the relay’s retry path: where a dropped link is retried, and what bounds it.")),
        r("s02", 2, RowKind.Thinking(text = "The retry lives in the link layer, so start at the dialler.", open = false, durationMs = 2_000L)),
    ) + ScriptedChat.run(
        listOf(
            r("s03", 3, explore(ExploreVerb.Search, "retry_after")),
            r("s04", 4, explore(ExploreVerb.Read, "crates/relay-core/src/dial.rs")),
            r("s05", 5, command("cargo test -p relay-core retry", CallPhase.Succeeded, ms = 6_100L)),
        ),
    ) + listOf(
        r("s06", 6, prose("The dialler retries a dropped link with backoff capped at 30 s, and gives up after the link’s deadline. The cap is `MAX_BACKOFF` in dial.rs.")),
        r("s07", 7, explore(ExploreVerb.Read, "crates/relay-core/src/link.rs", CallPhase.Running)),
    )

    fun nativeFrame(stop: JobStop): ChatFrame = ScriptedChat.frame(
        name = "sub-3f9a01c2",
        kind = Kind.ClaudeSdk,
        phase = PhaseView.Working,
        mode = Composer.Native(stop),
        activity = Activity(kind = ActivityKind.Running("s07"), sinceMs = 0L, elapsedMs = 19_000L, turnElapsedMs = 19_000L),
    )

    val nativeSubject = ChatSubject(
        name = "Survey the relay’s retry path",
        host = "Studio",
        directory = "",
        reach = Reach.Online(RouteKind.Direct),
        topLevel = false,
        native = true,
    )

    /** The parent the subagent's chat leads back to. */
    val nativeFamily = FamilyHeader(
        children = emptyList(),
        attention = Attention.Working,
        needYou = 0,
        parent = FleetCard(
            agent = AgentKey(host = List(16) { 1 }, agent = List(16) { 2 }),
            name = "pairing-audit",
            handle = "pairing-audit",
            kind = Kind.ClaudeSdk,
            attention = Attention.Working,
            cwd = "/Users/ada/src/amux",
            phaseSinceMs = 0L,
            host = "Studio",
            hostReach = Reach.Online(RouteKind.Direct),
            children = 4,
            familyAttention = Attention.NeedsYou,
            members = 5,
            descendants = 4,
            backgroundJobs = 0,
            closeHeld = false,
            filing = Filing.Open,
            topLevel = true,
            native = false,
        ),
    )

    // --- The dock ------------------------------------------------------------

    val pairingTasks = TasksView(
        done = 3,
        total = 7,
        current = "Updating the three spec tests",
        entries = listOf(
            TaskLine("Find every call site that maps a status to a string", TaskMark.Done),
            TaskLine("Collapse the match in pairing.rs onto one arm", TaskMark.Done),
            TaskLine("Delete the three unused error constants", TaskMark.Done),
            TaskLine("Update the three spec tests that assert on the old strings", TaskMark.Current),
            TaskLine("Run the spec suite", TaskMark.Todo),
            TaskLine("Check nothing in docs asserts on the old copy", TaskMark.Todo),
            TaskLine("Write the changelog line", TaskMark.Todo),
        ),
    )

    private fun startedCard(
        id: Int,
        name: String,
        kind: Kind,
        attention: Attention,
        host: String,
        exitCause: ExitCause? = null,
    ) = FleetCard(
        agent = AgentKey(host = List(16) { 1 }, agent = List(16) { id }),
        name = name,
        handle = name,
        kind = kind,
        attention = attention,
        cwd = "",
        phaseSinceMs = 0L,
        host = host,
        hostReach = Reach.Online(RouteKind.Direct),
        children = 0,
        familyAttention = attention,
        members = 1,
        descendants = 0,
        backgroundJobs = 0,
        closeHeld = false,
        filing = Filing.Open,
        topLevel = true,
        native = false,
        exitCause = exitCause,
    )

    /** The agents a parent started: one waiting on the person, one working. */
    val family = FamilyHeader(
        children = listOf(
            startedCard(3, "worker-2", Kind.ClaudeSdk, Attention.NeedsYou, "Laptop"),
            startedCard(4, "worker-3", Kind.Codex, Attention.Working, "Studio"),
        ),
        attention = Attention.NeedsYou,
        needYou = 1,
    )

    val docsSweep = startedCard(5, "docs-sweep", Kind.ClaudeSdk, Attention.Exited, "Studio", ExitCause.Finished)

    // --- Settings -----------------------------------------------------------

    private val claudeModeNames = linkedMapOf(
        "default" to "Default",
        "acceptEdits" to "Accept edits",
        "plan" to "Plan",
        "auto" to "Auto",
        "bypassPermissions" to "Bypass permissions",
    )

    fun claudeSdkSettings(mode: String = "plan"): SettingsView {
        val efforts = listOf("low", "medium", "high", "xhigh", "max")
        return SettingsView(
            models = listOf(
                ModelChoice("default", "Default (recommended)", "Opus 5.5 · Most capable for complex work", efforts, current = true, unlisted = false, defaultEffort = "high"),
                ModelChoice("sonnet", "Sonnet", "Sonnet 5 · Best for everyday tasks", efforts, current = false, unlisted = false, defaultEffort = "high"),
                ModelChoice("haiku", "Haiku", "Haiku 4.5 · Fastest for quick answers", emptyList(), current = false, unlisted = false, defaultEffort = null),
            ),
            efforts = efforts.map { EffortChoice(it, current = it == "high", default = it == "high", unlisted = false) },
            permissions = claudeModeNames.map { (value, name) ->
                PermissionChoice(
                    value = value,
                    displayName = name,
                    custom = false,
                    current = value == mode,
                    unlisted = false,
                    normal = value == "default",
                    neverAsks = value == "bypassPermissions",
                    settable = true,
                )
            },
            modes = emptyList(),
            changeable = Changeability(Changeable.Pick, Changeable.Pick, Changeable.Pick, Changeable.NotOffered),
            commands = emptyList(),
        )
    }

    /** Terminal Claude: its model and effort are changed by typing, its permission by cycling. */
    val settingsClaudePty = SettingsView(
        models = listOf(ModelChoice("claude-sonnet-5", "Sonnet 5", "", emptyList(), current = true, unlisted = true, defaultEffort = null)),
        efforts = listOf(EffortChoice("high", current = true, default = false, unlisted = true)),
        permissions = listOf(
            PermissionChoice("acceptEdits", "Accept edits", custom = false, current = true, unlisted = false, normal = false, neverAsks = false, settable = false),
        ),
        modes = emptyList(),
        changeable = Changeability(
            model = Changeable.ByTyping("/model"),
            effort = Changeable.ByTyping("/effort"),
            permission = Changeable.Cycle,
            mode = Changeable.NotOffered,
        ),
        commands = emptyList(),
    )

    // --- Asks ---------------------------------------------------------------

    fun card(
        kind: Kind,
        body: AskBody,
        choices: List<Choice>,
        state: CardState = CardState.Open,
        count: Long = 1L,
    ): AskCard = AskCard(
        kind = kind,
        key = "ask-1",
        itemKey = "x6",
        position = 1L,
        count = count,
        body = body,
        choices = choices,
        questionNote = kind != Kind.ClaudePty,
        questionSkip = kind != Kind.ClaudePty,
        questionReply = true,
        stopsTurn = true,
        defaultToNo = false,
        state = state,
    )

    private fun choice(outcome: ChoiceOutcome, primary: Boolean = false, takesNote: Boolean = false) =
        Choice(outcome, primary = primary, takesNote = takesNote)

    val sdkPermissionChoices = listOf(
        choice(ChoiceOutcome.AllowOnce, primary = true),
        choice(ChoiceOutcome.AllowAlways(listOf("cargo test"), emptyList(), "", "", Scope.Project)),
        choice(ChoiceOutcome.AllowForSession),
        choice(ChoiceOutcome.Deny, takesNote = true),
        choice(ChoiceOutcome.DenyAndStop, takesNote = true),
    )

    val codexPlanChoices = listOf(
        choice(ChoiceOutcome.ApprovePlan(mode = "", modeName = ""), primary = true),
        choice(ChoiceOutcome.SendBack, takesNote = true),
    )

    val claudePlanChoices = listOf(
        choice(ChoiceOutcome.ApprovePlan(mode = "auto", modeName = "Auto"), primary = true),
        choice(ChoiceOutcome.ApprovePlan(mode = "", modeName = "")),
        choice(ChoiceOutcome.SendBack, takesNote = true),
    )

    val allowOrDeny = listOf(
        choice(ChoiceOutcome.AllowOnce, primary = true),
        choice(ChoiceOutcome.Deny, takesNote = true),
    )

    val codexCommandChoices = listOf(
        choice(ChoiceOutcome.AllowOnce, primary = true),
        choice(ChoiceOutcome.AllowForSession),
        choice(ChoiceOutcome.AllowSimilar(prefix = listOf("curl", "-s"))),
        choice(ChoiceOutcome.AllowNetwork(hosts = listOf("localhost"))),
        choice(ChoiceOutcome.Deny),
        choice(ChoiceOutcome.DenyAndStop),
    )

    val codexCommandLargeTextChoices = listOf(
        choice(ChoiceOutcome.AllowOnce, primary = true),
        choice(ChoiceOutcome.AllowForSession),
        choice(ChoiceOutcome.AllowSimilar(prefix = listOf("curl", "-s"))),
        choice(ChoiceOutcome.Deny),
    )

    /** The command the chat and the command asks run: the spec suite. */
    val specSuite = AskBody.Command(
        command = "cargo test -p amux-ui",
        cwd = "~/src/amux",
        reason = "",
        description = "Run the spec suite",
    )

    val bareSpecSuite = AskBody.Command(command = "cargo test -p amux-ui", cwd = "", reason = "", description = "")

    val curlHealth = AskBody.Command(
        command = "curl -s localhost:8080/health",
        cwd = "~/src/amux",
        reason = "Check the dev server",
        description = "",
    )

    val plan = AskBody.Plan(body = PLAN_BODY, title = PLAN_TITLE)

    /** An edit's patch as the shared view hands it: one hunk, where it starts, then its lines. */
    val diff: List<FilePatch> = listOf(
        FilePatch(
            hunks = listOf(
                PatchHunk(
                    start = HunkStart(old = 12, new = 12),
                    lines = listOf(
                        PatchLine(LineKind.Context, "  match status {", number = 12),
                        PatchLine(LineKind.Removed, "      Code::NotFound => \"no such host\",", number = 13),
                        PatchLine(LineKind.Removed, "      Code::InvalidPin => \"wrong PIN\",", number = 14),
                        PatchLine(LineKind.Added, "      _ => \"could not pair, check the code\",", number = 13),
                        PatchLine(LineKind.Context, "  }", number = 14),
                    ),
                ),
            ),
        ),
    )

    /** A new file's whole body, every line added. */
    val created: List<FilePatch> = listOf(
        FilePatch(
            hunks = listOf(
                PatchHunk(
                    start = HunkStart(old = 0, new = 1),
                    lines = listOf(
                        PatchLine(LineKind.Added, "//! Every reason a pairing is refused.", number = 1),
                        PatchLine(LineKind.Added, "pub use super::Code;", number = 2),
                    ),
                ),
            ),
        ),
    )

    /** A schema with a defaulted repository, a choice of labels and a defaulted toggle. */
    val formFields = listOf(
        FormField(
            name = "repository",
            title = "Repository",
            description = "",
            required = true,
            kind = FormFieldKind.Text(maxLength = null, minLength = null),
            initial = FormValue.Text("jlw/amux"),
        ),
        FormField(
            name = "labels",
            title = "Labels",
            description = "",
            required = false,
            kind = FormFieldKind.Choice(listOf("bug", "ios", "docs")),
            initial = FormValue.Choice(0),
        ),
        FormField(
            name = "assign",
            title = "Assign to me",
            description = "",
            required = false,
            kind = FormFieldKind.Toggle,
            initial = FormValue.Toggle(true),
        ),
    )

    val redactionQuestion = QuestionView(
        header = "Owner",
        question = "Which crate should own the redaction table?",
        multiSelect = false,
        options = listOf(
            OptionView("amux-core", "Every client already depends on it", "", recommended = true),
            OptionView("amux-ui", "Closest to where the copy is used", "", recommended = false),
            OptionView("A new crate", "", "", recommended = false),
        ),
        allowOther = true,
        secret = false,
    )

    /** Codex names files as code, between backticks. */
    val codeQuestion = QuestionView(
        header = "Exact target",
        question = "`q1/README.md` is absent; `../plan1/README.md` has two lines (`UPDATED`, `done`). Which should be used?",
        multiSelect = false,
        options = listOf(
            OptionView("Create `README.md`", "Create the missing file with exactly one line: `UPDATED`.", "", recommended = true),
            OptionView("Edit the other", "Change `../plan1/README.md` instead.", "", recommended = false),
        ),
        allowOther = true,
        secret = false,
    )

    val platformQuestion = QuestionView(
        header = "Platforms",
        question = "Which platforms should the release build?",
        multiSelect = true,
        options = listOf(
            OptionView("macOS", "", "", recommended = false),
            OptionView("Linux", "x86_64 and arm64", "", recommended = false),
            OptionView("Windows", "", "", recommended = false),
        ),
        allowOther = true,
        secret = false,
    )

    val previewQuestion = QuestionView(
        header = "Layout",
        question = "Which layout for the host list?",
        multiSelect = false,
        options = listOf(
            OptionView(
                "Cards",
                "One card per host",
                "┌──────────────────────┐\n" +
                    "│ Studio        ● live │\n" +
                    "│ ~/src/amux  3 agents │\n" +
                    "├──────────────────────┤\n" +
                    "│ Laptop       ○ away  │\n" +
                    "└──────────────────────┘",
                recommended = false,
            ),
            OptionView("Grouped list", "", "Studio · live\nLaptop · away", recommended = false),
        ),
        allowOther = false,
        secret = false,
    )

    val secretQuestion = QuestionView(
        header = "Token",
        question = "Paste the deploy token",
        multiSelect = false,
        options = emptyList(),
        allowOther = true,
        secret = true,
    )

    val threeQuestions = listOf(
        platformQuestion,
        QuestionView(
            header = "Rollout",
            question = "How should the migration roll out?",
            multiSelect = false,
            options = listOf(
                OptionView("Behind a flag", "Off by default for one release", "", recommended = false),
                OptionView("All at once", "", "", recommended = false),
            ),
            allowOther = true,
            secret = false,
        ),
        QuestionView(
            header = "Logging",
            question = "What should be logged?",
            multiSelect = false,
            options = listOf(
                OptionView("Everything", "", "", recommended = false),
                OptionView("Nothing", "", "", recommended = false),
            ),
            allowOther = true,
            secret = false,
        ),
    )

    val closedAsk = CardState.Rejected(Refusal(reason = RefusalReason.ClosedAsk, detail = null))

    // --- Machines -----------------------------------------------------------

    /** A machine as the hosts tab knows it; a candidate is one nobody paired. */
    fun machine(
        index: Int,
        name: String,
        platform: Platform?,
        reach: Reach,
        trusted: Boolean = true,
        candidate: Boolean = false,
        addrs: List<String> = emptyList(),
        relayOnly: Boolean = false,
        pairingOpen: Boolean = false,
        version: String? = "0.8.0",
        providers: List<ProviderSignIn> = listOf(
            ProviderSignIn(Provider.Claude, signedIn = true),
            ProviderSignIn(Provider.Codex, signedIn = false),
        ),
    ): HostView = HostView(
        hostId = List(16) { index },
        link = 0L,
        sameNetwork = false,
        name = name,
        local = false,
        trusted = trusted,
        candidate = candidate,
        reach = reach,
        addrs = addrs,
        relayOnly = relayOnly,
        pairingOpen = pairingOpen,
        current = false,
        providers = providers,
        platform = platform,
        version = version,
    )

    val nearbyLaptop = machine(
        5, "laptop", Platform.Macos, Reach.Online(RouteKind.Direct),
        trusted = false, candidate = true, addrs = listOf("192.168.1.24:4433"), version = null, providers = emptyList(),
    )

    /** The laptop with its pairing window open, which raises the keypad. */
    val openLaptop = nearbyLaptop.copy(pairingOpen = true)

    /** A laptop only the relay reaches. */
    val relayLaptop = machine(
        5, "laptop", Platform.Macos, Reach.Online(RouteKind.Relay),
        trusted = false, candidate = true, relayOnly = true, version = null, providers = emptyList(),
    )

    /** A machine on the account nobody paired, which only the relay reaches. */
    val accountServer = machine(
        7, "nova-test", Platform.Linux, Reach.Online(RouteKind.Unspecified),
        trusted = false, candidate = true, relayOnly = true, version = null, providers = emptyList(),
    )

    val deskHost = machine(
        1, "desk", Platform.Macos, Reach.Online(RouteKind.Direct),
        addrs = listOf("192.168.1.20:4433", "[fe80::1c2b:9aff:fe31:7d10]:4433"),
    )

    /** A machine that unpaired this phone. */
    val oldLaptop = machine(6, "old-laptop", Platform.Macos, Reach.Offline(Offline.Revoked))

    /** Every group a machine can be in, and one found nearby and not paired. */
    val machines: List<HostView> = listOf(
        deskHost,
        machine(2, "build-box", Platform.Linux, Reach.Online(RouteKind.Relay)),
        machine(3, "mini", Platform.Macos, Reach.Away),
        machine(4, "garage-pi", Platform.Linux, Reach.Offline(Offline.Plain)),
        oldLaptop,
        nearbyLaptop,
    )

    val pendingLaptop = PendingPair(
        token = listOf(1, 2, 3),
        hostId = nearbyLaptop.hostId,
        name = "laptop",
        fingerprint = fingerprint(5),
        expiresAtMs = NOW_MS + (4 * 60 + 12) * 1_000L,
        via = RouteKind.Direct,
    )

    /** A key's fingerprint as the runtime writes one: 64 hex characters. */
    fun fingerprint(seed: Int): String =
        (0 until 32).joinToString("") { "%02x".format((seed * 37 + it * 11) and 0xff) }

    // --- New agent ----------------------------------------------------------

    val newAgentHosts: List<HostView> = listOf(
        HostView(
            hostId = List(16) { 7 },
            link = 0L,
            sameNetwork = false,
            name = "desk",
            local = false,
            trusted = true,
            candidate = false,
            reach = Reach.Online(RouteKind.Direct),
            addrs = emptyList(),
            relayOnly = false,
            pairingOpen = false,
            current = true,
            providers = listOf(
                ProviderSignIn(Provider.Claude, signedIn = true),
                ProviderSignIn(Provider.Codex, signedIn = true),
            ),
            platform = Platform.Macos,
        ),
        HostView(
            hostId = List(16) { 8 },
            link = 0L,
            sameNetwork = false,
            name = "laptop",
            local = false,
            trusted = true,
            candidate = false,
            reach = Reach.Away,
            addrs = emptyList(),
            relayOnly = false,
            pairingOpen = false,
            current = false,
            providers = emptyList(),
            platform = Platform.Macos,
        ),
    )

    val recentDirectories = listOf(
        Directory(path = "~/src/amux", name = "amux"),
        Directory(path = "~/src/amuxcloud", name = "amuxcloud"),
        Directory(path = "~/src/promo", name = "promo"),
    )

    val repositoryDirectories = listOf(
        Directory(path = "~/src/amux", name = "amux"),
        Directory(path = "~/src/dotfiles", name = "dotfiles"),
        Directory(path = "~/src/skills", name = "skills"),
    )

    /** Claude's catalogue on the desk, as New Agent offers it. */
    val claudeCatalogue = SettingsView(
        models = listOf(
            ModelChoice("default", "Default (recommended)", "Opus 5.5 · Most capable for complex work", listOf("low", "medium", "high"), current = true, unlisted = false, defaultEffort = "high"),
            ModelChoice("sonnet", "Sonnet", "Sonnet 5 · Best for everyday tasks", listOf("low", "medium", "high"), current = false, unlisted = false, defaultEffort = "high"),
        ),
        efforts = listOf("low", "medium", "high").map { EffortChoice(it, current = it == "high", default = it == "high", unlisted = false) },
        permissions = listOf(
            PermissionChoice("default", "Default", custom = false, current = true, unlisted = false, normal = true, neverAsks = false, settable = true),
            PermissionChoice("acceptEdits", "Accept edits", custom = false, current = false, unlisted = false, normal = false, neverAsks = false, settable = true),
            PermissionChoice("plan", "Plan", custom = false, current = false, unlisted = false, normal = false, neverAsks = false, settable = true),
            PermissionChoice("bypassPermissions", "Bypass permissions", custom = false, current = false, unlisted = false, normal = false, neverAsks = true, settable = true),
        ),
        modes = emptyList(),
        changeable = Changeability(Changeable.Pick, Changeable.Pick, Changeable.Pick, Changeable.NotOffered),
        commands = emptyList(),
    )
}

/**
 * An invented review of one agent's change to how its store opens: the pages
 * the shared runtime would hand over for it, as the iOS catalogue's review
 * examples draw them.
 */
object ReviewStory {
    const val SNAPSHOT: String = "5a1c0e7d9b3f42a8c6e1d0f9b7a5c3e1d2f4a6b8"
    const val BASE: String = "b0e2d4f6a8c1e3a5c7e9b1d3f5a7c9e1b3d5f7a9"
    const val LAST_ROUND: String = "3c5e7a9b1d3f5a7c9e1b3d5f7a9c1e3a5c7e9b1d"
    const val OPEN: String = "crates/store/src/open.rs"
    const val LIB: String = "crates/store/src/lib.rs"
    const val LOCK: String = "Cargo.lock"

    val anchor: ReviewAnchor = ReviewAnchor(commit = SNAPSHOT, path = OPEN, start = 41, end = 42)

    val sent: SentReviewRef = SentReviewRef(reviewId = listOf(1), round = 1, inputId = listOf(0x0a))

    /** What the agent read of the first round. */
    val sentText: String = """
        crates/store/src/open.rs, line 41, in impl Store › pub fn open:
        +        let conn = Connection::open(path)?;
        Return the store’s own error, not rusqlite’s.

        crates/store/src/open.rs, line 42, in impl Store › pub fn open:
        +        conn.busy_timeout(BUSY)?;
        Name it for what it waits on.
    """.trimIndent()

    /** The removed line 41 to the added line 42 of the store's open, by row. */
    fun twoLines(page: ReviewPage): Pair<Int, Int> =
        index("l/$OPEN/41/0", page) to index("l/$OPEN/0/42", page)

    fun index(key: String, page: ReviewPage): Int = page.rows.indexOfFirst { it.key == key }.coerceAtLeast(0)

    fun firstComment(page: ReviewPage): ReviewCommentRow? =
        page.rows.firstNotNullOfOrNull { (it.of as? ReviewRowOf.Comment)?.value }

    // --- Pages ---------------------------------------------------------------

    /**
     * All changes on a first look: three files, the lock file folded, and two
     * drafts, one on the lines it is about and one on a whole file.
     */
    fun page(banners: List<ReviewBanner> = emptyList(), folded: Boolean = false): ReviewPage {
        val rows = mutableListOf<ReviewRow>()
        rows += head(OPEN, added = 2, removed = 1, comments = 1, folded = folded)
        if (!folded) {
            rows += openBody(
                drafts = listOf(
                    comment(
                        listOf(0x01), OPEN, "Why five seconds? The phone’s store waits two.",
                        ReviewCommentState.Draft, ReviewCommentAt.Lines(start = 42, end = 42),
                    ),
                ),
            )
        }
        rows += head(LIB, added = 3, removed = 0, comments = 1, folded = folded)
        if (!folded) {
            rows += comment(
                listOf(0x02), LIB, "Keep the flags beside the open that uses them.",
                ReviewCommentState.Draft, ReviewCommentAt.File,
            )
            rows += libBody()
        }
        rows += head(LOCK, added = 4, removed = 1, flags = listOf(ReviewFileFlag.Lockfile), folded = true)
        return ReviewPage(
            banners = banners, rows = rows, toFetch = emptyList(), opening = false, empty = null,
            header = header(view = ReviewView.All, files = 3, added = 9, removed = 2, drafts = 2, sent = 0, rounds = 0),
        )
    }

    /**
     * The next look, since the last round: one file changed since, an earlier
     * comment sent and outdated, another whose code nearby changed, and a
     * draft on a new line.
     */
    fun sinceLast(): ReviewPage {
        val sentInFirst = ReviewCommentState.Sent(round = 1)
        val rows = listOf(
            head(OPEN, added = 1, removed = 1, comments = 3, changedSinceLast = true),
            comment(
                listOf(0x0a), OPEN, "Return the store’s own error, not rusqlite’s.", sentInFirst,
                ReviewCommentAt.Outdated,
                snippet = listOf(line(OPEN, LineKind.Removed, old = 41, text = "        let conn = Connection::open(path)?;")),
                folded = true,
            ),
            gap(OPEN, hidden = 39, up = ask(OPEN, 1, 39, "impl Store"), down = null),
            hunk(OPEN, old = 40, new = 40, listOf("impl Store", "pub fn open")),
            code(OPEN, LineKind.Context, old = 40, new = 40, text = "    pub fn open(path: &Path) -> Result<Store, StoreError> {"),
            code(OPEN, LineKind.Context, old = 41, new = 41, text = "        let conn = Connection::open_with_flags(path, FLAGS)?;"),
            code(OPEN, LineKind.Removed, old = 42, text = "        conn.busy_timeout(BUSY)?;", changed = listOf("BUSY")),
            code(
                OPEN, LineKind.Added, new = 42, text = "        conn.busy_timeout(BUSY_WAIT)?;",
                changed = listOf("BUSY_WAIT"), commented = true,
            ),
            comment(
                listOf(0x0b), OPEN, "Name it for what it waits on.", sentInFirst,
                ReviewCommentAt.Lines(start = 42, end = 42), changedNear = true,
            ),
            comment(
                listOf(0x03), OPEN, "Thanks. Two seconds matches the phone.", ReviewCommentState.Draft,
                ReviewCommentAt.Lines(start = 42, end = 42),
            ),
            code(OPEN, LineKind.Context, old = 43, new = 43, text = "        migrate(&conn)?;"),
            gap(OPEN, hidden = null, up = null, down = ask(OPEN, 44, 63, null)),
        )
        return ReviewPage(
            banners = emptyList(), rows = rows, toFetch = emptyList(), opening = false, empty = null,
            header = header(view = ReviewView.SinceLast, files = 1, added = 1, removed = 1, drafts = 1, sent = 2, rounds = 1),
        )
    }

    /** Nothing left to review. */
    fun empty(): ReviewPage = ReviewPage(
        banners = emptyList(), rows = emptyList(), toFetch = emptyList(), opening = false, empty = ReviewEmpty.Nothing,
        header = header(view = ReviewView.All, files = 0, added = 0, removed = 0, drafts = 0, sent = 0, rounds = 0),
    )

    /** Nothing changed since the last round, which still lists the comment sent in it. */
    fun nothingSince(): ReviewPage = ReviewPage(
        banners = emptyList(),
        rows = listOf(
            head(OPEN, added = 0, removed = 0, comments = 1),
            comment(
                listOf(0x0b), OPEN, "Name it for what it waits on.", ReviewCommentState.Sent(round = 1),
                ReviewCommentAt.Lines(start = 42, end = 42),
                snippet = listOf(line(OPEN, LineKind.Added, new = 42, text = "        conn.busy_timeout(BUSY)?;")),
            ),
        ),
        toFetch = emptyList(), opening = false, empty = ReviewEmpty.NothingSince,
        header = header(view = ReviewView.SinceLast, files = 0, added = 0, removed = 0, drafts = 0, sent = 1, rounds = 1),
    )

    /** An agent whose computer runs no reviews. */
    fun notOffered(): ReviewPage = ReviewPage(
        banners = listOf(ReviewBanner.NotOffered), rows = emptyList(), toFetch = emptyList(),
        opening = false, empty = null, header = null,
    )

    /**
     * The first round as its chip opens it: its whole diff, from main to the
     * round's snapshot, with the two comments it sent on their lines.
     */
    fun round(): ReviewPage {
        val scope = listOf("impl Store", "pub fn open")
        val sentInFirst = ReviewCommentState.Sent(round = 1)
        val rows = listOf(
            head(OPEN, added = 2, removed = 0, comments = 2),
            hunk(OPEN, old = 40, new = 40, scope),
            code(OPEN, LineKind.Context, old = 40, new = 40, text = "    pub fn open(path: &Path) -> Result<Store, StoreError> {"),
            code(OPEN, LineKind.Added, new = 41, text = "        let conn = Connection::open(path)?;", commented = true),
            comment(
                listOf(0x0a), OPEN, "Return the store’s own error, not rusqlite’s.", sentInFirst,
                ReviewCommentAt.Lines(start = 41, end = 41), scope = scope,
            ),
            code(OPEN, LineKind.Added, new = 42, text = "        conn.busy_timeout(BUSY)?;", commented = true),
            comment(
                listOf(0x0b), OPEN, "Name it for what it waits on.", sentInFirst,
                ReviewCommentAt.Lines(start = 42, end = 42), scope = scope,
            ),
            code(OPEN, LineKind.Context, old = 41, new = 43, text = "        migrate(&conn)?;"),
            code(OPEN, LineKind.Context, old = 42, new = 44, text = "        Ok(Store { conn })"),
            code(OPEN, LineKind.Context, old = 43, new = 45, text = "    }"),
            head(LIB, added = 3, removed = 0),
        ) + libBody()
        return ReviewPage(
            banners = emptyList(), rows = rows, toFetch = emptyList(), opening = false, empty = null,
            header = ReviewHeader(
                baseBranch = "main", baseCommit = BASE, snapshot = LAST_ROUND, view = ReviewView.All,
                comparedWith = BASE, comparedBase = "", views = listOf(ReviewViewTab(view = ReviewView.All, files = 2)),
                rounds = 0, files = 2, added = 5, removed = 0, drafts = 0, sent = 2,
                discardDrafts = false, startOver = true,
            ),
        )
    }

    // --- Pieces --------------------------------------------------------------

    private fun header(
        view: ReviewView,
        files: Int,
        added: Int,
        removed: Int,
        drafts: Int,
        sent: Int,
        rounds: Int,
    ): ReviewHeader {
        val views = mutableListOf(ReviewViewTab(view = ReviewView.All, files = 3))
        if (rounds > 0) views += ReviewViewTab(view = ReviewView.SinceLast, files = 1)
        return ReviewHeader(
            baseBranch = "main", baseCommit = BASE, snapshot = SNAPSHOT, view = view,
            comparedWith = if (view == ReviewView.All) BASE else LAST_ROUND,
            comparedBase = if (view == ReviewView.All) "" else BASE,
            views = views, rounds = rounds, files = files, added = added, removed = removed,
            drafts = drafts, sent = sent, discardDrafts = drafts > 0, startOver = true,
        )
    }

    /** The change to how the store opens: a gap above, the hunk in `impl Store › pub fn open`, and the rest below. */
    private fun openBody(drafts: List<ReviewRow>): List<ReviewRow> = listOf(
        gap(OPEN, hidden = 39, up = ask(OPEN, 1, 39, "impl Store"), down = null),
        hunk(OPEN, old = 40, new = 40, listOf("impl Store", "pub fn open")),
        code(OPEN, LineKind.Context, old = 40, new = 40, text = "    pub fn open(path: &Path) -> Result<Store, StoreError> {"),
        code(OPEN, LineKind.Removed, old = 41, text = "        let conn = Connection::open(path)?;", changed = listOf("open")),
        code(
            OPEN, LineKind.Added, new = 41, text = "        let conn = Connection::open_with_flags(path, FLAGS)?;",
            changed = listOf("open_with_flags", "FLAGS"),
        ),
        code(OPEN, LineKind.Added, new = 42, text = "        conn.busy_timeout(BUSY)?;", commented = true),
    ) + drafts + listOf(
        code(OPEN, LineKind.Context, old = 42, new = 43, text = "        migrate(&conn)?;"),
        code(OPEN, LineKind.Context, old = 43, new = 44, text = "        Ok(Store { conn })"),
        code(OPEN, LineKind.Context, old = 44, new = 45, text = "    }"),
        gap(OPEN, hidden = null, up = null, down = ask(OPEN, 46, 65, null)),
    )

    private fun libBody(): List<ReviewRow> = listOf(
        gap(LIB, hidden = 11, up = ask(LIB, 1, 11, null), down = null),
        hunk(LIB, old = 12, new = 12, emptyList()),
        code(LIB, LineKind.Context, old = 12, new = 12, text = "use rusqlite::{Connection, OpenFlags};"),
        code(LIB, LineKind.Added, new = 13, text = "const FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_WRITE;"),
        code(LIB, LineKind.Added, new = 14, text = "/// How long a write waits on another before giving up."),
        code(LIB, LineKind.Added, new = 15, text = "const BUSY: Duration = Duration::from_secs(5);"),
    )

    private fun head(
        path: String,
        added: Int,
        removed: Int,
        comments: Int = 0,
        flags: List<ReviewFileFlag> = emptyList(),
        folded: Boolean = false,
        changedSinceLast: Boolean = false,
    ): ReviewRow = ReviewRow(
        key = "f/$path",
        of = ReviewRowOf.File(
            ReviewFileHead(
                path = path, status = FileStatus.Modified, added = added, removed = removed, binary = false,
                language = "rust", flags = flags, folded = folded || flags.isNotEmpty(),
                changedSinceLast = changedSinceLast, inChange = true, comments = comments,
                outline = emptyList(), oldPath = null,
            ),
        ),
    )

    private fun hunk(path: String, old: Int, new: Int, scope: List<String>): ReviewRow = ReviewRow(
        key = "h/$path/$old/$new",
        of = ReviewRowOf.Hunk(ReviewHunkHead(path = path, oldCommit = BASE, oldStart = old, newStart = new, scope = scope)),
    )

    private fun gap(path: String, hidden: Int?, up: ReviewContextAsk?, down: ReviewContextAsk?): ReviewRow = ReviewRow(
        key = "g/$path/${up?.start ?: down?.start ?: 0}",
        of = ReviewRowOf.Gap(ReviewGap(path = path, loading = false, down = down, hidden = hidden, up = up)),
    )

    private fun ask(path: String, start: Int, end: Int, definition: String?): ReviewContextAsk =
        ReviewContextAsk(path = path, commit = SNAPSHOT, start = start, end = end, definition = definition)

    private fun comment(
        id: List<Int>,
        path: String,
        text: String,
        state: ReviewCommentState,
        at: ReviewCommentAt,
        changedNear: Boolean = false,
        snippet: List<ReviewCodeLine> = emptyList(),
        scope: List<String> = emptyList(),
        folded: Boolean = false,
    ): ReviewRow = ReviewRow(
        key = "c/" + id.joinToString("") { "%02x".format(it) },
        of = ReviewRowOf.Comment(
            ReviewCommentRow(
                id = id, path = path, text = text, state = state, at = at, changedNear = changedNear,
                scope = scope, snippet = snippet, folded = folded,
            ),
        ),
    )

    private fun code(
        path: String,
        kind: LineKind,
        old: Int? = null,
        new: Int? = null,
        text: String,
        changed: List<String> = emptyList(),
        commented: Boolean = false,
    ): ReviewRow = ReviewRow(
        key = "l/$path/${old ?: 0}/${new ?: 0}",
        of = ReviewRowOf.Line(line(path, kind, old, new, text, changed, commented)),
    )

    private fun line(
        path: String,
        kind: LineKind,
        old: Int? = null,
        new: Int? = null,
        text: String,
        changed: List<String> = emptyList(),
        commented: Boolean = false,
    ): ReviewCodeLine = ReviewCodeLine(
        path = path, kind = kind, text = text, runs = runs(text, changed), commented = commented,
        extra = false, newLine = new, oldLine = old,
    )

    /** A line as the host would colour it: Rust's keywords, types, numbers and comments, and the words named changed. */
    private fun runs(text: String, changed: List<String>): List<ReviewRun> {
        if (text.trimStart().startsWith("//")) return listOf(ReviewRun(text = text, kind = TokenKind.Comment, changed = false))
        val keywords = setOf("pub", "fn", "let", "impl", "const", "use", "mut", "return")
        val out = mutableListOf<ReviewRun>()
        val word = StringBuilder()
        fun flush() {
            if (word.isEmpty()) return
            val w = word.toString()
            val kind = when {
                w in keywords -> TokenKind.Keyword
                w[0].isUpperCase() && w != w.uppercase() -> TokenKind.Type
                w[0].isDigit() -> TokenKind.Number
                else -> TokenKind.Plain
            }
            out += ReviewRun(text = w, kind = kind, changed = w in changed)
            word.clear()
        }
        for (character in text) {
            if (character.isLetterOrDigit() || character == '_') {
                word.append(character)
            } else {
                flush()
                out += ReviewRun(text = character.toString(), kind = TokenKind.Plain, changed = false)
            }
        }
        flush()
        return out
    }
}
