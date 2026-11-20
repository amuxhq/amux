import AmuxCore
import AmuxDesign
import AmuxFeatures
import AmuxWords
import SwiftUI
import UIKit

/// One deterministic production-component state shared by previews and
/// app-hosted snapshot tests.
///
/// The catalogue lives in the app's Debug sources so its invented content is
/// never linked into a build somebody installs. Its views are the production
/// views themselves, drawn from typed values and a scripted chat.
@MainActor
struct ComponentExample: Identifiable {
    enum Family: String, CaseIterable {
        case rows = "Rows"
        case asks = "Asks"
        case composer = "Composer"
        case chat = "Chat"
        case review = "Review"
        case fleet = "Fleet"
        case hosts = "Hosts"
        case pairing = "Pairing"
        case settings = "Settings"
        case report = "Report"
    }

    let id: String
    let family: Family
    let canvas: CGSize
    let dynamicTypeSize: DynamicTypeSize
    let readinessIdentifier: String?
    let readinessValue: String?
    let build: @MainActor () -> AnyView

    init(
        id: String,
        family: Family,
        canvas: CGSize = CGSize(width: 390, height: 120),
        dynamicTypeSize: DynamicTypeSize = .large,
        readinessIdentifier: String? = nil,
        readinessValue: String? = nil,
        @ViewBuilder build: @escaping @MainActor () -> some View
    ) {
        self.id = id
        self.family = family
        self.canvas = canvas
        self.dynamicTypeSize = dynamicTypeSize
        self.readinessIdentifier = readinessIdentifier
        self.readinessValue = readinessValue
        self.build = { AnyView(build()) }
    }
}

/// The single inventory consumed by previews and snapshot tests: every chat
/// row kind, every ask card, the composer's states and the chat's own.
@MainActor
enum ComponentCatalog {
    static let examples: [ComponentExample] = rows + asks + composer + chat + overview + review + newAgent + fleet + hosts + pairing + settings + report

    static func example(id: String) -> ComponentExample? {
        examples.first { $0.id == id }
    }

    // MARK: - Rows

    private static func row(
        _ id: String, height: CGFloat = 110, expanded: Bool = false,
        readiness: Bool = false, size: DynamicTypeSize = .large, _ rows: Row...
    ) -> ComponentExample {
        ComponentExample(
            id: "row.\(id)", family: .rows, canvas: CGSize(width: 390, height: height),
            dynamicTypeSize: size,
            readinessIdentifier: readiness ? "chat.prose.render" : nil,
            readinessValue: readiness ? "rendered" : nil
        ) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                    ChatRowView(
                        row: row, expanded: expanded,
                        rail: RailJoin.of(row, next: index + 1 < rows.count ? rows[index + 1] : nil))
                }
            }
            .environment(\.attachmentAccess, CatalogFixtures.access)
        }
    }

    /// One row of a call that opens an agent, with what the fleet says of
    /// the agent or of its group.
    private static func opening(
        _ id: String, height: CGFloat = 110, _ row: Row, opened: OpenedAgent? = nil, group: GroupState? = nil
    ) -> ComponentExample {
        ComponentExample(id: "row.\(id)", family: .rows, canvas: CGSize(width: 390, height: height)) {
            ChatRowView(row: row, expanded: false, opened: opened, groupState: group)
                .environment(\.attachmentAccess, CatalogFixtures.access)
        }
    }

    private static let rows: [ComponentExample] = {
        typealias F = CatalogFixtures
        func r(
            _ id: String, _ order: UInt64, _ kind: RowKind, attention: Bool = false,
            decision: Decision? = nil, run: Run? = nil, parent: String? = nil
        ) -> Row {
            ScriptedChat.row(id, order, kind, attention: attention, decision: decision, run: run, parent: parent)
        }
        return [
            row("prompt", r("p1", 1, .prompt(text: [.text("Collapse the pairing errors onto one string.")], steered: false))),
            row("prompt-attachments", height: 390, r("p2", 2, .prompt(
                text: [.text("Here is the screen and the log."), .attachment(.image(F.photo)),
                       .attachment(.file(F.log)), .attachment(.text(name: "Pasted text", lines: 240, text: ""))],
                steered: false))),
            row("prompt-sent-review", height: 140, r("p5", 5, .prompt(
                text: [.text("These two, then run the suite."),
                       .attachment(.sentReview(comments: 2, review: ReviewStory.sent, text: ""))],
                steered: false))),
            row("prompt-steered", r("p3", 3, .prompt(text: [.text("Use the new error table instead.")], steered: true))),
            row("prompt-quote", height: 150, r("p4", 4, .prompt(
                text: [.text("> The retry only fires when the socket closes cleanly.\n\nOh **really**?")],
                steered: false))),
            row("prose", height: 330, readiness: true, r("s1", 4, .prose(
                text: [.text(F.markdown)], streaming: false, workingNote: false))),
            row("prose-code", height: 520, readiness: true, r("s3", 4, .prose(
                text: [.text(F.codeMarkdown)], streaming: false, workingNote: false))),
            row("prose-code-streaming", height: 220, readiness: true, r("s4", 4, .prose(
                text: [.text(F.streamingCode)], streaming: true, workingNote: false))),
            row("prose-working-note", height: 120, readiness: true, r("s2", 5, .prose(
                text: [.text("Checking which crates read the table first.")], streaming: true,
                workingNote: true))),
            row("thinking", r("t1", 6, .thinking(text: "", open: false, durationMs: 8_000))),
            row("thinking-open", height: 150, expanded: true, r("t2", 7, .thinking(
                text: "The three arms differ only in wording, so one arm with one string is enough.",
                open: false, durationMs: 4_200))),
            row("tool-call", r("c1", 8, .toolCall(
                server: "github", tool: "create_issue", fact: "#482", state: .succeeded, result: "Created #482"))),
            row("tool-call-asking", r("c2", 9, .toolCall(
                server: "github", tool: "merge_pull_request", fact: "", state: .asking, result: ""),
                attention: true)),
            row("file-change", height: 170, r("f1", 10, .fileChange(files: [
                FileRow(path: "crates/amux-ui/src/pairing.rs", change: .edited, added: 9, removed: 14, line: nil),
                FileRow(path: "crates/amux-ui/spec/pairing_copy.rs", change: .created(lines: 38), added: 38, removed: 0, line: nil),
                FileRow(path: "crates/amux-ui/src/old.rs", change: .moved(to: "crates/amux-ui/src/codes.rs"), added: 0, removed: 0, line: nil),
                FileRow(path: "crates/amux-ui/src/unused.rs", change: .deleted, added: 0, removed: 12, line: nil),
            ], state: .succeeded))),
            // Each file's verb, path and counts stacked, the name kept whole.
            row("file-change-large-text", height: 1100, size: .accessibility3, r("f2", 10, .fileChange(files: [
                FileRow(path: "crates/amux-ui/src/pairing.rs", change: .edited, added: 9, removed: 14, line: nil),
                FileRow(path: "crates/amux-ui/spec/pairing_copy.txt", change: .created(lines: 1), added: 1, removed: 0, line: nil),
                FileRow(path: "crates/amux-ui/src/unused.rs", change: .deleted, added: 0, removed: 12, line: nil),
            ], state: .succeeded))),
            row("command", height: 150, r("x1", 11, .command(
                command: "cargo test -p amux-ui", description: "", state: .succeeded,
                outputHead: ["running 42 tests", "test pairing::copy ok", "test result: ok. 42 passed"],
                moreLines: 118, outputTrimmed: false, outputTail: [], durationMs: 4_200, exitCode: 0))),
            row("command-failed", height: 150, r("x2", 12, .command(
                command: "cargo check -p amux-ui", description: "", state: .failed,
                outputHead: ["error[E0308]: mismatched types"], moreLines: 214, outputTrimmed: false, outputTail: [], durationMs: 4_200,
                exitCode: 101))),
            row("command-denied", r("x3", 13, .command(
                command: "rm -rf target", description: "", state: .denied, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [], durationMs: nil,
                exitCode: nil),
                decision: Decision(outcome: .denied, elsewhere: false, granted: nil, note: "Use cargo clean instead"))),
            row("command-allowed", r("x4", 14, .command(
                command: "cargo test -p amux-ui", description: "", state: .succeeded, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [],
                durationMs: 12_000, exitCode: 0),
                decision: Decision(outcome: .allowed, elsewhere: false, granted: .session, note: nil))),
            row("command-allowed-always", r("x8", 14, .command(
                command: "cargo test -p amux-ui", description: "", state: .succeeded, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [],
                durationMs: 12_000, exitCode: 0),
                decision: Decision(outcome: .allowed, elsewhere: false, granted: .claude(
                    subjects: ["cargo test"], directories: [], mode: "", modeName: "", destination: .project), note: nil))),
            row("command-allowed-prefix", r("x9", 14, .command(
                command: "curl -s localhost:8080/health", description: "", state: .succeeded, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [],
                durationMs: 300, exitCode: 0),
                decision: Decision(outcome: .allowed, elsewhere: false, granted: .commandPrefix(words: ["curl", "-s"]), note: nil))),
            row("command-elsewhere", r("x5", 15, .toolCall(
                server: "github", tool: "create_issue", fact: "", state: .succeeded, result: ""),
                decision: Decision(outcome: .allowed, elsewhere: true, granted: nil, note: nil))),
            row("command-asking", r("x6", 16, .command(
                command: "cargo test -p amux-ui", description: "", state: .asking, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [],
                durationMs: nil, exitCode: nil), attention: true)),
            row("explore", r("e1", 17, .explore(verb: .read, subject: "crates/wire/src/codes.rs", state: .succeeded, result: ""))),
            row("run", r("e2", 18, .explore(verb: .search, subject: "INVALID_PIN", state: .succeeded, result: ""),
                run: Run(first: "e0", last: "e2", steps: 6, live: false, openBelow: false, liveShown: false, unresolvedFailure: false, open: false, counts: RunCounts(commands: 0, edits: 0, reads: 4, searches: 2, lists: 0, fetches: 0, subagents: 0, other: 0), hidden: nil, recent: nil))),
            row("run-open-below", r("e3", 19, .explore(verb: .read, subject: "src/lib.rs", state: .succeeded, result: ""),
                run: Run(first: "e0", last: "e3", steps: 40, live: false, openBelow: true, liveShown: false, unresolvedFailure: false, open: false, counts: RunCounts(commands: 0, edits: 0, reads: 40, searches: 0, lists: 0, fetches: 0, subagents: 0, other: 0), hidden: nil, recent: nil))),
            // A call from history, which names no agent: what the call came
            // to is all there is to say.
            row("subagent-running", r("a1", 20, .subagent(
                description: "Find every INVALID_PIN reader", continued: false, state: .running,
                answer: "", durationMs: nil))),
            row("subagent-done", height: 150, r("a2", 21, .subagent(
                description: "Find every INVALID_PIN reader", continued: false, state: .succeeded,
                answer: "Three readers: pairing.rs, codes.rs and the pairing spec. All read the same string.",
                durationMs: 51_000))),
            // A call naming its subagent: how it stands is the fleet's, and
            // a tap opens its chat.
            opening("subagent-opens-running", F.call("a6", 41, "Survey the relay’s retry path", agent: 21),
                    opened: F.opened(21, "Survey the relay’s retry path", .working)),
            opening("subagent-opens-waiting", F.call("a7", 42, "Check the Windows build", agent: 22),
                    opened: F.opened(22, "Check the Windows build", .needsYou)),
            opening("subagent-opens-done", height: 150, F.call(
                "a8", 43, "Find every INVALID_PIN reader", agent: 23,
                answer: "Three readers: pairing.rs, codes.rs and the pairing spec. All read the same string."),
                    opened: F.opened(23, "Find every INVALID_PIN reader", .exited(cause: .finished))),
            opening("subagent-group", F.delegating.first { $0.id == "d04" }!, group: F.delegatedState),
            row("background", r("b1", 22, .background(command: "npm run dev", description: "", running: true, durationMs: nil))),
            row("image", height: 230, r("i1", 23, .image(path: "host-list-mock.png", generated: true, image: F.photo))),
            row("slash-output", height: 150, r("o1", 24, .slashOutput(
                command: "/cost", args: "", output: "Total cost: $0.42\nTotal duration: 18m 3s"))),
            row("turn-end", r("z1", 25, .turnEnd(failed: false, durationMs: 102_000))),
            row("stopped", r("z2", 26, .stopped)),
            row("compaction", r("z3", 27, .compaction(automatic: true, idle: false, tokensAfter: 22_000, tokensBefore: 148_000))),
            row("error", height: 140, expanded: false, r("z4", 28, .error(
                errorKind: "Provider overloaded", message: "The API returned 529 ten times in a row.",
                attempts: 10, gaveUp: true))),
            row("model-switch", r("z5", 29, .modelSwitch(to: "sonnet 4.6", reason: "Opus declined this request"))),
            row("boundary", r("z6", 30, .boundary(kind: .cleared, cause: ""))),
            row("agent-message", height: 130, r("m1", 31, .agentMessage(
                from: "worker-2", kind: .finished, text: [.text("3 specs updated, all green.")], to: "",
                sent: .unspecified, rejection: ""))),
            row("agent-message-rejected", height: 130, r("m2", 32, .agentMessage(
                from: "", kind: .message, text: [.text("Please rebase onto main first.")], to: "worker-3",
                sent: .rejected, rejection: "no agent by that name"))),
            row("auto-review", r("v1", 33, .autoReview(
                status: .approved, statusWord: "approved", risk: "low", rationale: "Local only.",
                subject: "x9"))),
            row("unrecognized", r("u1", 34, .unrecognized(what: "rate_limit_event", summary: "an event this build cannot read"))),
            row("subagent-steps", height: 200, expanded: true,
                r("a3", 35, .subagent(
                    description: "Explore the pairing code", continued: false, state: .running,
                    answer: "", durationMs: nil)),
                r("a4", 36, .explore(verb: .search, subject: "INVALID_PIN", state: .succeeded, result: ""), parent: "a3"),
                r("a5", 37, .explore(verb: .read, subject: "wire/src/codes.rs", state: .succeeded, result: ""), parent: "a3")),
            row("ask-question", height: 130, r("q1", 38, .ask(.questions(
                questions: [F.redactionQuestion], answers: [AnswerView(picked: ["amux-core"], hidden: false, skipped: false, note: nil, other: nil)],
                skipped: 0, resolution: .answered, reply: nil)))),
            row("ask-questions", height: 280, r("q2", 39, .ask(.questions(
                questions: F.threeQuestions,
                answers: [AnswerView(picked: ["macOS", "Linux"], hidden: false, skipped: false, note: nil, other: nil),
                          AnswerView(picked: ["All at once"], hidden: false, skipped: false, note: nil, other: nil),
                          AnswerView(
                            picked: [], hidden: false, skipped: false, note: "keep the old strings for one release",
                            other: "Only failures, with the host id")],
                skipped: 0, resolution: .answered, reply: nil)))),
            row("ask-secret", height: 130, r("q3", 40, .ask(.questions(
                questions: [F.secretQuestion], answers: [AnswerView(picked: [], hidden: true, skipped: false, note: nil, other: nil)],
                skipped: 0, resolution: .answered, reply: nil)))),
            row("ask-plan-approved", r("l1", 41, .ask(.plan(body: F.planBody, verdict: .approved, mode: "", modeName: "", writing: false, note: nil, title: F.planTitle)))),
            row("ask-plan-sent-back", r("l2", 42, .ask(.plan(body: F.planBody, verdict: .sentBack, mode: "", modeName: "", writing: false, note: "Don’t touch the wire codes yet", title: F.planTitle)))),
            row("ask-plan-accepting-edits", r("l4", 42, .ask(.plan(body: F.planBody, verdict: .approvedSwitching, mode: "acceptEdits", modeName: "Accept edits", writing: false, note: nil, title: F.planTitle)))),
            row("ask-questions-replied", height: 280, r("q4", 39, .ask(.questions(
                questions: F.threeQuestions,
                answers: [AnswerView(picked: ["macOS"], hidden: false, skipped: false, note: "Linux comes later", other: nil),
                          AnswerView(picked: [], hidden: false, skipped: true, note: nil, other: nil),
                          AnswerView(picked: [], hidden: false, skipped: true, note: nil, other: nil)],
                skipped: 2, resolution: .replied, reply: "Hold off on the rollout; ask me again after the release.")))),
            row("ask-plan-open", height: 330, expanded: true, readiness: true,
                r("l3", 43, .ask(.plan(body: F.planBody, verdict: .open, mode: "", modeName: "", writing: false, note: nil, title: F.planTitle)))),
            row("ask-form", r("g1", 44, .ask(.form(server: "github", message: "Create the issue", fields: ["repository", "labels", "assignee"], resolution: .answered)))),
            row("ask-link", r("g2", 45, .ask(.link(server: "linear", message: "Sign in to Linear", url: "https://linear.app/login", resolution: .answered)))),
            row("ask-grant", r("g3", 46, .ask(.grant(
                reason: "To run the integration tests against a local server.", read: [],
                write: ["~/src/amux/target"], network: false, hosts: [], resolution: .answered,
                granted: Granted(read: [], write: ["~/src/amux/target"], network: false, forSession: false))))),
            row("ask-unanswerable", r("g4", 47, .ask(.unanswerable(reason: "A tool server’s dialog", resolution: .cancelled)))),
        ]
    }()

    // MARK: - Asks

    private static func ask(
        _ id: String, height: CGFloat = 420, preset: AskPreset? = nil, size: DynamicTypeSize = .large,
        _ card: AskCard
    ) -> ComponentExample {
        ComponentExample(
            id: "ask.\(id)", family: .asks, canvas: CGSize(width: 390, height: height),
            dynamicTypeSize: size,
            readinessIdentifier: nil, readinessValue: nil
        ) {
            AskCardView(card: card, preset: preset) { _ in }
        }
    }

    private static let asks: [ComponentExample] = {
        typealias F = CatalogFixtures
        return [
            ask("permission-command", height: 480, F.card(.claudeSdk, .command(
                command: "cargo test -p amux-ui", cwd: "~/src/amux", reason: "",
                description: "Run the spec suite"), F.sdkPermissionChoices)),
            ask("permission-deny-note", height: 380, preset: .noting(3), F.card(.claudeSdk, .command(
                command: "rm -rf target", cwd: "~/src/amux", reason: "", description: "Clean the build"),
                F.sdkPermissionChoices)),
            ask("permission-edit", height: 560, F.card(.claudeSdk, .edit(
                path: "crates/amux-ui/src/pairing.rs", files: 1, added: 1, removed: 2, patch: F.diff,
                reason: "", created: false, line: 13), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .allowAlways(subjects: [], directories: [], mode: "acceptEdits", modeName: "Accept edits", scope: .session), primary: false, takesNote: false),
                    Choice(outcome: .deny, primary: false, takesNote: true),
                ])),
            ask("permission-create", height: 420, F.card(.claudeSdk, .edit(
                path: "crates/amux-ui/src/pairing/refusals/every_reason_a_pairing_is_refused.rs",
                files: 1, added: 2, removed: 0, patch: F.created,
                reason: "", created: true, line: 1), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .deny, primary: false, takesNote: true),
                ])),
            ask("permission-tool", height: 360, F.card(.claudeSdk, .tool(
                server: "github", tool: "create_issue",
                arguments: "{\n  \"repo\": \"jlw/amux\",\n  \"title\": \"Pairing copy\"\n}"), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .deny, primary: false, takesNote: true),
                ])),
            ask("permission-terminal", height: 380, F.card(.claudePty, .command(
                command: "cargo test -p amux-ui", cwd: "", reason: "", description: ""), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .allowAlways(subjects: ["cargo test"], directories: [], mode: "", modeName: "", scope: .project), primary: false, takesNote: false),
                    Choice(outcome: .deny, primary: false, takesNote: false),
                ])),
            ask("codex-command", height: 560, F.card(.codex, .command(
                command: "curl -s localhost:8080/health", cwd: "~/src/amux", reason: "Check the dev server",
                description: ""), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .allowForSession, primary: false, takesNote: false),
                    Choice(outcome: .allowSimilar(prefix: ["curl", "-s"]), primary: false, takesNote: false),
                    Choice(outcome: .allowNetwork(hosts: ["localhost"]), primary: false, takesNote: false),
                    Choice(outcome: .deny, primary: false, takesNote: false),
                    Choice(outcome: .denyAndStop, primary: false, takesNote: false),
                ], count: 2)),
            ask("question-single", height: 500, F.card(.claudeSdk, .question([F.redactionQuestion]), [])),
            ask("question-multi", height: 520, preset: .highlighted(0), F.card(.claudeSdk, .question([F.platformQuestion]), [])),
            ask("questions-step", height: 640, F.card(.codex, .question(F.threeQuestions), [])),
            ask("questions-review", height: 520, preset: .reviewing([.picked([0, 1]), .picked([1]), .typed("Only failures, with the host id")]),
                F.card(.codex, .question(F.threeQuestions), [])),
            ask("questions-skipped", height: 560, preset: .reviewing([.picked([0, 1]), .picked([]), .typed("Only failures, with the host id")]),
                F.card(.codex, .question(F.threeQuestions), [])),
            ask("question-note", height: 620, preset: .questionNote("Keep the old strings for one release"),
                F.card(.claudeSdk, .question([F.redactionQuestion]), [])),
            ask("question-reply", height: 420, preset: .replying("Hold off; I want to read the diff first."),
                F.card(.codex, .question(F.threeQuestions), [])),
            ask("question-terminal", height: 500, F.card(.claudePty, .question([F.redactionQuestion]), [])),
            ask("question-previews", height: 580, preset: .highlighted(0), F.card(.claudeSdk, .question([F.previewQuestion]), [])),
            ask("question-other", height: 520, preset: .other("Put it in amux-wire next to the codes"),
                F.card(.claudeSdk, .question([F.redactionQuestion]), [])),
            ask("question-secret", height: 420, preset: .other("hunter2"), F.card(.codex, .question([F.secretQuestion]), [])),
            ask("question-code", height: 460, F.card(.codex, .question([F.codeQuestion]), [])),
            ask("plan", height: 260, preset: .autoAccept, F.card(.claudeSdk, .plan(body: F.planBody, title: F.planTitle), [
                Choice(outcome: .approvePlan(mode: "auto", modeName: "Auto"), primary: true, takesNote: false),
                Choice(outcome: .approvePlan(mode: "", modeName: ""), primary: false, takesNote: false),
                Choice(outcome: .sendBack, primary: false, takesNote: true),
            ])),
            ask("plan-codex", height: 220, F.card(.codex, .plan(body: F.planBody, title: F.planTitle), F.codexPlanChoices)),
            ask("form", height: 520, F.card(.claudeSdk, .form(
                server: "github", message: "Create the issue in which repository?", fields: F.formFields), [
                    Choice(outcome: .submit, primary: true, takesNote: false),
                    Choice(outcome: .decline, primary: false, takesNote: false),
                ])),
            ask("link", height: 380, F.card(.codex, .link(
                server: "linear", message: "Sign in to Linear to continue.", url: "https://linear.app/login",
                localPage: false), [
                    Choice(outcome: .openLink, primary: true, takesNote: false),
                    Choice(outcome: .decline, primary: false, takesNote: false),
                ])),
            ask("access", height: 420, F.card(.codex, .access(
                reason: "To run the integration tests against a local server.", read: [],
                write: ["~/src/amux/target"], network: true, hosts: []), [
                    Choice(outcome: .grantForTurn, primary: true, takesNote: false),
                    Choice(outcome: .grantForSession, primary: false, takesNote: false),
                    Choice(outcome: .decline, primary: false, takesNote: false),
                ])),
            ask("sending", height: 120, F.card(.claudeSdk, .command(
                command: "cargo test", cwd: "", reason: "", description: ""), [], state: .sending)),
            ask("rejected", height: 460, F.card(.claudeSdk, .command(
                command: "cargo test -p amux-ui", cwd: "~/src/amux", reason: "", description: ""),
                F.sdkPermissionChoices, state: .rejected(Refusal(reason: .closedAsk, detail: nil)))),
            ask("not-confirmed", height: 260, F.card(.claudeSdk, .command(
                command: "cargo test -p amux-ui", cwd: "", reason: "", description: ""),
                F.sdkPermissionChoices, state: .notConfirmed)),
            ask("dismissed", height: 180, F.card(.claudeSdk, .command(
                command: "cargo test -p amux-ui", cwd: "", reason: "", description: ""),
                F.sdkPermissionChoices, state: .dismissed)),
            ask("unanswerable", height: 300, F.card(.claudePty, .unanswerable(reason: ""), [])),
            // At accessibility text sizes: nothing overprints, every button
            // is whole, and a choice that does not fit beside another goes
            // under it.
            ask("permission-command-large-text", height: 1500, size: .accessibility3, F.card(.claudeSdk, .command(
                command: "cargo test -p amux-ui", cwd: "~/src/amux", reason: "",
                description: "Run the spec suite"), F.sdkPermissionChoices)),
            ask("codex-command-large-text", height: 2600, size: .accessibility3, F.card(.codex, .command(
                command: "curl -s localhost:8080/health", cwd: "~/src/amux", reason: "Check the dev server",
                description: ""), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .allowForSession, primary: false, takesNote: false),
                    Choice(outcome: .allowSimilar(prefix: ["curl", "-s"]), primary: false, takesNote: false),
                    Choice(outcome: .deny, primary: false, takesNote: false),
                ])),
            ask("question-large-text", height: 2600, size: .accessibility5,
                F.card(.claudeSdk, .question([F.redactionQuestion]), [])),
            ask("plan-large-text", height: 900, size: .accessibility3, F.card(.claudeSdk, .plan(body: F.planBody, title: F.planTitle), [
                Choice(outcome: .approvePlan(mode: "auto", modeName: "Auto"), primary: true, takesNote: false),
                Choice(outcome: .approvePlan(mode: "", modeName: ""), primary: false, takesNote: false),
                Choice(outcome: .sendBack, primary: false, takesNote: true),
            ])),
            ask("not-confirmed-large-text", height: 900, size: .accessibility3, F.card(.claudeSdk, .command(
                command: "cargo test -p amux-ui", cwd: "", reason: "", description: ""),
                F.sdkPermissionChoices, state: .notConfirmed)),
        ]
    }()

    // MARK: - Composer

    private static func composer(
        _ id: String, height: CGFloat = 260, rows: [Row] = [], frame: ChatFrame,
        strip: ScriptedChat.Surroundings = ScriptedChat.strip(model: "opus 4.6", effort: "high"), draft: String = "",
        settings: SettingsView? = nil, showing: ChatOverlay? = nil, children: [FleetCard] = [],
        dockExpanded: Bool = false, refusedWords: [[UInt8]: String] = [:],
        subject: ChatSubject = CatalogFixtures.subject, size: DynamicTypeSize = .large,
        setUp: @escaping @MainActor (ChatModel) -> Void = { _ in }
    ) -> ComponentExample {
        ComponentExample(
            id: "composer.\(id)", family: .composer, canvas: CGSize(width: 390, height: height),
            dynamicTypeSize: size
        ) {
            let source = ScriptedChat(
                rows: rows, frame: frame, strip: strip, settings: settings, images: CatalogFixtures.images)
            let _ = source.refusedWords = refusedWords
            CatalogChat(source: source) { model in
                ChatStanding(
                    model: model, subject: subject, children: children, dockExpanded: dockExpanded,
                    showing: .constant(showing))
                    .onAppear {
                        if !draft.isEmpty { model.draft = draft }
                        setUp(model)
                    }
            }
            .frame(maxHeight: .infinity, alignment: .bottom)
        }
    }

    private static let composer: [ComponentExample] = {
        typealias F = CatalogFixtures
        return [
            composer("empty", height: 160, frame: ScriptedChat.frame(phase: .idle)),
            // The model's words on a line of their own, whole.
            composer("chip-large-text", height: 420, frame: ScriptedChat.frame(kind: .claudeSdk, phase: .idle),
                     strip: ScriptedChat.strip(model: "claude-fennel-v1", effort: "high"), size: .accessibility3),
            composer("draft", height: 220, frame: ScriptedChat.frame(phase: .idle),
                     draft: "Please tighten the retry path and keep the error visible.") {
                $0.attach(F.photoData, name: "screen.jpg", mime: "image/jpeg", image: true)
            },
            // Each file's name wraps rather than losing its extension, its
            // size under it, each remove mark a thumb's target. One after the
            // other: two attached together land in whichever order they finish.
            composer("attachments-large-text", height: 760, frame: ScriptedChat.frame(phase: .idle),
                     size: .accessibility3) { model in
                Task {
                    await model.attach(
                        Data("# Pairing errors\n".utf8), name: "pairing-errors-before-and-after.md",
                        mime: "text/plain", image: false)?.value
                    _ = model.attach(Data("relay up\n".utf8), name: "relay.log", mime: "text/plain", image: false)
                }
            },
            composer("working", height: 190, rows: [F.runningCommand], frame: ScriptedChat.frame(
                phase: .working,
                activity: Activity(kind: .running(key: F.runningCommand.id), sinceMs: 0, elapsedMs: 12_000, turnElapsedMs: 12_000))),
            composer("thinking", height: 190, frame: ScriptedChat.frame(
                phase: .working, activity: Activity(kind: .thinking, sinceMs: 0, elapsedMs: 24_000, turnElapsedMs: 24_000)),
                draft: "Also check the Windows path."),
            composer("retrying", height: 190, frame: ScriptedChat.frame(
                phase: .working,
                activity: Activity(kind: .retrying(attempt: 2, maxAttempts: 10, retryAtMs: nil), sinceMs: 0, elapsedMs: 3_000, turnElapsedMs: 3_000))),
            composer("queued", height: 400, frame: ScriptedChat.frame(
                phase: .working, activity: Activity(kind: .working, sinceMs: 0, elapsedMs: 41_000, turnElapsedMs: 41_000),
                queue: F.queue)),
            composer("not-confirmed", height: 280, frame: ScriptedChat.frame(underway: [
                SentPrompt(inputId: [4], text: [.text("Run the focused tests.")], lands: .queue, underway: .mayNotHaveArrived),
            ])),
            composer("rejected", height: 240, frame: ScriptedChat.frame(refused: [
                RefusedPrompt(inputId: [5], reason: Refusal(reason: .hostUnreachable, detail: nil)),
            ]), refusedWords: [[5]: "Run the focused tests."]),
            composer("sending", height: 280, frame: ScriptedChat.frame(
                phase: .working, activity: Activity(kind: .working, sinceMs: 0, elapsedMs: 4_000, turnElapsedMs: 4_000),
                underway: [
                    SentPrompt(inputId: [6], text: [.text("Run the focused tests.")], lands: .queue, underway: .sending(waiting: false)),
                    SentPrompt(inputId: [7], text: [.text("Then the Windows check.")], lands: .queue, underway: .sending(waiting: true)),
                ])),
            // A provider subagent's chat: no field, what runs it, and its
            // own Stop while the provider offers one.
            composer("native", height: 200, frame: F.nativeFrame(.offered(key: "toolu_02")),
                     subject: F.nativeSubject),
            composer("native-stopping", height: 200, frame: F.nativeFrame(.stopping), subject: F.nativeSubject),
            composer("native-refused", height: 230, frame: F.nativeFrame(
                .refused(key: "toolu_02", error: "Claude could not stop the task: it already finished.")),
                     subject: F.nativeSubject),
            composer("native-ended", height: 160, frame: F.nativeFrame(.unavailable, phase: .exited(cause: .finished)),
                     subject: F.nativeSubject),
            composer("resume", height: 200, frame: ScriptedChat.frame(phase: .exited(cause: .failed("code 1")), mode: .resume),
                     draft: "Rerun the failing tests."),
            composer("detached", height: 180, frame: ScriptedChat.frame(mode: .waiting(.detached), caughtUp: false),
                     draft: "Also add a test for the new string."),
            composer("catching-up", height: 160, frame: ScriptedChat.frame(mode: .waiting(.catchingUp), caughtUp: false)),
            composer("strip", height: 280, frame: ScriptedChat.frame(phase: .working, activity: Activity(kind: .working, sinceMs: 0, elapsedMs: 94_000, turnElapsedMs: 94_000)),
                     strip: ScriptedChat.strip(
                        tasks: F.pairingTasks,
                        context: ContextView(usedTokens: 168_000, nearFull: true, percent: 84, windowTokens: 200_000),
                        model: "opus 4.6", effort: "high", mode: "plan", background: 2)),
            // The strip once per provider kind, carrying the facts that
            // kind's interpreter reports: the renderer never sees the kind,
            // so these differ only in which facts are present.
            composer("strip-claude-pty", height: 220, frame: ScriptedChat.frame(), strip: ScriptedChat.strip(
                tasks: TasksView(done: 1, total: 3, current: "Resuming from the live checklist", entries: [
                    TaskLine(subject: "Read the live checklist", mark: .done),
                    TaskLine(subject: "Resume from the live checklist", mark: .current),
                    TaskLine(subject: "Report what changed", mark: .todo),
                ]),
                context: ContextView(usedTokens: 171_000, nearFull: true, percent: 86, windowTokens: 200_000),
                model: "claude-sonnet-5", mode: "acceptEdits", background: 2)),
            composer("strip-claude-sdk", height: 220, frame: ScriptedChat.frame(), strip: ScriptedChat.strip(
                context: ContextView(usedTokens: 30_513, nearFull: false, percent: 16, windowTokens: 200_000),
                model: "claude-opus-5-5", effort: "low", mode: "plan",
                usage: UsageView(blocked: false, windows: [UsageWindowView(label: .fiveHour, state: .nearLimit, resetsAtMs: nil, usedPercent: 83)], credits: nil),
                failedServers: [ServerView(name: "claude.ai Google Drive", error: "", needsAuth: true)],
                background: 3)),
            composer("strip-codex", height: 220, frame: ScriptedChat.frame(), strip: ScriptedChat.strip(
                tasks: TasksView(done: 1, total: 3, current: "Split the lexer", entries: [
                    TaskLine(subject: "Read the parser", mark: .done),
                    TaskLine(subject: "Split the lexer", mark: .current),
                    TaskLine(subject: "Run the parser tests", mark: .todo),
                ]),
                context: ContextView(usedTokens: 16_447, nearFull: false, percent: 6, windowTokens: 258_400),
                model: "gpt-5.6-luna", effort: "high", mode: "on-request",
                failedServers: [ServerView(name: "docs", error: "connection refused", needsAuth: false)])),
            // The dock opened: the task list, the agents this one started,
            // then the chip and a queued prompt.
            composer("strip-expanded", height: 1000, frame: ScriptedChat.frame(
                phase: .working, activity: Activity(kind: .working, sinceMs: 0, elapsedMs: 8_000, turnElapsedMs: 8_000),
                queue: [
                    QueuedRow(inputId: [1], text: [.text("Once the suite is green, squash it into one commit.")], steered: false, canWithdraw: true, canSendNow: true, fromAgent: nil),
                ]),
                     strip: ScriptedChat.strip(tasks: F.pairingTasks, model: "opus 4.6", effort: "high"),
                     children: F.family.children + [F.docsSweep], dockExpanded: true),
            composer("plus", height: 360, frame: ScriptedChat.frame(kind: .claudeSdk),
                     strip: ScriptedChat.strip(model: "claude-opus-5-5", effort: "low", mode: "acceptEdits"),
                     settings: F.claudeSdkSettings(mode: "acceptEdits"), showing: .plus),
            composer("dictation-denied", height: 240, frame: ScriptedChat.frame(phase: .idle),
                     draft: "Also check the Windows path.") {
                $0.dictation.prepare(speech: .denied, microphone: .notAsked, available: true)
            },
            composer("dictation-listening", height: 220, frame: ScriptedChat.frame(phase: .idle)) { model in
                model.dictation.prepare(speech: .allowed, microphone: .allowed, available: true)
                model.dictation.began(draft: "Please")
                model.draft = "Please"
                model.heard("tighten the retry path")
            },
            // The agent's review holds drafts: its chip, which the next send
            // carries, and while the agent works a send still, not Stop.
            composer("review-chip", height: 200, frame: ScriptedChat.frame(
                phase: .idle, reviewChip: ReviewChip(comments: 3, moved: false)),
                     draft: "These three, then run the suite."),
            composer("review-chip-working", height: 220, frame: ScriptedChat.frame(
                phase: .working, activity: Activity(kind: .working, sinceMs: 0, elapsedMs: 8_000, turnElapsedMs: 8_000),
                reviewChip: ReviewChip(comments: 1, moved: true))),
            composer("strip-trouble", height: 220, frame: ScriptedChat.frame(), strip: ScriptedChat.strip(
                model: "gpt-5", usage: UsageView(blocked: false, windows: [UsageWindowView(label: .fiveHour, state: .nearLimit, resetsAtMs: nil, usedPercent: 91)], credits: nil),
                failedServers: [ServerView(name: "github", error: "exited", needsAuth: false)])),
            composer("sign-in", height: 240, frame: ScriptedChat.frame(), strip: ScriptedChat.strip(signIn: F.failedSignIn)),
            // The settings card once per provider kind, from the settings
            // view each kind's interpreter state gives.
            composer("settings-claude-sdk", height: 640, frame: ScriptedChat.frame(kind: .claudeSdk),
                     strip: ScriptedChat.strip(model: "claude-opus-5-5", effort: "low", mode: "plan"),
                     settings: F.claudeSdkSettings(), showing: .settings),
            composer("settings-codex", height: 640, frame: ScriptedChat.frame(kind: .codex),
                     strip: ScriptedChat.strip(model: "gpt-6-astra", effort: "medium", mode: "on-request"),
                     settings: F.settingsCodex, showing: .settings),
            composer("settings-claude-pty", height: 420, frame: ScriptedChat.frame(kind: .claudePty),
                     strip: ScriptedChat.strip(model: "claude-sonnet-5", effort: "high", mode: "acceptEdits"),
                     settings: F.settingsClaudePty, showing: .settings),
            composer("chip-stops-asking", height: 160, frame: ScriptedChat.frame(kind: .claudeSdk),
                     strip: ScriptedChat.strip(model: "claude-opus-5-5", effort: "low", mode: "bypassPermissions"),
                     settings: F.claudeSdkSettings(mode: "bypassPermissions")),
            composer("slash", height: 460, frame: ScriptedChat.frame(kind: .claudeSdk), draft: "/c",
                     settings: F.withCommands),
            composer("pasted-text", height: 220, frame: ScriptedChat.frame(phase: .idle)) { model in
                model.type("Why does this fail? ")
                model.type("Why does this fail? " + (1...14).map { "error[E0308]: mismatched types at line \($0)" }.joined(separator: "\n"))
            },
            composer("usage-blocked", height: 200, frame: ScriptedChat.frame(), strip: ScriptedChat.strip(
                usage: UsageView(blocked: true, windows: [UsageWindowView(label: .weekly(model: nil), state: .blocked, resetsAtMs: nil, usedPercent: 100)], credits: nil))),
        ]
    }()

    // MARK: - Chat

    private static func chat(
        _ id: String, height: CGFloat = 844, readiness: (String, String)? = nil,
        chat: @escaping @MainActor () -> ScriptedChat, subject: ChatSubject = CatalogFixtures.subject,
        family: FamilyHeader? = nil, showing: ChatOverlay? = nil,
        size: DynamicTypeSize = .large,
        setUp: @escaping @MainActor (ChatModel, ScriptedChat) -> Void = { _, _ in }
    ) -> ComponentExample {
        example(
            id, height: height, readiness: readiness, chat: chat, subject: subject, family: family,
            showing: showing, size: size, setUp: setUp, deleteWarning: nil)
    }

    /// A settled agent's chat with its delete card up, over a worktree that
    /// goes with it, answered as `ask` answers.
    private static func deleting(_ id: String, ask: @escaping DeleteWarningAsk) -> ComponentExample {
        example(
            id, height: 844, readiness: nil,
            chat: {
                ScriptedChat(
                    rows: CatalogFixtures.conversation, frame: ScriptedChat.frame(phase: .exited(cause: .ended)))
            },
            subject: CatalogFixtures.worktreeSubject, family: nil, showing: .delete, size: .large,
            setUp: { _, _ in }, deleteWarning: ask)
    }

    private static func example(
        _ id: String, height: CGFloat, readiness: (String, String)?,
        chat: @escaping @MainActor () -> ScriptedChat, subject: ChatSubject,
        family: FamilyHeader?, showing: ChatOverlay?, size: DynamicTypeSize,
        setUp: @escaping @MainActor (ChatModel, ScriptedChat) -> Void, deleteWarning: DeleteWarningAsk?
    ) -> ComponentExample {
        ComponentExample(
            id: "chat.\(id)", family: .chat, canvas: CGSize(width: 390, height: height),
            dynamicTypeSize: size,
            readinessIdentifier: readiness?.0, readinessValue: readiness?.1
        ) {
            let source = chat()
            // Pushed, as the app pushes it, so its bar and its back button
            // are the system's.
            Pushed {
                CatalogChat(source: source) { model in
                    ChatScreen(
                        model: model, subject: subject, family: family, showing: showing,
                        deleteWarning: deleteWarning
                    ) { _ in }
                        .onAppear { setUp(model, source) }
                }
            }
        }
    }

    /// Puts the reader of `chat.new-activity` above the newest row: the
    /// first run opened so the chat is taller than the screen, the list
    /// moved to its top once it has laid out, and only then a row arriving.
    private static func readAboveNewActivity(_ model: ChatModel, _ source: ScriptedChat) {
        if let run = model.cell(for: "c14").row?.run { model.toggleRun(run) }
        Task { @MainActor in
            var list: UIScrollView?
            for _ in 0..<100 {
                // Taller than the screen with the header and composer the
                // rows sit between, so the top is somewhere to scroll to.
                if let window = DoorWindow.current, let found = VisibleTree.list(in: window),
                   found.contentSize.height + found.adjustedContentInset.top
                    + found.adjustedContentInset.bottom > found.bounds.height {
                    list = found
                    break
                }
                try? await Task.sleep(for: .milliseconds(20))
            }
            guard let list else { return }
            // An offset set without a drag is not the reader leaving; the
            // model is told, as the debug door's scroll does.
            model.reading(atNewest: false)
            list.setContentOffset(CGPoint(x: list.contentOffset.x, y: -list.adjustedContentInset.top), animated: false)
            source.append([ScriptedChat.row("n1", 100, .prose(text: [.text("Done. The three arms are one now.")], streaming: false, workingNote: false))])
            source.deliver(to: model)
        }
    }

    private static let chat: [ComponentExample] = {
        typealias F = CatalogFixtures
        return [
            chat("conversation", chat: { ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(phase: .idle), strip: ScriptedChat.strip(model: "opus 4.6")) }),
            chat("working", chat: {
                ScriptedChat(
                    rows: F.working,
                    frame: ScriptedChat.frame(
                        phase: .working,
                        activity: Activity(kind: .running(key: "w06"), sinceMs: 0, elapsedMs: 33_000, turnElapsedMs: 33_000)),
                    strip: ScriptedChat.strip(model: "opus 4.6"))
            }),
            chat("run-open", chat: {
                ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(phase: .idle), strip: ScriptedChat.strip(model: "opus 4.6"))
            }) { model, source in
                if let run = model.cell(for: "c14").row?.run { model.toggleRun(run) }
                source.deliver(to: model)
            },
            // The header pill stops growing past the largest ordinary size;
            // the step rows stack their verb, file and counts.
            chat("large-text", chat: {
                ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(phase: .idle), strip: ScriptedChat.strip(model: "opus 4.6"))
            }, size: .accessibility3),
            chat("asking", chat: {
                ScriptedChat(
                    rows: F.conversation + [F.askedCommand], frame: ScriptedChat.frame(phase: .needsYou),
                    card: F.card(.claudeSdk, .command(command: "cargo test -p amux-ui", cwd: "~/src/amux", reason: "", description: "Run the spec suite"), F.sdkPermissionChoices))
            }),
            chat("detached", chat: {
                ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(mode: .waiting(.detached), caughtUp: false))
            }),
            // Four subagents started in one turn: one row with what they
            // come to, one of them waiting on the person.
            chat("subagents", chat: { F.delegatingChat() }),
            chat("subagents-open", chat: { F.delegatingChat() }) { model, source in
                if let group = model.cell(for: "d04").row?.group { model.toggleGroup(group) }
                source.deliver(to: model)
            },
            // A subagent's own chat: its prompt, its work and no field.
            chat("subagent", chat: {
                ScriptedChat(rows: F.subagentWork, frame: F.nativeFrame(.offered(key: "toolu_02"), phase: .working))
            }, subject: F.nativeSubject, family: F.nativeFamily),
            chat("loading", readiness: ("chat.loading", "shown"), chat: {
                ScriptedChat(rows: [], frame: ScriptedChat.frame(mode: .waiting(.catchingUp), caughtUp: false))
            }),
            chat("away", chat: {
                ScriptedChat(rows: [], frame: ScriptedChat.frame(mode: .waiting(.detached), caughtUp: false))
            }, subject: F.awaySubject),
            chat("queued", chat: {
                ScriptedChat(
                    rows: F.working,
                    frame: ScriptedChat.frame(
                        phase: .working,
                        activity: Activity(kind: .running(key: "w06"), sinceMs: 0, elapsedMs: 33_000, turnElapsedMs: 33_000),
                        queue: F.queue),
                    strip: ScriptedChat.strip(model: "opus 4.6"))
            }),
            chat("signed-out", chat: {
                ScriptedChat(
                    rows: F.conversation, frame: ScriptedChat.frame(phase: .idle),
                    strip: ScriptedChat.strip(signIn: F.failedSignIn))
            }),
            // A list that fits on screen is always at its newest row and
            // follows arrivals, so the reader is put above it for real.
            chat("new-activity", readiness: ("chat.newActivity", "shown"), chat: {
                ScriptedChat(rows: F.working, frame: ScriptedChat.frame(phase: .working))
            }) { model, source in readAboveNewActivity(model, source) },
            chat("paging-unreachable", readiness: ("chat.paging", "unreachable"), chat: {
                let source = ScriptedChat(rows: Array(F.conversation.prefix(3)), frame: ScriptedChat.frame(hasOlder: true))
                source.paged = .originUnreachable
                return source
            }),
            chat("family", chat: {
                ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(phase: .working))
            }, family: F.family),
            chat("changes", readiness: ("chat.changes", "Uncommitted"), chat: {
                let source = ScriptedChat(
                    rows: F.conversation, frame: ScriptedChat.frame(phase: .idle),
                    strip: ScriptedChat.strip(git: F.git))
                return source
            }),
            chat("changes-on-branch", readiness: ("chat.changes", "OnBranch"), chat: {
                ScriptedChat(
                    rows: F.conversation, frame: ScriptedChat.frame(phase: .idle),
                    strip: ScriptedChat.strip(git: F.git))
            }) { model, _ in model.compare(.onBranch) },
            chat("plan", chat: {
                ScriptedChat(
                    rows: F.planned, frame: ScriptedChat.frame(kind: .codex, phase: .needsYou),
                    card: F.card(.codex, .plan(body: F.planBody, title: F.planTitle), F.codexPlanChoices))
            }),
            chat("landing", chat: {
                ScriptedChat(
                    rows: F.conversation, frame: ScriptedChat.frame(phase: .idle, underway: [
                        SentPrompt(inputId: [8], text: [.text("Now run the Windows check.")], lands: .feed, underway: .sending(waiting: false)),
                    ]))
            }),
            chat("landing-waiting", chat: {
                ScriptedChat(
                    rows: F.conversation, frame: ScriptedChat.frame(phase: .idle, underway: [
                        SentPrompt(inputId: [8], text: [.text("Now run the Windows check.")], lands: .feed, underway: .sending(waiting: true)),
                    ]))
            }),
            chat("rename", chat: { ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame()) },
                 showing: .rename),
            chat("settle-cuts", chat: {
                ScriptedChat(
                    rows: F.conversation, frame: ScriptedChat.frame(phase: .working),
                    strip: ScriptedChat.strip(background: 2))
            }, family: F.family, showing: .close),
            chat("delete", chat: {
                ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(phase: .exited(cause: .ended)))
            }, subject: F.closedSubject, showing: .delete),
            chat("delete-family", chat: {
                ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(phase: .exited(cause: .ended)))
            }, family: F.family, showing: .delete),
            // The host's answer comes at once: the uncommitted files it counted.
            deleting("delete-worktree") { .success(F.counted) },
            // Still asking the host what the worktree holds.
            deleting("delete-worktree-checking", ask: F.unanswered),
            // The host could not say what the worktree holds.
            deleting("delete-worktree-unknown") { .failure(RuntimeFailure("Studio cannot be reached")) },
            chat("settings", chat: {
                ScriptedChat(
                    rows: F.conversation, frame: ScriptedChat.frame(kind: .claudePty),
                    strip: ScriptedChat.strip(model: "claude-sonnet-5", effort: "high", mode: "acceptEdits"),
                    settings: F.settingsClaudePty)
            }, showing: .settings),
            placeholder("chat.opening", family: .chat, .waiting("Opening this chat"), identifier: "chat"),
            placeholder(
                "chat.not-opened", family: .chat,
                .failed(title: "Couldn’t open this chat", reason: "desk did not answer"), identifier: "chat"),
        ]
    }()

    /// A pushed page standing in for one that is not here, in a navigation
    /// stack as the app pushes it, so its bar is the system's.
    fileprivate static func placeholder(
        _ id: String, family: ComponentExample.Family, _ showing: PagePlaceholder.Showing, identifier: String
    ) -> ComponentExample {
        ComponentExample(
            id: id, family: family, canvas: CGSize(width: 390, height: 844)
        ) {
            Pushed { PagePlaceholder(showing, identifier: identifier) }
        }
    }
}

extension ComponentCatalog {
    // MARK: - Overview

    /// What runs around a chat: background jobs, the changed files by
    /// folder, a tool server that failed and the usage windows near a limit.
    fileprivate static let overview: [ComponentExample] = {
        typealias F = CatalogFixtures
        return [
            overview("overview") { F.surroundings },
            overview("overview-stop", confirming: 0) { F.surroundings },
        ]
    }()

    /// `confirming` opens the stop question on that background job.
    private static func overview(
        _ id: String, comparison: Comparison = .uncommitted, confirming: Int? = nil,
        strip: @escaping @MainActor () -> ScriptedChat.Surroundings
    ) -> ComponentExample {
        ComponentExample(
            id: "chat.\(id)", family: .chat, canvas: CGSize(width: 390, height: 844),
            readinessIdentifier: "chat.overview.comparison", readinessValue: comparison.rawValue
        ) {
            CatalogChat(source: ScriptedChat(
                rows: CatalogFixtures.conversation, frame: ScriptedChat.frame(phase: .working),
                strip: strip())
            ) { model in
                let job = confirming.map { strip().overview.jobs[$0] }
                ChatOverview(
                    model: model, subject: CatalogFixtures.subject, now: CatalogFixtures.now,
                    confirming: job
                ) {}
                    .onAppear { model.compare(comparison) }
            }
        }
    }
}

extension ComponentCatalog {
    // MARK: - Review

    /// The agent's review page, drawn from pages the shared runtime would
    /// hand over, with a scripted chat behind it whose acts change nothing.
    private static func review(
        _ id: String, height: CGFloat = 844, size: DynamicTypeSize = .large,
        page: @escaping @MainActor () -> ReviewPage, writing: Bool = false,
        editing: (@MainActor (ReviewPage) -> ReviewCommentRow?)? = nil,
        setUp: @escaping @MainActor (ReviewSession) -> Void = { _ in }
    ) -> ComponentExample {
        ComponentExample(
            id: "review.\(id)", family: .review, canvas: CGSize(width: 390, height: height),
            dynamicTypeSize: size
        ) {
            // In a navigation stack, as the app pushes it, so its bar is the
            // system's.
            Pushed {
                CatalogReviewPage(page: page(), setUp: setUp) { session in
                    ReviewScreen(session: session, writing: writing, editing: editing?(session.page)) {}
                }
            }
        }
    }

    /// A sent review's round, raised over the chat from its chip.
    private static func round(_ id: String, page: ReviewPage?) -> ComponentExample {
        ComponentExample(id: "review.\(id)", family: .review, canvas: CGSize(width: 390, height: 844)) {
            ReviewRoundSheet(
                round: SentRound(sent: ReviewStory.sent, text: ReviewStory.sentText), page: page,
                read: { page }, openReview: {}, close: {})
        }
    }

    fileprivate static let review: [ComponentExample] = {
        typealias R = ReviewStory
        return [
            review("page") { R.page() },
            review("selection", page: { R.page() }, setUp: R.selectTwoLines),
            review("comment", page: { R.page() }, writing: true, setUp: R.selectTwoLines),
            review("editing", page: { R.page() }, editing: R.firstComment),
            review("folded") { R.page(folded: true) },
            review("since-last") { R.sinceLast() },
            review("drafts-stale") { R.page(banners: [.changedSinceYouStarted]) },
            review("moved-drafts") { R.page(banners: [.movedDrafts(count: 2)]) },
            review("empty") { R.empty() },
            review("nothing-since") { R.nothingSince() },
            review("not-offered") { R.notOffered() },
            review("opening") { ReviewSession.opening },
            review("large-text", size: .accessibility3) { R.page() },
            round("round", page: R.round()),
            round("round-text", page: nil),
            placeholder("review.reading", family: .review, .waiting("Reading the changes"), identifier: "review"),
        ]
    }()
}

/// Holds one review page's session for as long as the example is drawn,
/// over a scripted chat that has the page open.
private struct CatalogReviewPage<Content: View>: View {
    @State private var session: ReviewSession
    let content: (ReviewSession) -> Content

    init(
        page: ReviewPage, setUp: @escaping @MainActor (ReviewSession) -> Void,
        @ViewBuilder content: @escaping (ReviewSession) -> Content
    ) {
        let source = ScriptedChat(rows: [], frame: ScriptedChat.frame(phase: .idle))
        source.anchorForAnySelection = ReviewStory.anchor
        source.openedReview(page)
        let session = ReviewSession(source: source)
        setUp(session)
        _session = State(initialValue: session)
        self.content = content
    }

    var body: some View { content(session) }
}

/// An invented review of one agent's change to how its store opens: the
/// pages the shared runtime would hand over for it.
@MainActor
enum ReviewStory {
    static let snapshot = "5a1c0e7d9b3f42a8c6e1d0f9b7a5c3e1d2f4a6b8"
    static let base = "b0e2d4f6a8c1e3a5c7e9b1d3f5a7c9e1b3d5f7a9"
    static let lastRound = "3c5e7a9b1d3f5a7c9e1b3d5f7a9c1e3a5c7e9b1d"
    static let open = "crates/store/src/open.rs"
    static let lib = "crates/store/src/lib.rs"
    static let lock = "Cargo.lock"

    static let anchor = ReviewAnchor(commit: snapshot, path: open, start: 41, end: 42)

    static let sent = SentReviewRef(
        reviewId: [1], round: 1, snapshot: lastRound, commentIds: [[0x0a], [0x0b]])

    /// What the agent read of the first round.
    static let sentText = """
        crates/store/src/open.rs, line 41, in impl Store › pub fn open:
        +        let conn = Connection::open(path)?;
        Return the store’s own error, not rusqlite’s.

        crates/store/src/open.rs, line 42, in impl Store › pub fn open:
        +        conn.busy_timeout(BUSY)?;
        Name it for what it waits on.
        """

    /// The removed line 41 to the added line 42 of the store's open.
    static func selectTwoLines(_ session: ReviewSession) {
        session.select(index(of: "l/\(open)/41/0", in: session.page))
        session.extend(to: index(of: "l/\(open)/0/42", in: session.page))
    }

    static func index(of key: String, in page: ReviewPage) -> Int {
        page.rows.firstIndex { $0.key == key } ?? 0
    }

    nonisolated static func firstComment(_ page: ReviewPage) -> ReviewCommentRow? {
        page.rows.lazy.compactMap { if case .comment(let comment) = $0.of { comment } else { nil } }.first
    }

    // MARK: - Pages

    /// All changes on a first look: three files, the lock file folded, and
    /// two drafts, one on the lines it is about and one on a whole file.
    static func page(banners: [ReviewBanner] = [], folded: Bool = false) -> ReviewPage {
        var rows: [ReviewRow] = []
        rows.append(head(open, added: 2, removed: 1, comments: 1, folded: folded, scope: true))
        if !folded {
            rows += openBody(drafts: [
                comment([0x01], open, "Why five seconds? The phone’s store waits two.", .draft, .lines(start: 42, end: 42)),
            ])
        }
        rows.append(head(lib, status: .modified, added: 3, removed: 0, comments: 1, folded: folded))
        if !folded {
            rows.append(comment([0x02], lib, "Keep the flags beside the open that uses them.", .draft, .file))
            rows += libBody()
        }
        rows.append(head(lock, added: 4, removed: 1, flags: [.lockfile], folded: true))
        return ReviewPage(
            banners: banners, rows: rows, toFetch: [], opening: false, empty: nil,
            header: header(view: .all, files: 3, added: 9, removed: 2, drafts: 2, sent: 0, rounds: 0))
    }

    /// The next look, since the last round: one file changed since, an
    /// earlier comment sent and outdated, another whose code nearby changed,
    /// and a draft on a new line.
    static func sinceLast() -> ReviewPage {
        var rows: [ReviewRow] = []
        rows.append(head(open, added: 1, removed: 1, comments: 3, changedSinceLast: true))
        rows.append(comment(
            [0x0a], open, "Return the store’s own error, not rusqlite’s.", .sent(round: 1), .outdated,
            snippet: [
                snip(open, .removed, old: 41, "        let conn = Connection::open(path)?;"),
            ], folded: true))
        rows.append(gap(open, hidden: 39, up: ask(open, 1, 39, "impl Store"), down: nil))
        rows.append(hunk(open, old: 40, new: 40, ["impl Store", "pub fn open"]))
        rows.append(code(open, .context, old: 40, new: 40, "    pub fn open(path: &Path) -> Result<Store, StoreError> {"))
        rows.append(code(open, .context, old: 41, new: 41, "        let conn = Connection::open_with_flags(path, FLAGS)?;"))
        rows.append(code(open, .removed, old: 42, "        conn.busy_timeout(BUSY)?;", changed: ["BUSY"]))
        rows.append(code(open, .added, new: 42, "        conn.busy_timeout(BUSY_WAIT)?;", changed: ["BUSY_WAIT"], commented: true))
        rows.append(comment(
            [0x0b], open, "Name it for what it waits on.", .sent(round: 1), .lines(start: 42, end: 42),
            changedNear: true))
        rows.append(comment([0x03], open, "Thanks. Two seconds matches the phone.", .draft, .lines(start: 42, end: 42)))
        rows.append(code(open, .context, old: 43, new: 43, "        migrate(&conn)?;"))
        rows.append(gap(open, hidden: nil, up: nil, down: ask(open, 44, 63, nil)))
        return ReviewPage(
            banners: [], rows: rows, toFetch: [], opening: false, empty: nil,
            header: header(view: .sinceLast, files: 1, added: 1, removed: 1, drafts: 1, sent: 2, rounds: 1))
    }

    /// Nothing left to review.
    static func empty() -> ReviewPage {
        ReviewPage(
            banners: [], rows: [], toFetch: [], opening: false, empty: .nothing,
            header: header(view: .all, files: 0, added: 0, removed: 0, drafts: 0, sent: 0, rounds: 0))
    }

    /// Nothing changed since the last round, which still lists the comment
    /// sent in it.
    static func nothingSince() -> ReviewPage {
        ReviewPage(
            banners: [], rows: [
                head(open, added: 0, removed: 0, comments: 1),
                comment(
                    [0x0b], open, "Name it for what it waits on.", .sent(round: 1), .lines(start: 42, end: 42),
                    snippet: [snip(open, .added, new: 42, "        conn.busy_timeout(BUSY)?;")]),
            ], toFetch: [], opening: false, empty: .nothingSince,
            header: header(view: .sinceLast, files: 0, added: 0, removed: 0, drafts: 0, sent: 1, rounds: 1))
    }

    /// An agent whose computer runs no reviews.
    static func notOffered() -> ReviewPage {
        ReviewPage(banners: [.notOffered], rows: [], toFetch: [], opening: false, empty: nil, header: nil)
    }

    /// The first round, as its chip opens it: each comment it sent on its
    /// file, with the code stored with it.
    static func round() -> ReviewPage {
        let first = { (path: String) in
            ReviewRow(key: "f/\(path)", of: .file(ReviewFileHead(
                path: path, status: .modified, added: 0, removed: 0, binary: false, language: "rust",
                flags: [], folded: false, changedSinceLast: false, inChange: false, comments: 1,
                outline: [], oldPath: nil)))
        }
        return ReviewPage(
            banners: [], rows: [
                first(open),
                comment(
                    [0x0a], open, "Return the store’s own error, not rusqlite’s.", .sent(round: 1),
                    .lines(start: 41, end: 41), snippet: [
                        snip(open, .context, old: 38, new: 38, "impl Store {"),
                        snip(open, .context, old: 40, new: 40, "    pub fn open(path: &Path) -> Result<Store, StoreError> {"),
                        snip(open, .added, new: 41, "        let conn = Connection::open(path)?;"),
                    ], scope: ["impl Store", "pub fn open"]),
                comment(
                    [0x0b], open, "Name it for what it waits on.", .sent(round: 1), .lines(start: 42, end: 42),
                    snippet: [snip(open, .added, new: 42, "        conn.busy_timeout(BUSY)?;")],
                    scope: ["impl Store", "pub fn open"]),
            ], toFetch: [], opening: false, empty: nil,
            header: ReviewHeader(
                baseBranch: "main", baseCommit: base, snapshot: lastRound, view: .all, comparedWith: base,
                views: [ReviewViewTab(view: .all, files: 0)], rounds: 0, files: 0, added: 0, removed: 0,
                drafts: 0, sent: 2, discardDrafts: false, startOver: true))
    }

    // MARK: - Pieces

    private static func header(
        view: ReviewView, files: UInt32, added: UInt32, removed: UInt32, drafts: UInt32, sent: UInt32,
        rounds: UInt32
    ) -> ReviewHeader {
        var views = [ReviewViewTab(view: .all, files: 3)]
        if rounds > 0 { views.append(ReviewViewTab(view: .sinceLast, files: 1)) }
        return ReviewHeader(
            baseBranch: "main", baseCommit: base, snapshot: snapshot, view: view,
            comparedWith: view == .all ? base : lastRound, views: views, rounds: rounds, files: files,
            added: added, removed: removed, drafts: drafts, sent: sent, discardDrafts: drafts > 0,
            startOver: true)
    }

    /// The change to how the store opens: a gap above, the hunk in `impl
    /// Store › pub fn open`, and the rest of the file below.
    private static func openBody(drafts: [ReviewRow]) -> [ReviewRow] {
        var rows: [ReviewRow] = [
            gap(open, hidden: 39, up: ask(open, 1, 39, "impl Store"), down: nil),
            hunk(open, old: 40, new: 40, ["impl Store", "pub fn open"]),
            code(open, .context, old: 40, new: 40, "    pub fn open(path: &Path) -> Result<Store, StoreError> {"),
            code(open, .removed, old: 41, "        let conn = Connection::open(path)?;", changed: ["open"]),
            code(open, .added, new: 41, "        let conn = Connection::open_with_flags(path, FLAGS)?;",
                 changed: ["open_with_flags", "FLAGS"]),
            code(open, .added, new: 42, "        conn.busy_timeout(BUSY)?;", commented: true),
        ]
        rows += drafts
        rows += [
            code(open, .context, old: 42, new: 43, "        migrate(&conn)?;"),
            code(open, .context, old: 43, new: 44, "        Ok(Store { conn })"),
            code(open, .context, old: 44, new: 45, "    }"),
            gap(open, hidden: nil, up: nil, down: ask(open, 46, 65, nil)),
        ]
        return rows
    }

    private static func libBody() -> [ReviewRow] {
        [
            gap(lib, hidden: 11, up: ask(lib, 1, 11, nil), down: nil),
            hunk(lib, old: 12, new: 12, []),
            code(lib, .context, old: 12, new: 12, "use rusqlite::{Connection, OpenFlags};"),
            code(lib, .added, new: 13, "const FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_WRITE;"),
            code(lib, .added, new: 14, "/// How long a write waits on another before giving up."),
            code(lib, .added, new: 15, "const BUSY: Duration = Duration::from_secs(5);"),
        ]
    }

    private static func head(
        _ path: String, status: FileStatus = .modified, added: UInt32, removed: UInt32, comments: UInt32 = 0,
        flags: [ReviewFileFlag] = [], folded: Bool = false, changedSinceLast: Bool = false, scope: Bool = false
    ) -> ReviewRow {
        ReviewRow(key: "f/\(path)", of: .file(ReviewFileHead(
            path: path, status: status, added: added, removed: removed, binary: false, language: "rust",
            flags: flags, folded: folded || !flags.isEmpty, changedSinceLast: changedSinceLast, inChange: true,
            comments: comments, outline: [], oldPath: nil)))
    }

    private static func hunk(_ path: String, old: UInt32, new: UInt32, _ scope: [String]) -> ReviewRow {
        ReviewRow(key: "h/\(path)/\(old)/\(new)", of: .hunk(ReviewHunkHead(
            path: path, oldCommit: base, oldStart: old, newStart: new, scope: scope)))
    }

    private static func gap(_ path: String, hidden: UInt32?, up: ReviewContextAsk?, down: ReviewContextAsk?) -> ReviewRow {
        ReviewRow(
            key: "g/\(path)/\(up?.start ?? down?.start ?? 0)",
            of: .gap(ReviewGap(path: path, loading: false, down: down, hidden: hidden, up: up)))
    }

    private static func ask(_ path: String, _ start: UInt32, _ end: UInt32, _ definition: String?) -> ReviewContextAsk {
        ReviewContextAsk(path: path, commit: snapshot, start: start, end: end, definition: definition)
    }

    private static func comment(
        _ id: [UInt8], _ path: String, _ text: String, _ state: ReviewCommentState, _ at: ReviewCommentAt,
        changedNear: Bool = false, snippet: [ReviewCodeLine] = [], scope: [String] = [], folded: Bool = false
    ) -> ReviewRow {
        ReviewRow(
            key: "c/" + id.map { String(format: "%02x", $0) }.joined(),
            of: .comment(ReviewCommentRow(
                id: id, path: path, text: text, state: state, at: at, changedNear: changedNear,
                scope: scope, snippet: snippet, folded: folded)))
    }

    private static func code(
        _ path: String, _ kind: LineKind, old: UInt32? = nil, new: UInt32? = nil, _ text: String,
        changed: [String] = [], commented: Bool = false
    ) -> ReviewRow {
        ReviewRow(key: "l/\(path)/\(old ?? 0)/\(new ?? 0)", of: .line(line(
            path, kind, old: old, new: new, text, changed: changed, commented: commented)))
    }

    private static func snip(
        _ path: String, _ kind: LineKind, old: UInt32? = nil, new: UInt32? = nil, _ text: String
    ) -> ReviewCodeLine {
        line(path, kind, old: old, new: new, text, changed: [], commented: false)
    }

    private static func line(
        _ path: String, _ kind: LineKind, old: UInt32?, new: UInt32?, _ text: String, changed: [String],
        commented: Bool
    ) -> ReviewCodeLine {
        ReviewCodeLine(
            path: path, kind: kind, text: text, runs: runs(text, changed: changed), commented: commented,
            extra: false, newLine: new, oldLine: old)
    }

    /// A line as the host would colour it: Rust's keywords, types, strings,
    /// numbers and comments, and the words named changed.
    private static func runs(_ text: String, changed: [String]) -> [ReviewRun] {
        if text.trimmingCharacters(in: .whitespaces).hasPrefix("//") {
            return [ReviewRun(text: text, kind: .comment, changed: false)]
        }
        let keywords: Set = ["pub", "fn", "let", "impl", "const", "use", "mut", "return"]
        var out: [ReviewRun] = []
        var word = ""
        func flush() {
            guard !word.isEmpty else { return }
            let kind: TokenKind =
                keywords.contains(word) ? .keyword
                : word.first?.isUppercase == true && word != word.uppercased() ? .type
                : word.first?.isNumber == true ? .number
                : .plain
            out.append(ReviewRun(text: word, kind: kind, changed: changed.contains(word)))
            word = ""
        }
        for character in text {
            if character.isLetter || character.isNumber || character == "_" {
                word.append(character)
            } else {
                flush()
                out.append(ReviewRun(text: String(character), kind: .plain, changed: false))
            }
        }
        flush()
        return out
    }
}

/// Holds one scripted chat's model for as long as the example is drawn.
private struct CatalogChat<Content: View>: View {
    @State private var model: ChatModel
    let content: (ChatModel) -> Content

    init(source: ScriptedChat, @ViewBuilder content: @escaping (ChatModel) -> Content) {
        _model = State(initialValue: ChatModel(source: source, loadingHintAfter: .zero))
        self.content = content
    }

    var body: some View { content(model) }
}

/// The catalogue's invented chat.
@MainActor
enum CatalogFixtures {
    // MARK: - Composer

    /// Prompts behind a running turn: one that can be withdrawn or sent now,
    /// one steering, one from another agent.
    static let queue = [
        QueuedRow(inputId: [1], text: [.text("Then run the Windows check.")], steered: false, canWithdraw: true, canSendNow: true, fromAgent: nil),
        QueuedRow(inputId: [2], text: [.text("Use the new error table instead.")], steered: true, canWithdraw: false, canSendNow: false, fromAgent: nil),
        QueuedRow(inputId: [3], text: [.text("worker-2 finished the specs.")], steered: false, canWithdraw: false, canSendNow: true, fromAgent: "worker-2"),
    ]

    static let failedSignIn = SignInView(state: .failed, account: "ada@example.com", message: "OAuth token has expired.")

    // MARK: - Settings

    private static let claudeModes = ["default", "acceptEdits", "plan", "auto", "bypassPermissions"]
    private static let claudeModeNames = [
        "default": "Default", "acceptEdits": "Accept edits", "plan": "Plan", "auto": "Auto",
        "bypassPermissions": "Bypass permissions",
    ]

    static func claudeSdkSettings(mode: String = "plan") -> SettingsView {
        let efforts = ["low", "medium", "high", "xhigh", "max"]
        return SettingsView(
            models: [
                ModelChoice(value: "default", displayName: "Default (recommended)", description: "Opus 5.5 · Most capable for complex work", efforts: efforts, current: true, unlisted: false, defaultEffort: "high"),
                ModelChoice(value: "sonnet", displayName: "Sonnet", description: "Sonnet 5 · Best for everyday tasks", efforts: efforts, current: false, unlisted: false, defaultEffort: "high"),
                ModelChoice(value: "haiku", displayName: "Haiku", description: "Haiku 4.5 · Fastest for quick answers", efforts: [], current: false, unlisted: false, defaultEffort: nil),
            ],
            efforts: efforts.map { EffortChoice(value: $0, current: $0 == "high", default: $0 == "high", unlisted: false) },
            permissions: claudeModes.map {
                PermissionChoice(
                    value: $0, displayName: claudeModeNames[$0] ?? $0, custom: false, current: $0 == mode,
                    unlisted: false, normal: $0 == "default", neverAsks: $0 == "bypassPermissions",
                    settable: true)
            },
            modes: [],
            changeable: Changeability(model: .pick, effort: .pick, permission: .pick, mode: .notOffered),
            commands: [])
    }

    static let withCommands: SettingsView = {
        var view = claudeSdkSettings()
        view.commands = [
            CommandView(name: "clear", description: "Clear conversation history and free up context", argumentHint: "", source: "", sigil: "/", anywhere: false),
            CommandView(name: "compact", description: "Clear history but keep a summary in context", argumentHint: "<instructions>", source: "", sigil: "/", anywhere: false),
            CommandView(name: "context", description: "Show current context usage", argumentHint: "", source: "", sigil: "/", anywhere: false),
            CommandView(name: "cost", description: "Show the total cost and duration of the session", argumentHint: "", source: "", sigil: "/", anywhere: false),
            CommandView(name: "code-review:review", description: "Review the current diff for correctness bugs", argumentHint: "[level]", source: "code-review", sigil: "/", anywhere: false),
            CommandView(name: "init", description: "Initialize a new CLAUDE.md file", argumentHint: "", source: "", sigil: "/", anywhere: false),
        ]
        return view
    }()

    static let settingsCodex: SettingsView = {
        let efforts = ["low", "medium", "high", "xhigh", "max", "ultra"]
        let presets: [(String, String)] = [
            ("read-only", "Read only"), ("default", "Default"), ("full-access", "Full access"),
        ]
        return SettingsView(
            models: [
                ModelChoice(value: "gpt-6-astra", displayName: "GPT-6-Astra", description: "Frontier agentic coding model.", efforts: efforts, current: true, unlisted: false, defaultEffort: "medium"),
                ModelChoice(value: "gpt-6-sol", displayName: "GPT-6-Sol", description: "Smaller, faster and cheaper.", efforts: efforts, current: false, unlisted: false, defaultEffort: "medium"),
            ],
            efforts: efforts.map { EffortChoice(value: $0, current: $0 == "medium", default: $0 == "medium", unlisted: false) },
            permissions: presets.map { value, name in
                PermissionChoice(
                    value: value, displayName: name, custom: false, current: value == "default", unlisted: false,
                    normal: value == "default", neverAsks: value == "full-access", settable: true)
            },
            modes: [
                ModeChoice(value: "default", displayName: "Default", current: true, unlisted: false, normal: true, settable: true),
                ModeChoice(value: "plan", displayName: "Plan", current: false, unlisted: false, normal: false, settable: true),
            ],
            changeable: Changeability(model: .pick, effort: .pick, permission: .pick, mode: .pick),
            commands: [])
    }()

    static let settingsClaudePty = SettingsView(
        models: [ModelChoice(value: "claude-sonnet-5", displayName: "Sonnet 5", description: "", efforts: [], current: true, unlisted: true, defaultEffort: nil)],
        efforts: [EffortChoice(value: "high", current: true, default: false, unlisted: true)],
        permissions: [PermissionChoice(
            value: "acceptEdits", displayName: "Accept edits", custom: false, current: true, unlisted: false,
            normal: false, neverAsks: false, settable: false)],
        modes: [],
        changeable: Changeability(
            model: .byTyping("/model"), effort: .byTyping("/effort"), permission: .cycle, mode: .notOffered),
        commands: [])

    static let photoData: Data = {
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: 120, height: 80))
        return renderer.jpegData(withCompressionQuality: 0.9) { context in
            UIColor(red: 0.35, green: 0.55, blue: 0.75, alpha: 1).setFill()
            context.fill(CGRect(x: 0, y: 0, width: 120, height: 80))
            UIColor(red: 0.95, green: 0.85, blue: 0.45, alpha: 1).setFill()
            context.fill(CGRect(x: 18, y: 16, width: 36, height: 36))
        }
    }()

    /// The catalogue's clock, where a view reads ages and resets against it.
    static let now = Date(timeIntervalSince1970: 1_790_000_000)

    static let git = Git(
        baseBranch: "main", branch: "refactor-auth",
        checkout: nil,
        onBranch: ChangeTotals(files: 6, added: 120, removed: 30),
        uncommitted: ChangeTotals(files: 5, added: 56, removed: 20))

    static let changes = Changes(
        totals: ChangeTotals(files: 5, added: 56, removed: 20),
        folders: [
            Folder(path: "", files: [
                ChangedFile(path: "README.md", name: "README.md", added: 2, removed: 0, status: .modified, binary: false, language: "", oldPath: nil),
            ]),
            Folder(path: "crates/amux-ui/src/", files: [
                ChangedFile(path: "crates/amux-ui/src/errors.rs", name: "errors.rs", added: 20, removed: 0, status: .added, binary: false, language: "", oldPath: nil),
                ChangedFile(path: "crates/amux-ui/src/pairing.rs", name: "pairing.rs", added: 25, removed: 8, status: .modified, binary: false, language: "", oldPath: nil),
                ChangedFile(path: "crates/amux-ui/src/strip.rs", name: "strip.rs", added: 0, removed: 12, status: .deleted, binary: false, language: "", oldPath: nil),
            ]),
            Folder(path: "docs/", files: [
                ChangedFile(path: "docs/PAIRING.md", name: "PAIRING.md", added: 9, removed: 0, status: .added, binary: false, language: "", oldPath: nil),
            ]),
        ],
        base: nil)

    /// A busy chat's surroundings: two jobs, five changed files, a tool
    /// server that needs signing in and three usage windows.
    static var surroundings: ScriptedChat.Surroundings {
        let ms = Int64(now.timeIntervalSince1970 * 1_000)
        var strip = ScriptedChat.strip(
            usage: UsageView(blocked: false, windows: [
                UsageWindowView(label: .fiveHour, state: .nearLimit, resetsAtMs: nil, usedPercent: 91),
                UsageWindowView(label: .weekly(model: nil), state: .ok, resetsAtMs: ms + 4 * 86_400_000, usedPercent: 40),
                UsageWindowView(label: .weekly(model: "Fable"), state: .nearLimit, resetsAtMs: ms + 4 * 86_400_000, usedPercent: 84),
            ], credits: nil),
            failedServers: [ServerView(name: "github", error: "The token expired.", needsAuth: true)],
            git: git, changes: changes)
        strip.overview.jobs = [
            JobRow(command: "npm run dev -- --port 5173", startedAtMs: ms - 14 * 60_000, kind: .shell, stop: .offered(key: "b1"), step: nil),
            JobRow(command: "cargo watch -x 'test -p amux-ui'", startedAtMs: ms - 40_000, kind: .shell, stop: .stopping, step: nil),
        ]
        return strip
    }

    static let photo = BlobRef(hash: [7, 7, 7], name: "screen.jpg", mime: "image/jpeg", size: 184_320)
    static let log = BlobRef(hash: [8, 8, 8], name: "pairing.log", mime: "text/plain", size: 12_288)
    static let images: [[UInt8]: Data] = [photo.hash: photoData, Array("screen.jpg".utf8): photoData]

    static func bytes(_ blob: BlobRef) -> Data? { images[blob.hash] }

    /// The rows' attachments. Their pictures are made here, ahead of the
    /// first drawing, so a snapshot never catches a row before its picture.
    static let access: AttachmentAccess = {
        for (hash, data) in images { _ = Thumbnail.prepare(data, hash: hash) }
        return AttachmentAccess(bytes: bytes)
    }()

    static let subject = ChatSubject(
        name: "refactor-auth", host: "Studio", directory: "/Users/ada/src/amux", branch: "ada/refactor-auth",
        reach: .online(.direct))
    static let closedSubject = ChatSubject(
        name: "refactor-auth", host: "Studio", directory: "/Users/ada/src/amux", branch: "ada/refactor-auth",
        reach: .online(.direct), filing: .closed)
    /// Closed in a worktree amux made, which deleting it removes with its
    /// uncommitted changes.
    static let worktreeSubject = ChatSubject(
        name: "refactor-auth", host: "Studio", directory: "/Users/ada/src/amux", branch: "ada/refactor-auth",
        reach: .online(.direct), filing: .closed)
    /// The host's preview of what deleting it takes with it.
    static let counted = DeleteWarning.goes(
        WorkspaceGoes(branch: closedSubject.branch, branchCommits: nil, uncommitted: ChangeTotals(files: 3, added: 42, removed: 7)))
    /// A host asked what a delete takes that has not answered yet.
    static func unanswered() async -> Result<DeleteWarning?, RuntimeFailure> {
        try? await Task.sleep(for: .seconds(86_400))
        return .success(nil)
    }
    static let subagentSubject = ChatSubject(
        name: "refactor-auth-tests", host: "Studio", directory: "/Users/ada/src/amux", branch: "ada/refactor-auth",
        reach: .online(.direct), topLevel: false)
    static let awaySubject = ChatSubject(
        name: "refactor-auth", host: "Studio", directory: "/Users/ada/src/amux", reach: .away)

    static let markdown = """
        Done. The three arms are **one** now:

        - `Code::NotFound` and `Code::InvalidPin` share one string
        - the new test asserts on it

        ```rust
        _ => "could not pair, check the code",
        ```
        """

    /// Code in the languages agents write most, each token kind on show,
    /// and a diff fence coloured by its signs.
    static let codeMarkdown = """
        The guard now names the host:

        ```swift
        // Refuse a pairing the host did not offer.
        func refuse(_ code: Int) throws -> String {
            guard code > 0 else { throw PairingError.unknown }
            return "no such host: \\(code)"
        }
        ```

        ```ts
        export const retry = (tries: number): Promise<void> => wait(250 * tries)
        ```

        ```diff
        -      Code::NotFound => "no such host",
        +      _ => "could not pair, check the code",
        ```
        """

    /// A reply still arriving inside its code block: the lines it has
    /// finished are coloured as they stream, the one being written is not.
    static let streamingCode = """
        Here is the loop:

        ```python
        for host in hosts:
            if host.reachable:
                pair(host, retries=3
        """

    /// A plan as the shared view hands it: the title lifted from its
    /// opening heading, and the rest.
    static let planTitle = "Collapse the pairing failures onto one message"
    static let planBody = """
        The client maps gRPC statuses onto distinct strings in three places. The protocol refuses \
        to distinguish them, so the client must not either.

        1. One arm, one string
        2. Update the three specs
        """

    /// An edit's patch as the shared view hands it: one file, its one hunk
    /// with where it starts.
    static let diff: [FilePatch] = [FilePatch(hunks: [PatchHunk(lines: [
        PatchLine(kind: .context, text: "  match status {", number: 12),
        PatchLine(kind: .removed, text: "      Code::NotFound => \"no such host\",", number: 13),
        PatchLine(kind: .removed, text: "      Code::InvalidPin => \"wrong PIN\",", number: 14),
        PatchLine(kind: .added, text: "      _ => \"could not pair, check the code\",", number: 13),
        PatchLine(kind: .context, text: "  }", number: 14),
    ], start: HunkStart(old: 12, new: 12))], file: nil)]

    /// A new file's whole body, every line added.
    static let created: [FilePatch] = [FilePatch(hunks: [PatchHunk(lines: [
        PatchLine(kind: .added, text: "//! Every reason a pairing is refused.", number: 1),
        PatchLine(kind: .added, text: "pub use super::Code;", number: 2),
    ], start: HunkStart(old: 0, new: 1))], file: nil)]

    /// As the shared view reads a schema with a defaulted repository, a
    /// choice of labels and a defaulted toggle.
    static let formFields = [
        FormField(
            name: "repository", title: "Repository", description: "", required: true,
            kind: .text(maxLength: nil, minLength: nil), initial: .text("jlw/amux")),
        FormField(
            name: "labels", title: "Labels", description: "", required: false,
            kind: .choice(options: ["bug", "ios", "docs"]), initial: .choice(0)),
        FormField(
            name: "assign", title: "Assign to me", description: "", required: false, kind: .toggle,
            initial: .toggle(true)),
    ]

    static let redactionQuestion = QuestionView(
        header: "Owner", question: "Which crate should own the redaction table?", multiSelect: false,
        options: [
            OptionView(label: "amux-core", description: "Every client already depends on it", preview: "", recommended: true),
            OptionView(label: "amux-ui", description: "Closest to where the copy is used", preview: "", recommended: false),
            OptionView(label: "A new crate", description: "", preview: "", recommended: false),
        ], allowOther: true, secret: false)

    /// Codex names files as code, between backticks.
    static let codeQuestion = QuestionView(
        header: "Exact target",
        question: "`q1/README.md` is absent; `../plan1/README.md` has two lines (`UPDATED`, `done`). Which should be used?",
        multiSelect: false,
        options: [
            OptionView(
                label: "Create `README.md`", description: "Create the missing file with exactly one line: `UPDATED`.",
                preview: "", recommended: true),
            OptionView(
                label: "Edit the other", description: "Change `../plan1/README.md` instead.",
                preview: "", recommended: false),
        ], allowOther: true, secret: false)

    static let platformQuestion = QuestionView(
        header: "Platforms", question: "Which platforms should the release build?", multiSelect: true,
        options: [
            OptionView(label: "macOS", description: "", preview: "", recommended: false),
            OptionView(label: "Linux", description: "x86_64 and arm64", preview: "", recommended: false),
            OptionView(label: "Windows", description: "", preview: "", recommended: false),
        ], allowOther: true, secret: false)

    static let previewQuestion = QuestionView(
        header: "Layout", question: "Which layout for the host list?", multiSelect: false,
        options: [
            OptionView(label: "Cards", description: "One card per host", preview: """
                ┌──────────────────────┐
                │ Studio        ● live │
                │ ~/src/amux  3 agents │
                ├──────────────────────┤
                │ Laptop       ○ away  │
                └──────────────────────┘
                """, recommended: false),
            OptionView(label: "Grouped list", description: "", preview: "Studio · live\nLaptop · away", recommended: false),
        ], allowOther: false, secret: false)

    static let secretQuestion = QuestionView(
        header: "Token", question: "Paste the deploy token", multiSelect: false, options: [],
        allowOther: true, secret: true)

    static let threeQuestions = [
        platformQuestion,
        QuestionView(
            header: "Rollout", question: "How should the migration roll out?", multiSelect: false,
            options: [
                OptionView(label: "Behind a flag", description: "Off by default for one release", preview: "", recommended: false),
                OptionView(label: "All at once", description: "", preview: "", recommended: false),
            ], allowOther: true, secret: false),
        QuestionView(
            header: "Logging", question: "What should be logged?", multiSelect: false,
            options: [
                OptionView(label: "Everything", description: "", preview: "", recommended: false),
                OptionView(label: "Nothing", description: "", preview: "", recommended: false),
            ], allowOther: true, secret: false),
    ]

    static let sdkPermissionChoices = [
        Choice(outcome: .allowOnce, primary: true, takesNote: false),
        Choice(outcome: .allowAlways(subjects: ["cargo test"], directories: [], mode: "", modeName: "", scope: .project), primary: false, takesNote: false),
        Choice(outcome: .allowForSession, primary: false, takesNote: false),
        Choice(outcome: .deny, primary: false, takesNote: true),
        Choice(outcome: .denyAndStop, primary: false, takesNote: true),
    ]

    static let codexPlanChoices = [
        Choice(outcome: .approvePlan(mode: "", modeName: ""), primary: true, takesNote: false),
        Choice(outcome: .sendBack, primary: false, takesNote: true),
    ]

    /// A prompt, the agent's look around, and the plan it proposed.
    static let planned: [Row] = [
        ScriptedChat.row("p1", 1, .prompt(text: [.text("Plan how to collapse the pairing errors.")], steered: false)),
        ScriptedChat.row("e1", 2, .explore(verb: .search, subject: "INVALID_PIN", state: .succeeded, result: "")),
        ScriptedChat.row("e2", 3, .explore(verb: .read, subject: "wire/src/codes.rs", state: .succeeded, result: "")),
        ScriptedChat.row("l1", 4, .ask(.plan(body: planBody, verdict: .open, mode: "", modeName: "", writing: false, note: nil, title: planTitle))),
    ]

    static func card(
        _ kind: Kind, _ body: AskBody, _ choices: [Choice], state: CardState = .open, count: UInt = 1
    ) -> AskCard {
        AskCard(
            kind: kind, key: "ask-1", itemKey: "x6", position: 1, count: count, body: body,
            choices: choices, questionNote: kind != .claudePty, questionSkip: kind != .claudePty,
            questionReply: true, stopsTurn: true, defaultToNo: false, state: state)
    }

    static let runningCommand = ScriptedChat.row("rc", 50, .command(
        command: "cargo test -p amux-ui", description: "", state: .running, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [],
        durationMs: nil, exitCode: nil))

    static let askedCommand = ScriptedChat.row("x6", 60, .command(
        command: "cargo test -p amux-ui", description: "", state: .asking, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [],
        durationMs: nil, exitCode: nil), attention: true)

    /// A finished turn: the prompt, one run of steps the agent took (a
    /// failed check it fixed, a lint it left failing, a command the person
    /// refused) and its answer.
    static let conversation: [Row] = {
        func r(
            _ id: String, _ order: UInt64, _ kind: RowKind, attention: Bool = false,
            decision: Decision? = nil
        ) -> Row {
            ScriptedChat.row(id, order, kind, attention: attention, decision: decision)
        }
        func command(_ command: String, _ state: CallPhase, exit: Int32? = 0, ms: Int64? = nil, output: [String] = [], more: UInt = 0) -> RowKind {
            .command(command: command, description: "", state: state, outputHead: output, moreLines: more, outputTrimmed: false, outputTail: [], durationMs: ms, exitCode: exit)
        }
        func edit(_ path: String, _ added: UInt32, _ removed: UInt32) -> RowKind {
            .fileChange(files: [FileRow(path: path, change: .edited, added: added, removed: removed, line: nil)], state: .succeeded)
        }
        return [
            r("c02", 2, .prompt(text: [.text("Collapse the pairing errors onto one string.")], steered: false)),
        ] + ScriptedChat.run([
            r("c03", 3, .thinking(text: "", open: false, durationMs: 8_000)),
            r("c04", 4, .explore(verb: .search, subject: "INVALID_PIN", state: .succeeded, result: "")),
            r("c05", 5, .explore(verb: .read, subject: "crates/wire/src/codes.rs", state: .succeeded, result: "")),
            r("c06", 6, .explore(verb: .read, subject: "crates/amux-ui/src/pairing.rs", state: .succeeded, result: "")),
            r("c07", 7, edit("crates/amux-ui/src/pairing.rs", 9, 14)),
            r("c08", 8, command("cargo check -p amux-ui", .failed, exit: 101, ms: 4_200, output: ["error[E0308]: mismatched types"], more: 214)),
            r("c09", 9, edit("crates/amux-ui/src/pairing.rs", 2, 1)),
            r("c10", 10, command("cargo check -p amux-ui", .succeeded, ms: 3_900)),
            r("c11", 11, .fileChange(files: [FileRow(path: "crates/amux-ui/tests/spec/pairing_copy.rs", change: .created(lines: 38), added: 38, removed: 0, line: nil)], state: .succeeded)),
            r("c12", 12, command("cargo test -p amux-ui", .succeeded, ms: 12_000),
              decision: Decision(outcome: .allowed, elsewhere: false, granted: .session, note: nil)),
            r("c13", 13, command("cargo clippy -p amux-ui", .failed, exit: 1, ms: 6_100, output: ["warning: unused import: `PinError`"], more: 12)),
            r("c14", 14, command("rm -rf target", .denied, exit: nil),
              decision: Decision(outcome: .denied, elsewhere: false, granted: nil, note: "Use cargo clean instead")),
        ], failed: ["c13"]) + [
            r("c15", 15, .prose(text: [.text("Done. The three arms are one now, and the new test asserts on the single string. Clippy still flags an unused import I left for you to decide on.")], streaming: false, workingNote: false)),
            r("c16", 16, .turnEnd(failed: false, durationMs: 102_000)),
        ]
    }()

    /// The finished turn, then a second one the agent is still at: five
    /// steps in, its newest running.
    static let working: [Row] = {
        func r(_ id: String, _ order: UInt64, _ kind: RowKind) -> Row {
            ScriptedChat.row(id, order, kind)
        }
        return conversation + [
            r("w01", 20, .prompt(text: [.text("Drop the unused import and run the whole suite.")], steered: false)),
        ] + ScriptedChat.run([
            r("w02", 21, .explore(verb: .read, subject: "crates/amux-ui/src/pairing.rs", state: .succeeded, result: "")),
            r("w03", 22, .fileChange(files: [FileRow(path: "crates/amux-ui/src/pairing.rs", change: .edited, added: 0, removed: 1, line: nil)], state: .succeeded)),
            r("w04", 23, .command(command: "cargo clippy -p amux-ui", description: "", state: .succeeded, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [], durationMs: 5_800, exitCode: 0)),
            r("w05", 24, .explore(verb: .search, subject: "pairing_copy", state: .succeeded, result: "")),
            r("w06", 25, .command(command: "cargo test --workspace", description: "Run the whole suite", state: .running, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [], durationMs: nil, exitCode: nil)),
        ], live: true)
    }()

    static let pairingTasks = TasksView(
        done: 3, total: 7, current: "Updating the three spec tests",
        entries: [
            TaskLine(subject: "Find every call site that maps a status to a string", mark: .done),
            TaskLine(subject: "Collapse the match in pairing.rs onto one arm", mark: .done),
            TaskLine(subject: "Delete the three unused error constants", mark: .done),
            TaskLine(subject: "Update the three spec tests that assert on the old strings", mark: .current),
            TaskLine(subject: "Run the spec suite", mark: .todo),
            TaskLine(subject: "Check nothing in docs asserts on the old copy", mark: .todo),
            TaskLine(subject: "Write the changelog line", mark: .todo),
        ])

    static let docsSweep = FleetCard(
        agent: AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: 5, count: 16)),
        name: "docs-sweep", handle: "docs-sweep", kind: .claudeSdk, attention: .exited, cwd: "", phaseSinceMs: 0,
        host: "Studio", hostReach: .online(.direct), children: 0, familyAttention: .exited,
        members: 1, descendants: 0, backgroundJobs: 0, closeHeld: false, filing: .open, topLevel: true, native: false, branch: nil, exitCause: .finished, repository: nil)

    static let family = FamilyHeader(
        children: [
            FleetCard(agent: AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: 3, count: 16)),
                      name: "worker-2", handle: "worker-2", kind: .claudeSdk, attention: .needsYou, cwd: "", phaseSinceMs: 0,
                      host: "Laptop", hostReach: .online(.direct), children: 0, familyAttention: .needsYou,
                      members: 1, descendants: 0, backgroundJobs: 0, closeHeld: false, filing: .open, topLevel: true, native: false, branch: nil, exitCause: nil, repository: nil),
            FleetCard(agent: AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: 4, count: 16)),
                      name: "worker-3", handle: "worker-3", kind: .codex, attention: .working, cwd: "", phaseSinceMs: 0,
                      host: "Studio", hostReach: .online(.direct), children: 0, familyAttention: .working,
                      members: 1, descendants: 0, backgroundJobs: 0, closeHeld: false, filing: .open, topLevel: true, native: false, branch: nil, exitCause: nil, repository: nil),
        ],
        attention: .needsYou, needYou: 1, parent: nil)
}

/// A component on a fixed, production-coloured canvas.
///
/// `reducesTransparency` draws every frosted surface flat, as it is for a
/// person who turned Reduce Transparency on: what the snapshot comparison
/// photographs. Off, the surfaces are glass and material, as the app draws
/// them on a phone.
@MainActor
struct ComponentExampleView: View {
    let example: ComponentExample
    let appearance: ColorScheme
    let reducesTransparency: Bool

    init(example: ComponentExample, appearance: ColorScheme, reducesTransparency: Bool) {
        self.example = example
        self.appearance = appearance
        self.reducesTransparency = reducesTransparency
    }

    var body: some View {
        ZStack {
            Ground()
            example.build()
                .padding([.chat, .fleet, .hosts, .pairing, .settings, .report].contains(example.family) ? 0 : 18)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
        }
        .frame(width: example.canvas.width, height: example.canvas.height)
        .clipped()
        .environment(\.design, Design.app)
        .environment(\.codeHighlighting, .bridge)
        .environment(\.photographed, true)
        .environment(\.reducesMotion, true)
        .environment(\.reducesTransparency, reducesTransparency)
        .environment(\.dynamicTypeSize, example.dynamicTypeSize)
        .preferredColorScheme(appearance)
    }
}

/// Keeps the snapshot target at the app boundary while reusing the production
/// in-process reporting path.
@MainActor
struct ComponentReadinessReportingView: View {
    private let content: AnyView
    private let didChange: @MainActor ([(identifier: String, value: String?)]) -> Void

    init<Content: View>(
        content: Content,
        didChange: @escaping @MainActor ([(identifier: String, value: String?)]) -> Void
    ) {
        self.content = AnyView(content)
        self.didChange = didChange
    }

    var body: some View {
        content
            .reportingIdentifiedElements(includeGeometry: false)
            .onPreferenceChange(IdentifiedElements.self) { declared in
                let values = declared.map { (identifier: $0.identifier, value: $0.value) }
                Task { @MainActor in didChange(values) }
            }
    }
}

/// Browsable preview backed by the same inventory snapshot tests use.
@MainActor
struct ComponentCatalogGallery: View {
    @State private var appearance = Appearance.light

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 24) {
                ForEach(ComponentCatalog.examples) { example in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(example.id).font(.headline.monospaced())
                        ComponentExampleView(
                            example: example, appearance: appearance.colorScheme,
                            reducesTransparency: false)
                    }
                }
            }
            .padding(20)
        }
    }
}

#Preview("Component Catalog") {
    ComponentCatalogGallery()
}

extension ComponentCatalog {
    // MARK: - New agent

    /// Starting an agent on a host whose catalogue arrived: the chosen
    /// provider's model, effort, permission and mode, and the worktree
    /// switch; once more with the provider signed out there.
    fileprivate static let newAgent: [ComponentExample] = [
        newAgent("claude", provider: .claude, signedIn: true),
        newAgent("codex", provider: .codex, signedIn: true),
        newAgent("browsing", provider: .claude, signedIn: true, browsing: true),
        newAgent("signed-out", provider: .claude, signedIn: false),
        newAgent("large-text", provider: .claude, signedIn: true, size: .accessibility3, height: 2400),
        newAgent("refused", provider: .claude, signedIn: true) { store in
            store.starts()
            store.started(.failure(RuntimeFailure("desk refused: ~/src/amux is not a git repository")))
        },
        newAgent("no-repositories", provider: .claude, signedIn: true, browsing: true) { store in
            store.listed(.success(Directories(recent: [], repositories: [], history: [])), asked: store.asking())
        },
        newAgent("unlisted", provider: .claude, signedIn: true, browsing: true) { store in
            store.listed(.failure(RuntimeFailure("desk did not answer")), asked: store.asking())
        },
    ]

    private static func newAgent(
        _ id: String, provider: NewAgentStore.Provider, signedIn: Bool, browsing: Bool = false,
        size: DynamicTypeSize = .large, height: CGFloat = 1100,
        setUp: @escaping @MainActor (NewAgentStore) -> Void = { _ in }
    ) -> ComponentExample {
        ComponentExample(
            id: "new-agent.\(id)", family: .chat, canvas: CGSize(width: 390, height: height),
            dynamicTypeSize: size,
            readinessIdentifier: browsing ? "new-agent.browse" : "new-agent.worktree",
            readinessValue: browsing ? "" : "on"
        ) {
            let (store, hosts) = CatalogFixtures.newAgent(provider: provider, signedIn: signedIn)
            let _ = setUp(store)
            // The chooser is drawn on its own: a capture leaves out what the
            // screen presents, and the form under it is the other stories.
            if browsing {
                NewAgentDirectories(model: store) { _ in }
            } else {
                NavigationStack {
                    NewAgent(model: store, hosts: hosts) { _ in }
                }
            }
        }
    }
}

extension CatalogFixtures {
    static let desk = HostId(bytes: Array(repeating: 7, count: 16))!

    static func newAgent(
        provider: NewAgentStore.Provider, signedIn: Bool
    ) -> (NewAgentStore, HostsStore) {
        let hosts = HostsStore()
        hosts.stopWaiting()
        hosts.show([HostView(
            hostId: desk.bytes, name: "desk", local: false, trusted: true, candidate: false,
            reach: .online(.direct), link: 0, addrs: [], relayOnly: false, pairingOpen: false, current: true,
            providers: [
                ProviderSignIn(provider: .claude, signedIn: signedIn),
                ProviderSignIn(provider: .codex, signedIn: true),
            ],
            claudeKind: nil, lastDialError: nil, platform: .macos, signedIn: nil, through: nil, version: nil),
        HostView(
            hostId: Array(repeating: 8, count: 16), name: "laptop", local: false, trusted: true,
            candidate: false, reach: .away, link: 0, addrs: [], relayOnly: false, pairingOpen: false, current: false, providers: [],
            claudeKind: nil, lastDialError: nil, platform: .macos, signedIn: nil, through: nil, version: nil)])
        let store = NewAgentStore()
        store.open(on: desk)
        store.listed(.success(Directories(
            recent: [
                Directory(path: "~/src/amux", name: "amux", repository: nil),
                Directory(path: "~/src/amuxcloud", name: "amuxcloud", repository: nil),
                Directory(path: "~/src/promo", name: "promo", repository: nil),
            ],
            repositories: [
                Directory(path: "~/src/amux", name: "amux", repository: nil),
                Directory(path: "~/src/dotfiles", name: "dotfiles", repository: nil),
                Directory(path: "~/src/skills", name: "skills", repository: nil),
            ], history: [])), asked: store.asking())
        for offered in NewAgentStore.Provider.allCases {
            let catalogue = offered == .codex ? codexCatalogue : claudeCatalogue
            store.offered(.success(catalogue), for: offered, asked: store.askingOffer(for: offered))
        }
        store.choose(provider: provider)
        if provider == .codex {
            store.choose(.model("gpt-6-astra"))
            store.choose(.effort("high"))
            store.choose(.permission("read-only"))
            store.choose(.mode("plan"))
        }
        store.newWorktree = true
        return (store, hosts)
    }

    static let codexCatalogue = Catalogue(
        models: [
            OfferedModel(value: "gpt-6-astra", displayName: "GPT-6-Astra", description: "Frontier agentic coding model.", efforts: ["low", "medium", "high"], resolvedModel: "", defaultEffort: "medium"),
            OfferedModel(value: "gpt-6-sol", displayName: "GPT-6-Sol", description: "Smaller, faster and cheaper.", efforts: ["low", "medium"], resolvedModel: "", defaultEffort: "low"),
        ],
        commands: [],
        permissions: [
            OfferedPermission(value: "read-only", displayName: "Read only", normal: false, neverAsks: false, settable: true, models: []),
            OfferedPermission(value: "default", displayName: "Default", normal: true, neverAsks: false, settable: true, models: []),
            OfferedPermission(value: "full-access", displayName: "Full access", normal: false, neverAsks: true, settable: true, models: []),
        ],
        modes: [
            OfferedMode(value: "default", displayName: "Default", normal: true, settable: true),
            OfferedMode(value: "plan", displayName: "Plan", normal: false, settable: true),
        ],
        skills: [], newAgent: nil)

    static let claudeCatalogue = Catalogue(
        models: [
            OfferedModel(value: "default", displayName: "Default (recommended)", description: "Opus 5.5 · Most capable for complex work", efforts: ["low", "medium", "high"], resolvedModel: "", defaultEffort: "high"),
            OfferedModel(value: "sonnet", displayName: "Sonnet", description: "Sonnet 5 · Best for everyday tasks", efforts: ["low", "medium", "high"], resolvedModel: "", defaultEffort: "high"),
        ],
        commands: [],
        permissions: [
            OfferedPermission(value: "default", displayName: "Default", normal: true, neverAsks: false, settable: true, models: []),
            OfferedPermission(value: "acceptEdits", displayName: "Accept edits", normal: false, neverAsks: false, settable: true, models: []),
            OfferedPermission(value: "plan", displayName: "Plan", normal: false, neverAsks: false, settable: true, models: []),
            OfferedPermission(value: "bypassPermissions", displayName: "Bypass permissions", normal: false, neverAsks: true, settable: true, models: []),
        ],
        modes: [],
        skills: [], newAgent: nil)
}

private extension QuestionResponse {
    static func picked(_ positions: [UInt32]) -> QuestionResponse {
        QuestionResponse(note: nil, other: nil, picked: positions)
    }

    static func typed(_ words: String) -> QuestionResponse {
        QuestionResponse(note: nil, other: words, picked: [])
    }
}

extension ComponentCatalog {
    // MARK: - Hosts

    /// The machines in every group a machine can be in, with one found and
    /// not paired, and none at all; then one machine's page, and one that no
    /// longer trusts this phone. The revoke confirmation is the system's own
    /// dialog, which a flat capture cannot hold; it has a preview of its own.
    fileprivate static let hosts: [ComponentExample] = [
        hostsList("list") { _ in },
        hostsList("empty", hosts: []) { _ in },
        hostsList("network-refused", hosts: []) { $0.sawLocalNetwork(.denied) },
        // A machine on the account only the relay reaches, on the free plan:
        // its card sells what pairs it rather than a code that cannot.
        hostsList("on-the-account-free", hosts: CatalogFixtures.machines + [CatalogFixtures.accountServer]) {
            $0.show(CatalogFixtures.account(pro: false))
        },
        hostsList("large-text", size: .accessibility3) { _ in },
        hostPage("page", host: CatalogFixtures.deskHost),
        hostPage("revoked", host: CatalogFixtures.oldLaptop),
    ]

    private static func hostsList(
        _ id: String, hosts: [HostView] = CatalogFixtures.machines, size: DynamicTypeSize = .large,
        setUp: @escaping @MainActor (HostsStore) -> Void
    ) -> ComponentExample {
        ComponentExample(
            id: "hosts.\(id)", family: .hosts, canvas: CGSize(width: 390, height: 844), dynamicTypeSize: size
        ) {
            let store = CatalogFixtures.hostsStore(hosts)
            // In a navigation stack, as the tab holds it, so its title is the
            // system's.
            NavigationStack {
                HostsTab(model: store) { _ in }
                    .onAppear { setUp(store) }
            }
        }
    }

    private static func hostPage(_ id: String, host: HostView) -> ComponentExample {
        ComponentExample(
            id: "host.\(id)", family: .hosts, canvas: CGSize(width: 390, height: 844)
        ) {
            hostPageView(host: host)
        }
    }

    /// One machine's page as the hosts tab pushes it.
    static func hostPageView(host: HostView, confirming: Bool = false) -> some View {
        Pushed {
            HostPage(
                model: CatalogFixtures.hostsStore(CatalogFixtures.machines), host: host.id!,
                confirming: confirming
            ) { _ in }
        }
    }
}

extension ComponentCatalog {
    // MARK: - Pairing

    /// Pairing with the laptop found on this network by its six digits, from
    /// an empty keypad to the machine's name and key and the pairing done; a
    /// code that did not work, one the machine refused in words for the
    /// person, and one that reached a machine only the relay can carry; then
    /// an invitation link waiting, refused, and naming a machine that needs
    /// an account.
    fileprivate static let pairing: [ComponentExample] = [
        code("empty") { _ in },
        code("offered", machine: CatalogFixtures.openLaptop) { _ in },
        code("typing") { $0.enter("418") },
        code("checking") { store in
            store.enter("418207")
            _ = store.checking()
        },
        code("refused") { store in
            store.enter("418207")
            store.reached(.failure(RuntimeFailure("wrong secret")), attempt: store.checking())
        },
        code("already-paired") { store in
            store.enter("418207")
            store.reached(
                .failure(RuntimeFailure("Already paired with studio. Unpair it first.")),
                attempt: store.checking())
        },
        code("subscribe", machine: CatalogFixtures.relayLaptop) { store in
            store.enter("418207")
            store.reached(.failure(RuntimeFailure("relay refused")), attempt: store.checking(), relayOnly: true)
        },
        confirm("confirm") { store in
            store.enter("418207")
            store.reached(.success(CatalogFixtures.pendingLaptop), attempt: store.checking())
        },
        confirm("trusted") { store in
            store.enter("418207")
            let attempt = store.checking()
            store.reached(.success(CatalogFixtures.pendingLaptop), attempt: attempt)
            store.paired(.success(CatalogFixtures.pendingLaptop.name), attempt: attempt)
        },
        confirm("link-checking", machine: nil) { _ = $0.checking() },
        confirm("link-refused", machine: nil) { store in
            store.reached(.failure(RuntimeFailure("expired")), attempt: store.checking())
        },
        ComponentExample(
            id: "pair.needs-account", family: .pairing, canvas: CGSize(width: 390, height: 844)
        ) {
            NavigationStack { PairNeedsAnAccount { _ in } }
        },
        offer("offered", .offered(CatalogFixtures.deskOffer)),
        offer("offered-accepting", .offered(CatalogFixtures.deskOffer), working: true),
        offer("offered-offline", .offered(CatalogFixtures.deskOffer), failure: .unreachable("desk")),
        offer("offering", .offering(host: CatalogFixtures.nearbyLaptop, email: "ada@example.com")),
    ]

    /// An account offered across a pairing: the desk offering this phone
    /// its account, accepted and the desk away, and this phone offering a
    /// signed-out laptop it just paired its own.
    private static func offer(
        _ id: String, _ prompt: SignInPrompt, working: Bool = false, failure: SignInOffers.Failure? = nil
    ) -> ComponentExample {
        ComponentExample(
            id: "pair.sign-in-\(id)", family: .pairing, canvas: CGSize(width: 390, height: 844)
        ) {
            NavigationStack { SignInOfferPanel(prompt: prompt, working: working, failure: failure) { _ in } }
        }
    }

    /// The code in the system's number pad, as the pairing sheet asks for it
    /// for one machine. The field takes focus once drawn and the pad rises
    /// after, so the picture waits for the screen to stop changing.
    private static func code(
        _ id: String, machine: HostView = CatalogFixtures.nearbyLaptop,
        setUp: @escaping @MainActor (PairingStore) -> Void
    ) -> ComponentExample {
        ComponentExample(
            id: "pair.code-\(id)", family: .pairing, canvas: CGSize(width: 390, height: 844)
        ) {
            let store = PairingStore(clock: { CatalogFixtures.now })
            store.open(machine: machine)
            setUp(store)
            return NavigationStack { PairByCode(model: store) { _ in } }
        }
    }

    /// The machine's answer, after a code or from a link.
    private static func confirm(
        _ id: String, machine: HostView? = CatalogFixtures.nearbyLaptop,
        setUp: @escaping @MainActor (PairingStore) -> Void
    ) -> ComponentExample {
        ComponentExample(
            id: "pair.\(id)", family: .pairing, canvas: CGSize(width: 390, height: 844)
        ) {
            let store = PairingStore(clock: { CatalogFixtures.now })
            store.open(machine: machine)
            setUp(store)
            return NavigationStack { PairConfirmation(model: store) { _ in } }
        }
    }
}

extension ComponentCatalog {
    // MARK: - Settings

    /// The settings sheet with two accounts and with none, signing in from
    /// it, and every page it opens: subscribing and deleting an account.
    /// Removing one, and what a sign-in asks when it is refused or comes back
    /// as somebody else, are the system's dialogs, which a flat capture
    /// cannot hold; they have previews of their own.
    fileprivate static let settings: [ComponentExample] = [
        sheet("two-accounts") { StoryApp(settingsOpen: true) { StoryFleet.busy(accounts: 2) }.settings },
        sheet("signed-out") { StoryApp(settingsOpen: true) { StoryFleet.busy(accounts: 0) }.settings },
        sheet("signing-in") {
            StoryApp(settingsOpen: true, signIn: SignInStore(phase: .handingOff)) {
                StoryFleet.busy(accounts: 0)
            }.settings
        },
        sheet("notifications") { StoryApp(settingsOpen: true, notifications: .on).settings },
        // Tall enough to hold the whole sheet, down to its help rows.
        sheet("large-text", size: .accessibility3, height: 2600) { StoryApp(settingsOpen: true).settings },
        sheet("delete") {
            let stores = StoryFleet.busy()
            return DeleteAccountOverlay(
                entry: stores.accounts.selectedAccount, model: DeletionStore(asking: AccountId("ada")),
                actions: { _ in }
            ) { YouScreen(accounts: stores.accounts) { _ in } }
        },
        sheet("delete-blocked") {
            let stores = StoryFleet.busy()
            return DeleteAccountOverlay(
                entry: stores.accounts.selectedAccount,
                model: DeletionStore(
                    asking: AccountId("ada"), typed: "ada@example.com",
                    phase: .blocked(source: .appStore, manageURL: AmuxCloudService.appStoreSubscriptions)),
                actions: { _ in }
            ) { YouScreen(accounts: stores.accounts) { _ in } }
        },
        sheet("delete-failed") {
            let stores = StoryFleet.busy()
            return DeleteAccountOverlay(
                entry: stores.accounts.selectedAccount,
                model: DeletionStore(
                    asking: AccountId("ada"), typed: "ada@example.com", phase: .failed("amux.sh did not answer")),
                actions: { _ in }
            ) { YouScreen(accounts: stores.accounts) { _ in } }
        },
        sheet("paywall", pushed: true) { Paywall(model: PaywallStore(plans: CatalogFixtures.plans)) { _ in } },
        sheet("paywall-confirming", pushed: true) {
            Paywall(model: PaywallStore(plans: CatalogFixtures.plans, phase: .confirming)) { _ in }
        },
        sheet("paywall-unconfirmed", pushed: true) {
            Paywall(
                model: PaywallStore(plans: CatalogFixtures.plans, phase: .unconfirmed(.unreachable))) { _ in }
        },
        sheet("paywall-subscribed", pushed: true) {
            Paywall(
                model: PaywallStore(
                    entitlement: .active(grant: .purchased(.appStore), renews: CatalogFixtures.now.addingTimeInterval(86_400 * 300)),
                    plans: CatalogFixtures.plans, phase: .entitled(.purchased(.appStore)))) { _ in }
        },
    ]

    private static func sheet(
        _ id: String, size: DynamicTypeSize = .large, height: CGFloat = 844, pushed: Bool = false,
        build: @escaping @MainActor () -> some View
    ) -> ComponentExample {
        ComponentExample(
            id: "settings.\(id)", family: .settings, canvas: CGSize(width: 390, height: height), dynamicTypeSize: size
        ) {
            if pushed { Pushed { build() } } else { NavigationStack { build() } }
        }
    }
}

/// A page as the app shows it: pushed onto a stack, so the system bar draws
/// the back button a person sees on it, which a stack's root never has.
struct Pushed<Content: View>: View {
    @ViewBuilder let content: () -> Content

    var body: some View {
        NavigationStack(path: .constant([0])) {
            Color.clear.navigationDestination(for: Int.self) { _ in content() }
        }
    }
}

extension ComponentCatalog {
    // MARK: - Report

    /// Reporting a problem, from writing it to the receipt, over a frozen
    /// frame of the agent list.
    fileprivate static let report: [ComponentExample] = [
        page("ready") { ReportStore(capture: CatalogFixtures.frozenFleet, open: true) },
        page("marked") {
            ReportStore(
                capture: CatalogFixtures.frozenFleet,
                draft: ReportDraft(
                    note: "The subtitle says no agents while three are listed.",
                    marks: [
                        ReportMark(x: 16, y: 104, width: 220, height: 30, note: "Says no agents"),
                        ReportMark(x: 12, y: 300, width: 366, height: 70, note: ""),
                    ]),
                open: true)
        },
        page("signed-out", signedIn: false) { ReportStore(capture: CatalogFixtures.frozenFleet, open: true) },
        page("sending") {
            ReportStore(
                capture: CatalogFixtures.frozenFleet, draft: ReportDraft(note: "The list jumps when a row finishes."),
                sending: .sending, open: true)
        },
        page("failed") {
            ReportStore(
                capture: CatalogFixtures.frozenFleet, draft: ReportDraft(note: "The list jumps when a row finishes."),
                sending: .failed("amux.sh did not answer"), open: true)
        },
        page("sent") {
            ReportStore(
                capture: CatalogFixtures.frozenFleet,
                sending: .sent(ReportReceipt(id: "rep_4KX2M9", receivedAt: CatalogFixtures.now)), open: true)
        },
    ]

    private static func page(
        _ id: String, signedIn: Bool = true, store: @escaping @MainActor () -> ReportStore
    ) -> ComponentExample {
        ComponentExample(
            id: "report.\(id)", family: .report, canvas: CGSize(width: 390, height: 844)
        ) {
            ReportScreen(model: store(), signedIn: signedIn) { _ in }
        }
    }
}

extension CatalogFixtures {
    /// A frozen agent list to report on: drawn here rather than captured,
    /// so the picture is the same on every run.
    @MainActor static let frozenFleet: ReportCapture = {
        let size = CGSize(width: 390, height: 844)
        let renderer = ImageRenderer(content: FrozenFleetPicture().frame(width: size.width, height: size.height))
        renderer.scale = 2
        let png = renderer.uiImage?.pngData() ?? Data()
        return ReportCapture(frame: FrozenFrame(png: png, width: size.width, height: size.height, scale: 2))
    }()
}

/// The agent list as a report's frozen frame shows it: a title, a subtitle
/// and three rows, enough to read as the screen it stands for.
private struct FrozenFleetPicture: View {
    private let design = Design.app

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Agents")
                .designFont(.screenTitle, design)
                .padding(.top, 70)
            Text("No agents yet · 3 hosts")
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
                .padding(.top, 4)
                .padding(.bottom, 26)
            ForEach(["fix-flaky-ci", "relay-backoff", "docs-pass"], id: \.self) { name in
                VStack(alignment: .leading, spacing: 4) {
                    Text(name).designFont(.bodyEmphasis, design)
                    Text("desk · Working")
                        .designFont(.detail, design)
                        .foregroundStyle(design.inkMuted.color)
                }
                .padding(.vertical, 14)
                Rectangle().fill(design.hairline.color).frame(height: 1)
            }
            Spacer()
        }
        .foregroundStyle(design.ink.color)
        .padding(.horizontal, 20)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(design.ground.color)
        .environment(\.colorScheme, .dark)
    }
}

extension CatalogFixtures {
    static let ada = SignedInAccount(id: AccountId("ada"), email: "ada@example.com", displayName: "Ada")
    static let lovelace = SignedInAccount(id: AccountId("ada-work"), email: "ada@lovelace.dev", displayName: "Lovelace Labs")
    static let plans = [
        Plan(id: Plan.monthlyID, period: .monthly, price: "$4.99"),
        Plan(id: Plan.yearlyID, period: .yearly, price: "$49.99", saving: "Save 17%"),
    ]
}

extension CatalogFixtures {
    static let nearbyLaptop = machine(5, "laptop", platform: .macos, reach: .online(.direct), trusted: false, candidate: true, addrs: ["192.168.1.24:4433"], version: nil, providers: [])
    /// The laptop with its pairing window open, which raises the keypad.
    static let openLaptop = machine(5, "laptop", platform: .macos, reach: .online(.direct), trusted: false, candidate: true, addrs: ["192.168.1.24:4433"], pairingOpen: true, version: nil, providers: [])
    static let relayLaptop = machine(5, "laptop", platform: .macos, reach: .online(.relay), trusted: false, candidate: true, relayOnly: true, version: nil, providers: [])
    /// A machine on the account nobody paired, which only the relay reaches.
    static let accountServer = machine(7, "nova-test", platform: .linux, reach: .online(.unspecified), trusted: false, candidate: true, relayOnly: true, version: nil, providers: [])
    static let deskOffer = SignInOfferView(
        hostId: deskHost.hostId, hostName: "desk", email: "ada@example.com", accountName: "Ada")
    static let pendingLaptop = PendingPair(
        token: [1, 2, 3], hostId: nearbyLaptop.hostId, name: "laptop", fingerprint: fingerprint(5),
        expiresAtMs: Int64((now.timeIntervalSince1970 + 4 * 60 + 12) * 1000), via: .direct)
}

extension CatalogFixtures {
    static func machine(
        _ index: UInt8, _ name: String, platform: Platform?, reach: Reach, trusted: Bool = true,
        candidate: Bool = false, addrs: [String] = [], relayOnly: Bool = false, pairingOpen: Bool = false,
        version: String? = "0.8.0",
        providers: [ProviderSignIn] = [
            ProviderSignIn(provider: .claude, signedIn: true), ProviderSignIn(provider: .codex, signedIn: false),
        ]
    ) -> HostView {
        HostView(
            hostId: Array(repeating: index, count: 16), name: name, local: false, trusted: trusted,
            candidate: candidate, reach: reach, link: 0, addrs: addrs, relayOnly: relayOnly, pairingOpen: pairingOpen, current: false, providers: providers,
            claudeKind: nil, lastDialError: nil, platform: platform, signedIn: nil, through: nil, version: version)
    }

    static let deskHost = machine(1, "desk", platform: .macos, reach: .online(.direct), addrs: ["192.168.1.20:4433", "[fe80::1c2b:9aff:fe31:7d10]:4433"])
    static let oldLaptop = machine(6, "old-laptop", platform: .macos, reach: .offline(.revoked))
    static let machines: [HostView] = [
        deskHost,
        machine(2, "build-box", platform: .linux, reach: .online(.relay)),
        machine(3, "mini", platform: .macos, reach: .away),
        machine(4, "garage-pi", platform: .linux, reach: .offline(.plain)),
        oldLaptop,
        machine(5, "laptop", platform: .macos, reach: .online(.direct), trusted: false, candidate: true, addrs: ["192.168.1.24:4433"], version: nil, providers: []),
    ]

    static func account(pro: Bool) -> AccountView {
        AccountView(
            binding: .signedIn, email: "ada@example.com", name: "Ada", picture: "", relay: .connected, pro: pro)
    }

    static func hostsStore(_ hosts: [HostView]) -> HostsStore {
        let store = HostsStore(clock: { now })
        // Drawn as a phone that has been open a while: every machine has
        // answered or failed to, and none is still being dialled.
        store.stopWaiting()
        store.show(hosts)
        store.show(Roster(
            identity: Identity(hostId: Array(repeating: 9, count: 16), name: "Ada’s iPhone", fingerprint: fingerprint(9)),
            peers: hosts.filter(\.trusted).map {
                PairedPeer(hostId: $0.hostId, name: $0.name, fingerprint: fingerprint($0.hostId[0]))
            }))
        return store
    }

    /// A key's fingerprint as the runtime writes one: 64 hex characters.
    static func fingerprint(_ seed: UInt8) -> String {
        (0..<32).map { String(format: "%02x", UInt8(truncatingIfNeeded: Int(seed) &* 37 &+ $0 &* 11)) }.joined()
    }
}

// MARK: - Subagents

extension CatalogFixtures {
    /// An agent its parent's provider runs, as the fleet keys it on Studio.
    static func subagent(_ index: UInt8) -> AgentKey {
        AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: index, count: 16))
    }

    static func opened(_ index: UInt8, _ title: String, _ phase: PhaseView) -> OpenedAgent {
        OpenedAgent(agent: subagent(index), name: title, kind: .claudeSdk, phase: phase)
    }

    /// The Agent call that started subagent `agent`.
    static func call(
        _ id: String, _ order: UInt64, _ description: String, agent: UInt8, state: CallPhase = .succeeded,
        answer: String = ""
    ) -> Row {
        ScriptedChat.row(
            id, order, .subagent(description: description, continued: false, state: state, answer: answer, durationMs: nil),
            opens: subagent(agent).agent)
    }

    /// The four subagents the delegating turn starts, by where each stands.
    static let delegated: [(UInt8, String, PhaseView)] = [
        (21, "Survey the relay’s retry path", .working),
        (22, "Check the Windows build", .needsYou),
        (23, "Find every INVALID_PIN reader", .exited(cause: .finished)),
        (24, "Read the pairing spec", .working),
    ]

    static let delegatedState = GroupState(
        agents: 4, running: 2, needsYou: 1, idle: 0, finished: 1, ended: 0, failed: 0, unknown: 0)

    /// A turn that hands its work to four subagents at once, its calls
    /// folded into one group as the shared view folds them.
    static let delegating: [Row] = {
        func r(_ id: String, _ order: UInt64, _ kind: RowKind) -> Row { ScriptedChat.row(id, order, kind) }
        return conversation + [
            r("d01", 30, .prompt(text: [.text("Split the pairing audit four ways and report back.")], steered: false)),
            r("d02", 31, .thinking(text: "", open: false, durationMs: 3_000)),
            r("d03", 32, .prose(text: [.text("Starting four subagents, one per area.")], streaming: false, workingNote: false)),
        ] + ScriptedChat.group(delegated.enumerated().map { index, agent in
            call("d0\(index + 4)", 33 + UInt64(index), agent.1, agent: agent.0, state: .running)
        })
    }()

    /// The parent while its subagents work: what the fleet says of them
    /// comes with the chat.
    static func delegatingChat() -> ScriptedChat {
        ScriptedChat(
            rows: delegating,
            frame: ScriptedChat.frame(
                phase: .working,
                activity: Activity(kind: .subagents(count: 3), sinceMs: 0, elapsedMs: 41_000, turnElapsedMs: 41_000)),
            strip: ScriptedChat.strip(model: "opus 4.6"),
            agents: ChatAgents(
                agents: delegated.map { opened($0.0, $0.1, $0.2) },
                groups: [GroupView(first: "d04", state: delegatedState)],
                live: GroupState(
                    agents: 3, running: 2, needsYou: 1, idle: 0, finished: 0, ended: 0, failed: 0, unknown: 0)))
    }

    /// Subagent 21's own chat: the task its parent gave it, its thinking,
    /// the tools it ran and its answer so far.
    static let subagentWork: [Row] = {
        func r(_ id: String, _ order: UInt64, _ kind: RowKind) -> Row { ScriptedChat.row(id, order, kind) }
        return [
            r("s01", 1, .prompt(text: [.text("Survey the relay’s retry path: where a dropped link is retried, and what bounds it.")], steered: false)),
            r("s02", 2, .thinking(text: "The retry lives in the link layer, so start at the dialler.", open: false, durationMs: 2_000)),
        ] + ScriptedChat.run([
            r("s03", 3, .explore(verb: .search, subject: "retry_after", state: .succeeded, result: "")),
            r("s04", 4, .explore(verb: .read, subject: "crates/relay-core/src/dial.rs", state: .succeeded, result: "")),
            r("s05", 5, .command(command: "cargo test -p relay-core retry", description: "", state: .succeeded, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [], durationMs: 6_100, exitCode: 0)),
        ]) + [
            r("s06", 6, .prose(text: [.text("The dialler retries a dropped link with backoff capped at 30 s, and gives up after the link’s deadline. The cap is `MAX_BACKOFF` in dial.rs.")], streaming: false, workingNote: false)),
            r("s07", 7, .explore(verb: .read, subject: "crates/relay-core/src/link.rs", state: .running, result: "")),
        ]
    }()

    static func nativeFrame(_ stop: JobStop, phase: PhaseView = .working) -> ChatFrame {
        let working = phase == .working
        return ScriptedChat.frame(
            name: "sub-3f9a01c2", kind: .claudeSdk, phase: phase, mode: .native(stop: stop),
            activity: working
                ? Activity(kind: .running(key: "s07"), sinceMs: 0, elapsedMs: 19_000, turnElapsedMs: 19_000) : nil)
    }

    static let nativeSubject = ChatSubject(
        name: "Survey the relay’s retry path", host: "Studio", directory: "", reach: .online(.direct),
        topLevel: false, native: true)

    /// The parent the subagent's chat leads back to.
    static let nativeFamily = FamilyHeader(
        children: [], attention: .working, needYou: 0,
        parent: FleetCard(
            agent: AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: 2, count: 16)),
            name: "pairing-audit", handle: "pairing-audit", kind: .claudeSdk, attention: .working,
            cwd: "/Users/ada/src/amux", phaseSinceMs: 0, host: "Studio", hostReach: .online(.direct),
            children: 4, familyAttention: .needsYou, members: 5, descendants: 4, backgroundJobs: 0,
            closeHeld: false, filing: .open, topLevel: true, native: false, branch: nil, exitCause: nil,
            repository: nil))
}
