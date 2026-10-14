import AmuxCore
import XCTest

@testable import AmuxFeatures

final class ChatWordsTests: XCTestCase {
    private func row(_ kind: RowKind, attention: Bool = false, decision: Decision? = nil) -> Row {
        Row(
            id: "k", order: 1, atMs: 0, kind: kind, collapsed: false, attention: attention,
            decision: decision, parent: nil, run: nil)
    }

    func testDurationsAndCountsReadTheWayTheVocabularyWritesThem() {
        XCTAssertEqual(ChatWords.duration(102_000), "1m 42s")
        XCTAssertEqual(ChatWords.duration(8_000), "8s")
        XCTAssertEqual(ChatWords.duration(4_200), "4.2s")
        XCTAssertEqual(ChatWords.tokens(148_000), "148k")
        XCTAssertEqual(
            ChatWords.compaction(before: 148_000, after: 22_000, automatic: false),
            "Compacted · 148k → 22k")
        XCTAssertEqual(ChatWords.turnEnd(durationMs: 102_000, costUsd: nil, failed: false), "Worked 1m 42s")
        XCTAssertEqual(ChatWords.thinking(open: false, durationMs: 8_000), "Thought for 8s")
    }

    func testARunSaysWhatItDidAndThatItContinuesBelow() {
        let run = Run(
            first: "a", last: "b", steps: 6, live: false, openBelow: false,
            unresolvedFailure: false, counts: RunCounts(commands: 0, edits: 0, reads: 4, searches: 2, subagents: 0, other: 0),
            recent: nil)
        XCTAssertEqual(ChatWords.run(run), "4 reads · 2 searches")
        let open = Run(
            first: "a", last: "b", steps: 41, live: false, openBelow: true,
            unresolvedFailure: false, counts: RunCounts(commands: 0, edits: 0, reads: 40, searches: 0, subagents: 0, other: 1),
            recent: nil)
        XCTAssertEqual(ChatWords.run(open), "40+ reads · 1+ more")
    }

    func testACallsVerbSaysWhetherItAsksRunsWasRefusedOrRan() {
        let words = { (phase: CallPhase, row: Row) in
            ChatWords.verb(phase, row, wants: "Wants to run", doing: "Running", done: "Ran")
        }
        XCTAssertEqual(words(.asking, row(.stopped, attention: true)), "Wants to run")
        // Announced and asking nobody, it reads as under way.
        XCTAssertEqual(words(.pending, row(.stopped)), "Running")
        XCTAssertEqual(words(.running, row(.stopped)), "Running")
        XCTAssertEqual(words(.succeeded, row(.stopped)), "Ran")
        let denied = Decision(outcome: .denied, elsewhere: false, granted: nil, note: "Use cargo clean")
        XCTAssertEqual(words(.denied, row(.stopped, decision: denied)), "Denied")
        let allowed = Decision(outcome: .allowed, elsewhere: false, granted: .session, note: nil)
        XCTAssertEqual(words(.succeeded, row(.stopped, decision: allowed)), "Allowed")
        XCTAssertEqual(words(.running, row(.stopped, decision: allowed)), "Running")
        XCTAssertEqual(words(.failed, row(.stopped, decision: allowed)), "Ran")
    }

    func testAVerbThatNamesTheOutcomeLeavesItOutOfTheMeta() {
        let allowed = Decision(outcome: .allowed, elsewhere: false, granted: .session, note: nil)
        XCTAssertEqual(
            ChatWords.meta(["12s"], row(.stopped, decision: allowed), verbPhase: .succeeded),
            "12s · this session")
        let denied = Decision(outcome: .denied, elsewhere: false, granted: nil, note: "Use cargo clean")
        XCTAssertEqual(
            ChatWords.meta([], row(.stopped, decision: denied), verbPhase: .denied, note: false), "")
        XCTAssertNil(ChatWords.state(.denied, decided: true), "the decision says it")
        XCTAssertEqual(ChatWords.state(.asking, decided: false), "waiting")
    }

    func testTheRailRunsBetweenGridRowsAndBreaksAtProse() {
        let tool = row(.background(command: "npm run dev", running: true, durationMs: nil))
        let prose = row(.prose(text: [.text("Done.")], streaming: false, workingNote: false))
        XCTAssertEqual(RailJoin.of(tool, next: tool), RailJoin(continues: true))
        XCTAssertEqual(RailJoin.of(tool, next: prose), RailJoin(continues: false))
        XCTAssertEqual(RailJoin.of(tool, next: nil), RailJoin(continues: false))
        XCTAssertEqual(RailJoin.of(prose, next: tool), .none)
        var step = tool
        step.parent = "agent"
        XCTAssertEqual(RailJoin.of(step, next: tool), RailJoin(continues: true, nested: true))
    }

    func testADecisionIsMetaWithScopeNoteAndWhereItWasAnswered() {
        let allowed = Decision(outcome: .allowed, elsewhere: false, granted: .session, note: nil)
        XCTAssertEqual(ChatWords.meta(["12s"], row(.stopped, decision: allowed)), "12s · allowed · this session")
        let denied = Decision(outcome: .denied, elsewhere: false, granted: nil, note: "Use cargo clean\nplease")
        XCTAssertEqual(
            ChatWords.meta([], row(.stopped, decision: denied), verbPhase: .denied),
            "“Use cargo clean”")
        let elsewhere = Decision(outcome: .allowed, elsewhere: true, granted: nil, note: nil)
        XCTAssertEqual(ChatWords.decision(elsewhere), "allowed · in the terminal")
    }

    func testADecidedPermissionSaysWhatWasGranted() {
        let always = Decision(
            outcome: .allowed, elsewhere: false,
            granted: .claude(subjects: ["cargo test"], directories: [], mode: "", modeName: "", destination: .project),
            note: nil)
        XCTAssertEqual(ChatWords.decision(always), "allowed · always cargo test in this project")
        let switched = Decision(
            outcome: .allowed, elsewhere: false,
            granted: .claude(subjects: [], directories: [], mode: "acceptEdits", modeName: "Accept edits", destination: .session),
            note: nil)
        XCTAssertEqual(ChatWords.decision(switched), "allowed · switched to Accept edits")
        let prefix = Decision(outcome: .allowed, elsewhere: false, granted: .commandPrefix(words: ["curl", "-s"]), note: nil)
        XCTAssertEqual(ChatWords.decision(prefix), "allowed · commands starting with curl -s")
        let hosts = Decision(outcome: .allowed, elsewhere: false, granted: .networkHosts(hosts: ["example.com"]), note: nil)
        XCTAssertEqual(ChatWords.decision(hosts), "allowed · network access to example.com")
    }

    func testPermissionsAndModesAreWordedFromTheCatalogue() {
        func permission(
            _ value: String, _ name: String, custom: Bool = false, unlisted: Bool = false,
            neverAsks: Bool = false, settable: Bool = true
        ) -> PermissionChoice {
            PermissionChoice(
                value: value, displayName: name, custom: custom, current: false, unlisted: unlisted,
                normal: false, neverAsks: neverAsks, settable: settable)
        }
        XCTAssertEqual(ChatWords.permission(permission("acceptEdits", "Accept edits")), "Accept edits")
        XCTAssertEqual(ChatWords.permission(permission("", "", custom: true)), "Custom")
        XCTAssertEqual(ChatWords.permissionDetail(permission("auto", "Auto")), "")
        XCTAssertEqual(ChatWords.permissionDetail(permission("full", "Full", neverAsks: true)), "Acts without asking")
        XCTAssertEqual(ChatWords.permissionDetail(permission("odd", "odd", unlisted: true)), "Reported by the agent")
        XCTAssertEqual(ChatWords.permissionDetail(permission("auto", "Auto", settable: false)), "Can’t be picked from here")
        XCTAssertEqual(
            ChatWords.mode(ModeChoice(value: "plan", displayName: "Plan", current: true, unlisted: false, normal: false, settable: true)),
            "Plan")
        let switching = Choice(
            outcome: .allowAlways(subjects: [], directories: [], mode: "acceptEdits", modeName: "Accept edits", scope: .session),
            primary: false, takesNote: false)
        XCTAssertEqual(ChatWords.choice(switching), "Switch to Accept edits")
        XCTAssertEqual(ChatWords.scopeRow(switching).title, "Switch to Accept edits")
    }

    func testChoicesAreStatedAsOutcomes() {
        let always = Choice(
            outcome: .allowAlways(subjects: ["cargo test"], directories: [], mode: "", modeName: "", scope: .project),
            primary: false, takesNote: false)
        XCTAssertEqual(ChatWords.choice(always), "Always allow cargo test in this project")
        XCTAssertEqual(
            ChatWords.choice(Choice(outcome: .allowSimilar(prefix: ["curl", "-s"]), primary: false, takesNote: false)),
            "Allow commands starting with curl -s")
        XCTAssertEqual(
            ChatWords.choice(Choice(outcome: .deny(stops: false), primary: false, takesNote: true)), "Deny…")
        XCTAssertEqual(
            ChatWords.choice(Choice(outcome: .allowNetwork(hosts: ["localhost"]), primary: false, takesNote: false)),
            "Allow network access to localhost")
    }

    func testTheActivityLineNamesWhatIsHappeningAndForHowLong() {
        XCTAssertEqual(
            ChatWords.activity(.running(key: "k"), elapsedMs: 12_400, subject: "cargo test"),
            "Running cargo test · 12s")
        XCTAssertEqual(ChatWords.activity(.working, elapsedMs: 24_000, subject: nil), "Working · 24s")
        XCTAssertEqual(ChatWords.activity(.thinking, elapsedMs: 24_000, subject: nil), "Thinking")
        XCTAssertEqual(ChatWords.activity(.subagents(count: 3), elapsedMs: 5_000, subject: nil), "3 subagents working")
        XCTAssertEqual(ChatWords.activity(.compacting, elapsedMs: 5_000, subject: nil), "Compacting")
        XCTAssertEqual(
            ChatWords.activity(.retrying(attempt: 2, maxAttempts: 10, retryAtMs: nil), elapsedMs: 0, subject: nil),
            "Retrying · attempt 2 of 10")
    }

    /// The line draws its subject in mono and its time at the right end, so
    /// the parts come apart.
    func testTheActivityLineComesInWordsSubjectAndTime() {
        let parts = ChatWords.activityParts(.running(key: "k"), elapsedMs: 12_400, subject: "cargo test")
        XCTAssertEqual(parts.words, "Running")
        XCTAssertEqual(parts.subject, "cargo test")
        XCTAssertEqual(parts.time, "12s")
        XCTAssertNil(ChatWords.activityParts(.running(key: "k"), elapsedMs: 0, subject: "").subject)
    }

    /// The dock's head names the task in progress, else the next one to do.
    func testTheTaskChipNamesTheTaskInProgress() {
        let tasks = TasksView(done: 1, total: 3, current: "Splitting the lexer", entries: [
            TaskLine(subject: "Read the parser", mark: .done),
            TaskLine(subject: "Split the lexer", mark: .current),
            TaskLine(subject: "Run the tests", mark: .todo),
        ])
        XCTAssertEqual(ChatWords.headTask(tasks), "Split the lexer")
        var waiting = tasks
        waiting.entries[1].mark = .done
        XCTAssertEqual(ChatWords.headTask(waiting), "Run the tests")
    }

    func testTheEmptyFieldSaysWhoAMessageGoesToOrWhySendingWaits() {
        XCTAssertEqual(
            ChatWords.placeholder(.send, agent: "refactor-auth", host: "Studio", reach: .online(.direct), working: false),
            "Message refactor-auth")
        XCTAssertEqual(
            ChatWords.placeholder(.send, agent: "refactor-auth", host: "Studio", reach: .online(.direct), working: true),
            "Queue a message")
        XCTAssertEqual(
            ChatWords.placeholder(.holds(.detached), agent: "a", host: "Studio", reach: .away, working: false),
            "Studio is out of reach · sends when back")
        XCTAssertEqual(
            ChatWords.placeholder(.resume, agent: "a", host: "Studio", reach: .online(.direct), working: false),
            "a has exited · a message resumes it")
    }

    func testAReachedLimitIsNamedWithItsResetAboveAComposerThatStillSends() {
        let now = Date(milliseconds: 1_800_000_000_000)
        let nowMs: Int64 = 1_800_000_000_000
        let usage = UsageView(blocked: true, windows: [
            UsageWindowView(label: .fiveHour, usedPercent: 100, state: .blocked, resetsAtMs: nowMs + 7_200_000),
            UsageWindowView(label: .weekly(model: nil), usedPercent: 64, state: .ok, resetsAtMs: nowMs + 259_200_000),
        ], credits: nil)
        let reached = ChatWords.limitReached(usage, now: now)
        XCTAssertTrue(reached.hasPrefix("5-hour limit reached · resets "), reached)
        let overview = Overview(jobs: [], failedServers: [], changes: nil, tasks: nil, usageNearLimit: usage)
        XCTAssertEqual(ChatWords.strip(context: nil, overview: overview, now: now).first?.text, reached)
        XCTAssertEqual(ChatWords.strip(context: nil, overview: overview, now: now).first?.warn, true)
        var past = usage
        past.windows[0].resetsAtMs = nowMs - 1
        XCTAssertEqual(ChatWords.limitReached(past, now: now), "5-hour limit reached", "a reset already past is not promised")
        XCTAssertEqual(
            ChatWords.limitReached(UsageView(blocked: true, windows: [], credits: nil), now: now), "Usage limit reached")
    }

    func testSigningInIsAskedForOnTheAgentsHost() {
        XCTAssertEqual(ChatWords.needsSignIn(.claudeSdk), "Claude needs you to sign in")
        XCTAssertEqual(ChatWords.signInSteps(.claudeSdk, host: "desk"), "Run claude and sign in with /login on desk.")
        XCTAssertEqual(ChatWords.needsSignIn(.codex), "Codex needs you to sign in")
        XCTAssertEqual(ChatWords.signInSteps(.codex, host: "desk"), "Run codex login on desk.")
    }

    func testAQueuedPromptSaysWhetherItWasSteered() {
        let queued = QueuedRow(
            inputId: [1], text: [], steered: false, canWithdraw: true, canSendNow: true,
            fromAgent: nil)
        XCTAssertEqual(ChatWords.queued(queued), "queued")
        var steered = queued
        steered.steered = true
        XCTAssertEqual(ChatWords.queued(steered), "steered")
        XCTAssertEqual(ChatWords.underway(.mayNotHaveArrived, host: "Studio"), "may not have arrived")
        XCTAssertEqual(ChatWords.underway(.sending(waiting: true), host: "Studio"), "waiting for Studio…")
    }

    func testASecretAnswerIsNeverShown() {
        XCTAssertEqual(ChatWords.answer(AnswerView(picked: [], hidden: true, skipped: false, note: nil, other: nil)), "answered (hidden)")
        XCTAssertEqual(
            ChatWords.answer(AnswerView(picked: ["macOS"], hidden: false, skipped: false, note: nil, other: "BSD")), "macOS, “BSD”")
    }

    func testAQuestionLeftUnansweredReadsSkipped() {
        let left = AnswerView(picked: [], hidden: false, skipped: true, note: "later", other: nil)
        XCTAssertEqual(ChatWords.answer(left), "Skipped")
    }

    private func field(
        _ name: String, _ kind: FormFieldKind, required: Bool = false, initial: FormValue = .text("")
    ) -> FormField {
        FormField(name: name, title: name, description: "", required: required, kind: kind, initial: initial)
    }

    private var asked: [FormField] {
        [
            field("repo", .text(maxLength: nil, minLength: nil), required: true),
            field("count", .number(integer: true, maximum: nil, minimum: 1)),
            field("assign", .toggle, initial: .toggle(true)),
        ]
    }

    /// The phone holds values and words problems; whether the form can go is
    /// the shared view's check, reached through the bridge.
    func testTheSharedCheckGatesSubmitAndAChangedFieldSaysWhy() throws {
        let entries = FormEntry.entries(asked)
        XCTAssertEqual(entries.map(\.value), [.text(""), .text(""), .toggle(true)])
        let untouched = try XCTUnwrap(Bridge.formProblems(asked, entries.map(\.value)))
        XCTAssertEqual(untouched, [FormProblem(field: 0, problem: .required)])
        XCTAssertNil(FormEntry.shown(untouched, at: 0, of: entries[0]), "a required field says so on its label")
        var filled = entries
        filled[0].value = .text("jlw/amux")
        filled[1].value = .text("0")
        let low = try XCTUnwrap(Bridge.formProblems(asked, filled.map(\.value)))
        XCTAssertEqual(FormEntry.shown(low, at: 1, of: filled[1]), .belowMinimum(minimum: 1))
        filled[1].value = .text("3")
        XCTAssertEqual(Bridge.formProblems(asked, filled.map(\.value)), [])
    }

    func testAFormDrawnAgainRestoresTheValuesItKept() {
        var typed = FormEntry.entries(asked)
        typed[0].value = .text("jlw/amux")
        typed[2].value = .toggle(false)
        let kept = typed.map(\.value)
        XCTAssertEqual(FormEntry.entries(asked, kept: kept), typed, "drawn again, it holds what was typed")
        XCTAssertEqual(FormEntry.entries(asked, kept: nil), FormEntry.entries(asked), "a new form starts as written")
        XCTAssertEqual(
            FormEntry.entries(asked, kept: [.text("jlw/amux")]).map(\.value), [.text(""), .text(""), .toggle(true)],
            "values kept for another shape of form are not this one's")
    }

    func testEveryFieldProblemHasWords() {
        let problems: [FieldProblem] = [
            .required, .notANumber, .notWholeNumber, .belowMinimum(minimum: 1), .aboveMaximum(maximum: 2.5),
            .tooShort(minLength: 1), .tooLong(maxLength: 80), .tooFew(minItems: 2), .tooMany(maxItems: 3),
        ]
        for problem in problems {
            XCTAssertFalse(ChatWords.fieldProblem(problem).isEmpty, "\(problem)")
        }
        XCTAssertEqual(ChatWords.fieldProblem(.tooShort(minLength: 1)), "At least 1 character")
        XCTAssertEqual(ChatWords.fieldProblem(.aboveMaximum(maximum: 2.5)), "At most 2.5")
    }

    func testEveryNameProblemHasWords() {
        for problem in AgentNameProblem.allCases {
            XCTAssertFalse(ChatWords.nameProblem(problem).isEmpty, "\(problem)")
        }
    }

    func testTheReviewSaysHowMuchChangedAndWhatAttachingCarries() {
        XCTAssertEqual(ChatWords.changes(files: 1, added: 18, removed: 2), "1 file, 18 added, 2 removed")
        XCTAssertEqual(ChatWords.changes(files: 4, added: 0, removed: 28), "4 files, 0 added, 28 removed")
        XCTAssertEqual(ChatWords.attachReview(1), "Attach Review · 1 comment")
        XCTAssertEqual(ChatWords.attachReview(3), "Attach Review · 3 comments")
        XCTAssertEqual(ChatWords.commentOn(lines: 1), "Comment on 1 line")
        XCTAssertEqual(ChatWords.commentOn(lines: 5), "Comment on 5 lines")
        XCTAssertEqual(ChatWords.chip(.review(comments: 2, patch: nil)), "Review · 2 comments")
        let added = DiffLine(kind: .added, text: "    Refused(String),", comments: [], newLine: 11, oldLine: nil)
        XCTAssertEqual(ChatWords.spoken(added), "Added line 11, Refused(String),")
        let removed = DiffLine(kind: .removed, text: "    Busy,", comments: [], newLine: nil, oldLine: 13)
        XCTAssertEqual(ChatWords.spoken(removed), "Removed line 13, Busy,")
    }

    /// The chip says the model by the one name the view gives it, its
    /// effort, and only a permission or mode other than the normal one.
    func testTheModelChipSaysTheSummaryTheViewHandsOver() {
        let permission = PermissionChoice(
            value: "acceptEdits", displayName: "Accept edits", custom: false, current: true,
            unlisted: false, normal: false, neverAsks: false, settable: true)
        let plan = ModeChoice(
            value: "plan", displayName: "Plan", current: true, unlisted: false, normal: false,
            settable: true)
        let summary = ControlsSummary(effort: "high", mode: nil, model: "Sonnet 5", permission: nil)
        let chip = ChatWords.chip(summary)
        XCTAssertEqual(chip?.model, "Sonnet 5")
        XCTAssertEqual(chip?.detail, "high")
        XCTAssertEqual(chip?.unusual, [])
        var unusual = summary
        unusual.permission = permission
        unusual.mode = plan
        XCTAssertEqual(ChatWords.chip(unusual)?.unusual, ["Accept edits", "Plan"])
        var custom = permission
        custom.custom = true
        custom.value = ""
        custom.displayName = ""
        XCTAssertEqual(ChatWords.permission(custom), "Custom")
        XCTAssertNil(ChatWords.chip(ControlsSummary(effort: nil, mode: nil, model: nil, permission: nil)))
    }

    /// Where only the agent's own commands change the model and effort,
    /// the phone says so, naming the commands the view hands over.
    func testTypedChangesNameTheAgentsCommands() {
        let typed = Changeability(
            model: .byTyping("/model"), effort: .byTyping("/effort"), permission: .cycle, mode: .notOffered)
        XCTAssertEqual(
            ChatWords.byTyping(typed),
            "To change the model or effort, type /model <name> or /effort <level> in the composer.")
        let picked = Changeability(model: .pick, effort: .pick, permission: .pick, mode: .notOffered)
        XCTAssertNil(ChatWords.byTyping(picked))
        XCTAssertEqual(ChatWords.lines(1), "1 line")
        XCTAssertEqual(ChatWords.lines(3), "3 lines")
    }

    /// An edit's ask says whether it creates the file (docs/CLIENT.md: the
    /// ask's `created` is there "so it reads 'Wants to create'"); the
    /// terminal's card and the phone's own row both say it, the card does not.
    func testAnEditCardThatCreatesItsFileSaysSo() {
        let card = { (created: Bool?) in
            AskCard(
                kind: .codex, key: "ask:1", itemKey: "", position: 1, count: 1,
                body: .edit(
                    path: "/work/notes.txt", files: 1, added: 1, removed: 0, lines: [], reason: "",
                    created: created, line: 1),
                choices: [], questionNote: false, questionSkip: false, questionReply: false,
                stopsTurn: false, state: .open)
        }
        XCTAssertEqual(ChatWords.headline(card(false)), "Wants to edit a file")
        XCTAssertTrue(
            ChatWords.headline(card(true)).contains("create"),
            "a new file's card reads \(ChatWords.headline(card(true)))")
    }
}
