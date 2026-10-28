import AmuxCore
import AmuxDesign
import AmuxFeatures
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
    /// Whether the example takes one more change after it reports ready
    /// (an attachment chip replacing "Attaching", a focused field's caret
    /// appearing), so its picture has to wait for the screen to stop
    /// changing. Every other example is final once it reports ready.
    let settlesLate: Bool
    let build: @MainActor () -> AnyView

    init(
        id: String,
        family: Family,
        canvas: CGSize = CGSize(width: 390, height: 120),
        dynamicTypeSize: DynamicTypeSize = .large,
        readinessIdentifier: String? = nil,
        readinessValue: String? = nil,
        settlesLate: Bool = false,
        @ViewBuilder build: @escaping @MainActor () -> some View
    ) {
        self.id = id
        self.family = family
        self.canvas = canvas
        self.dynamicTypeSize = dynamicTypeSize
        self.readinessIdentifier = readinessIdentifier
        self.readinessValue = readinessValue
        self.settlesLate = settlesLate
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
                        rail: RailJoin.of(row, next: index + 1 < rows.count ? rows[index + 1] : nil),
                        bytes: CatalogFixtures.bytes)
                }
            }
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
            row("prompt-attachments", height: 190, r("p2", 2, .prompt(
                text: [.text("Here is the screen and the log."), .attachment(.image(F.photo)),
                       .attachment(.file(F.log)), .attachment(.text(name: "Pasted text", lines: 240, text: ""))],
                steered: false))),
            row("prompt-steered", r("p3", 3, .prompt(text: [.text("Use the new error table instead.")], steered: true))),
            row("prose", height: 330, readiness: true, r("s1", 4, .prose(
                text: [.text(F.markdown)], streaming: false, workingNote: false))),
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
                command: "cargo test -p amux-ui", state: .succeeded,
                outputHead: ["running 42 tests", "test pairing::copy ok", "test result: ok. 42 passed"],
                moreLines: 118, outputTrimmed: false, outputTail: [], durationMs: 4_200, exitCode: 0))),
            row("command-failed", height: 150, r("x2", 12, .command(
                command: "cargo check -p amux-ui", state: .failed,
                outputHead: ["error[E0308]: mismatched types"], moreLines: 214, outputTrimmed: false, outputTail: [], durationMs: 4_200,
                exitCode: 101))),
            row("command-denied", r("x3", 13, .command(
                command: "rm -rf target", state: .denied, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [], durationMs: nil,
                exitCode: nil),
                decision: Decision(outcome: .denied, elsewhere: false, granted: nil, note: "Use cargo clean instead"))),
            row("command-allowed", r("x4", 14, .command(
                command: "cargo test -p amux-ui", state: .succeeded, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [],
                durationMs: 12_000, exitCode: 0),
                decision: Decision(outcome: .allowed, elsewhere: false, granted: .session, note: nil))),
            row("command-allowed-always", r("x8", 14, .command(
                command: "cargo test -p amux-ui", state: .succeeded, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [],
                durationMs: 12_000, exitCode: 0),
                decision: Decision(outcome: .allowed, elsewhere: false, granted: .claude(
                    subjects: ["cargo test"], directories: [], mode: "", modeName: "", destination: .project), note: nil))),
            row("command-allowed-prefix", r("x9", 14, .command(
                command: "curl -s localhost:8080/health", state: .succeeded, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [],
                durationMs: 300, exitCode: 0),
                decision: Decision(outcome: .allowed, elsewhere: false, granted: .commandPrefix(words: ["curl", "-s"]), note: nil))),
            row("command-elsewhere", r("x5", 15, .toolCall(
                server: "github", tool: "create_issue", fact: "", state: .succeeded, result: ""),
                decision: Decision(outcome: .allowed, elsewhere: true, granted: nil, note: nil))),
            row("command-asking", r("x6", 16, .command(
                command: "cargo test -p amux-ui", state: .asking, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [],
                durationMs: nil, exitCode: nil), attention: true)),
            row("explore", r("e1", 17, .explore(verb: .read, subject: "crates/wire/src/codes.rs", state: .succeeded, result: ""))),
            row("run", r("e2", 18, .explore(verb: .search, subject: "INVALID_PIN", state: .succeeded, result: ""),
                run: Run(first: "e0", last: "e2", steps: 6, live: false, openBelow: false, unresolvedFailure: false, open: false, counts: RunCounts(commands: 0, edits: 0, reads: 4, searches: 2, subagents: 0, other: 0), recent: nil))),
            row("run-open-below", r("e3", 19, .explore(verb: .read, subject: "src/lib.rs", state: .succeeded, result: ""),
                run: Run(first: "e0", last: "e3", steps: 40, live: false, openBelow: true, unresolvedFailure: false, open: false, counts: RunCounts(commands: 0, edits: 0, reads: 40, searches: 0, subagents: 0, other: 0), recent: nil))),
            row("subagent-running", r("a1", 20, .subagent(
                description: "Find every INVALID_PIN reader", running: true, toolCount: 12,
                lastTool: "Read wire/src/codes.rs", answer: "", durationMs: 48_000))),
            row("subagent-done", height: 150, r("a2", 21, .subagent(
                description: "Find every INVALID_PIN reader", running: false, toolCount: 12, lastTool: "",
                answer: "Three readers: pairing.rs, codes.rs and the pairing spec. All read the same string.",
                durationMs: 51_000))),
            row("background", r("b1", 22, .background(command: "npm run dev", running: true, durationMs: nil))),
            row("image", height: 230, r("i1", 23, .image(path: "host-list-mock.png", generated: true, image: F.photo))),
            row("slash-output", height: 150, r("o1", 24, .slashOutput(
                command: "/cost", args: "", output: "Total cost: $0.42\nTotal duration: 18m 3s"))),
            row("turn-end", r("z1", 25, .turnEnd(failed: false, durationMs: 102_000))),
            row("stopped", r("z2", 26, .stopped)),
            row("compaction", r("z3", 27, .compaction(automatic: true, tokensAfter: 22_000, tokensBefore: 148_000))),
            row("error", height: 140, expanded: false, r("z4", 28, .error(
                errorKind: "Provider overloaded", message: "The API returned 529 ten times in a row.",
                attempts: 10, gaveUp: true))),
            row("model-switch", r("z5", 29, .modelSwitch(from: "opus", to: "sonnet 4.6", reason: "Opus declined this request"))),
            row("boundary", r("z6", 30, .boundary(kind: .resumed, cause: "after an update"))),
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
                    description: "Explore the pairing code", running: true, toolCount: 2,
                    lastTool: "Grep INVALID_PIN", answer: "", durationMs: 9_000)),
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
                path: "crates/amux-ui/src/pairing.rs", files: 1, added: 1, removed: 2, lines: F.diff,
                reason: "", created: false, line: 13), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .allowAlways(subjects: [], directories: [], mode: "acceptEdits", modeName: "Accept edits", scope: .session), primary: false, takesNote: false),
                    Choice(outcome: .deny(stops: false), primary: false, takesNote: true),
                ])),
            ask("permission-create", height: 420, F.card(.claudeSdk, .edit(
                path: "crates/amux-ui/src/pairing/refusals/every_reason_a_pairing_is_refused.rs",
                files: 1, added: 2, removed: 0, lines: F.created,
                reason: "", created: true, line: 1), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .deny(stops: false), primary: false, takesNote: true),
                ])),
            ask("permission-tool", height: 360, F.card(.claudeSdk, .tool(
                server: "github", tool: "create_issue",
                arguments: "{\n  \"repo\": \"jlw/amux\",\n  \"title\": \"Pairing copy\"\n}"), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .deny(stops: false), primary: false, takesNote: true),
                ])),
            ask("permission-terminal", height: 380, F.card(.claudePty, .command(
                command: "cargo test -p amux-ui", cwd: "", reason: "", description: ""), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .allowAlways(subjects: ["cargo test"], directories: [], mode: "", modeName: "", scope: .project), primary: false, takesNote: false),
                    Choice(outcome: .deny(stops: true), primary: false, takesNote: false),
                ])),
            ask("codex-command", height: 560, F.card(.codex, .command(
                command: "curl -s localhost:8080/health", cwd: "~/src/amux", reason: "Check the dev server",
                description: ""), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .allowForSession, primary: false, takesNote: false),
                    Choice(outcome: .allowSimilar(prefix: ["curl", "-s"]), primary: false, takesNote: false),
                    Choice(outcome: .allowNetwork(hosts: ["localhost"]), primary: false, takesNote: false),
                    Choice(outcome: .deny(stops: false), primary: false, takesNote: false),
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
                server: "linear", message: "Sign in to Linear to continue.", url: "https://linear.app/login"), [
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
                    Choice(outcome: .deny(stops: false), primary: false, takesNote: false),
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
        dockExpanded: Bool = false, refusedWords: [[UInt8]: String] = [:], settlesLate: Bool = false,
        subject: ChatSubject = CatalogFixtures.subject, size: DynamicTypeSize = .large,
        setUp: @escaping @MainActor (ChatModel) -> Void = { _ in }
    ) -> ComponentExample {
        ComponentExample(
            id: "composer.\(id)", family: .composer, canvas: CGSize(width: 390, height: height),
            dynamicTypeSize: size, settlesLate: settlesLate
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
                     draft: "Please tighten the retry path and keep the error visible.", settlesLate: true) {
                $0.attach(F.photoData, name: "screen.jpg", mime: "image/jpeg", image: true)
            },
            // Each file's name wraps rather than losing its extension, its
            // size under it, each remove mark a thumb's target. One after the
            // other: two attached together land in whichever order they finish.
            composer("attachments-large-text", height: 760, frame: ScriptedChat.frame(phase: .idle),
                     settlesLate: true, size: .accessibility3) { model in
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
            composer("resume", height: 200, frame: ScriptedChat.frame(phase: .exited(cause: .failed("code 1")), mode: .resume),
                     draft: "Pick up where you left off and rerun the tests.", settlesLate: true),
            composer("detached", height: 180, frame: ScriptedChat.frame(mode: .holds(.detached), caughtUp: false, waiting: .detached),
                     draft: "Also add a test for the new string."),
            composer("catching-up", height: 160, frame: ScriptedChat.frame(mode: .holds(.catchingUp), caughtUp: false, waiting: .catchingUp)),
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
            composer("review-token", height: 200, frame: ScriptedChat.frame(phase: .idle),
                     draft: "Please address these before the next run.", settlesLate: true) {
                $0.attach(F.writtenReview())
            },
            composer("strip-trouble", height: 220, frame: ScriptedChat.frame(), strip: ScriptedChat.strip(
                model: "gpt-5", usage: UsageView(blocked: false, windows: [UsageWindowView(label: .fiveHour, state: .nearLimit, resetsAtMs: nil, usedPercent: 91)], credits: nil),
                failedServers: [ServerView(name: "github", error: "exited", needsAuth: false)])),
            composer("sign-in", height: 240, frame: ScriptedChat.frame(), strip: ScriptedChat.strip(signIn: F.expiredSignIn)),
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
        ComponentExample(
            id: "chat.\(id)", family: .chat, canvas: CGSize(width: 390, height: height),
            dynamicTypeSize: size,
            // The system bar lays its items out a frame after the chat.
            readinessIdentifier: readiness?.0, readinessValue: readiness?.1, settlesLate: true
        ) {
            let source = chat()
            // In a navigation stack, as the app pushes it, so its bar is the
            // system's.
            NavigationStack {
                CatalogChat(source: source) { model in
                    ChatScreen(model: model, subject: subject, family: family, showing: showing) { _ in }
                        .onAppear { setUp(model, source) }
                }
            }
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
                ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(mode: .holds(.detached), caughtUp: false, waiting: .detached))
            }),
            chat("loading", readiness: ("chat.loading", "shown"), chat: {
                ScriptedChat(rows: [], frame: ScriptedChat.frame(mode: .holds(.catchingUp), caughtUp: false, waiting: .catchingUp))
            }),
            chat("away", chat: {
                ScriptedChat(rows: [], frame: ScriptedChat.frame(mode: .holds(.detached), caughtUp: false, waiting: .detached))
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
                    strip: ScriptedChat.strip(signIn: F.expiredSignIn))
            }),
            chat("new-activity", readiness: ("chat.newActivity", "shown"), chat: {
                ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(phase: .working))
            }) { model, source in
                model.reading(atNewest: false)
                source.append([ScriptedChat.row("n1", 100, .prose(text: [.text("Done. The three arms are one now.")], streaming: false, workingNote: false))])
                source.deliver(to: model)
            },
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
                source.working = F.review
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
            chat("stop", chat: { ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame()) }, showing: .stop),
            chat("stop-cuts", chat: {
                ScriptedChat(
                    rows: F.conversation, frame: ScriptedChat.frame(phase: .working),
                    strip: ScriptedChat.strip(background: 2))
            }, family: F.family, showing: .stop),
            chat("delete", chat: {
                ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(phase: .exited(cause: .ended)))
            }, showing: .delete),
            chat("delete-family", chat: {
                ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(phase: .exited(cause: .ended)))
            }, family: F.family, showing: .delete),
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
            id: id, family: family, canvas: CGSize(width: 390, height: 844),
            // The system bar lays its items out a frame after the page.
            settlesLate: true
        ) {
            NavigationStack { PagePlaceholder(showing, identifier: identifier) {} }
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
            overview("overview", settlesLate: true) { F.surroundings },
            overview("overview-stop", confirming: 0, settlesLate: true) { F.surroundings },
        ]
    }()

    /// `confirming` opens the stop question on that background job.
    private static func overview(
        _ id: String, comparison: Comparison = .uncommitted, confirming: Int? = nil,
        settlesLate: Bool = false,
        strip: @escaping @MainActor () -> ScriptedChat.Surroundings
    ) -> ComponentExample {
        ComponentExample(
            id: "chat.\(id)", family: .chat, canvas: CGSize(width: 390, height: 844),
            readinessIdentifier: "chat.overview.comparison", readinessValue: comparison.rawValue,
            settlesLate: settlesLate
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

    private static func review(
        _ id: String, height: CGFloat = 844, writing: Bool = false, editing: Int? = nil,
        setUp: @escaping @MainActor (ReviewModel) -> Void = { _ in }
    ) -> ComponentExample {
        ComponentExample(
            id: "review.\(id)", family: .review, canvas: CGSize(width: 390, height: height),
            // The system bar lays its items out a frame after the page.
            settlesLate: true
        ) {
            // In a navigation stack, as the app pushes it, so its bar is the
            // system's.
            NavigationStack {
                CatalogReview(setUp: setUp) { model in
                    ReviewPage(model: model, against: "Uncommitted", writing: writing, editing: editing) { _ in }
                }
            }
        }
    }

    fileprivate static let review: [ComponentExample] = {
        typealias F = CatalogFixtures
        return [
            review("page") { F.comment($0) },
            review("selection") { model in
                model.begin(at: ReviewLine(file: 0, hunk: 0, line: 2))
                model.extend(to: ReviewLine(file: 0, hunk: 0, line: 4))
            },
            review("comment", writing: true) { model in
                model.begin(at: ReviewLine(file: 0, hunk: 0, line: 2))
                model.extend(to: ReviewLine(file: 0, hunk: 0, line: 4))
            },
            review("editing", editing: 0) { F.comment($0) },
            review("folded") { model in
                F.comment(model)
                model.toggle(file: 0)
                model.toggle(file: 1)
            },
            placeholder("review.reading", family: .review, .waiting("Reading the changes"), identifier: "review"),
            placeholder(
                "review.no-changes", family: .review,
                .empty(title: "No changes", detail: "This agent has no uncommitted changes."),
                identifier: "review"),
            placeholder(
                "review.not-read", family: .review,
                .failed(title: "Couldn’t read the changes", reason: "desk did not answer"),
                identifier: "review"),
        ]
    }()
}

/// Holds one review's model for as long as the example is drawn.
private struct CatalogReview<Content: View>: View {
    @State private var model = ReviewModel(review: CatalogFixtures.review)
    let setUp: @MainActor (ReviewModel) -> Void
    let content: (ReviewModel) -> Content

    init(setUp: @escaping @MainActor (ReviewModel) -> Void, @ViewBuilder content: @escaping (ReviewModel) -> Content) {
        self.setUp = setUp
        self.content = content
        setUp(_model.wrappedValue)
    }

    var body: some View { content(model) }
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

    static let expiredSignIn = SignInView(state: .expired, account: "ada@example.com", message: "OAuth token has expired.")

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

    /// An agent's working tree: a changed file, another of the same name,
    /// and a new file.
    static let reviewPatch = """
        diff --git a/crates/amux-ui/src/pairing/errors.rs b/crates/amux-ui/src/pairing/errors.rs
        index 1111111..2222222 100644
        --- a/crates/amux-ui/src/pairing/errors.rs
        +++ b/crates/amux-ui/src/pairing/errors.rs
        @@ -8,11 +8,10 @@ use crate::codes::Code;
         /// Why a pairing did not happen.
         pub enum PairError {
             Expired,
        -    Refused,
        -    Unknown,
        -    Busy,
        +    Refused(String),
        +    Busy { retry_after_ms: u64 },
         }
         
         impl PairError {
        -    pub fn message(&self) -> &str {
        +    pub fn message(&self) -> String {
                 match self {
        diff --git a/crates/amux-ui/tests/errors.rs b/crates/amux-ui/tests/errors.rs
        index 3333333..4444444 100644
        --- a/crates/amux-ui/tests/errors.rs
        +++ b/crates/amux-ui/tests/errors.rs
        @@ -1,4 +1,5 @@
         use amux_ui::pairing::PairError;
        +use amux_ui::pairing::describe;
         
         #[test]
         fn every_error_has_one_string() {
        diff --git a/docs/PAIRING.md b/docs/PAIRING.md
        new file mode 100644
        index 0000000..5555555
        --- /dev/null
        +++ b/docs/PAIRING.md
        @@ -0,0 +1,3 @@
        +# Pairing
        +
        +A refused pairing now says why.

        """

    /// The catalogue's clock, where a view reads ages and resets against it.
    static let now = Date(timeIntervalSince1970: 1_790_000_000)

    static let git = Git(
        baseBranch: "main", branch: "refactor-auth",
        onBranch: ChangeTotals(files: 6, added: 120, removed: 30),
        uncommitted: ChangeTotals(files: 5, added: 56, removed: 20))

    static let changes = Changes(
        totals: ChangeTotals(files: 5, added: 56, removed: 20),
        folders: [
            Folder(path: "", files: [
                ChangedFile(path: "README.md", name: "README.md", added: 2, removed: 0, status: .modified, binary: false),
            ]),
            Folder(path: "crates/amux-ui/src/", files: [
                ChangedFile(path: "crates/amux-ui/src/errors.rs", name: "errors.rs", added: 20, removed: 0, status: .added, binary: false),
                ChangedFile(path: "crates/amux-ui/src/pairing.rs", name: "pairing.rs", added: 25, removed: 8, status: .modified, binary: false),
                ChangedFile(path: "crates/amux-ui/src/strip.rs", name: "strip.rs", added: 0, removed: 12, status: .deleted, binary: false),
            ]),
            Folder(path: "docs/", files: [
                ChangedFile(path: "docs/PAIRING.md", name: "PAIRING.md", added: 9, removed: 0, status: .added, binary: false),
            ]),
        ])

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

    static let review = FrozenReview(
        diff: Diff(
            head: "4f2a9c1", files: [], base: DiffBase(base: .head(Empty())), mergeBase: nil,
            patch: BlobRef(hash: [5, 5, 5], name: "patch", mime: "text/x-diff", size: UInt64(reviewPatch.utf8.count))),
        patch: reviewPatch)

    /// Two comments: one on a changed line, one on a new file's line.
    static func comment(_ model: ReviewModel) {
        model.begin(at: ReviewLine(file: 0, hunk: 0, line: 5))
        model.comment("Say what refused it, not only that it was refused.")
        model.begin(at: ReviewLine(file: 2, hunk: 0, line: 2))
        model.comment("Link the error table here.")
    }

    static func writtenReview() -> ReviewModel {
        let model = ReviewModel(review: review)
        comment(model)
        return model
    }

    static let photo = BlobRef(hash: [7, 7, 7], name: "screen.jpg", mime: "image/jpeg", size: 184_320)
    static let log = BlobRef(hash: [8, 8, 8], name: "pairing.log", mime: "text/plain", size: 12_288)
    static let images: [[UInt8]: Data] = [photo.hash: photoData, Array("screen.jpg".utf8): photoData]

    static func bytes(_ blob: BlobRef) -> Data? { images[blob.hash] }

    static let subject = ChatSubject(
        name: "refactor-auth", host: "Studio", directory: "/Users/ada/src/amux", branch: "ada/refactor-auth",
        reach: .online(.direct))
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

    /// A plan as the shared view hands it: the title lifted from its
    /// opening heading, and the rest.
    static let planTitle = "Collapse the pairing failures onto one message"
    static let planBody = """
        The client maps gRPC statuses onto distinct strings in three places. The protocol refuses \
        to distinguish them, so the client must not either.

        1. One arm, one string
        2. Update the three specs
        """

    /// An edit's patch as the shared view hands it: where its hunk starts,
    /// then its lines.
    static let diff: [EditLine] = [
        .hunk(oldStart: 12, newStart: 12),
        .line(PatchLine(kind: .context, text: "  match status {", number: 12)),
        .line(PatchLine(kind: .removed, text: "      Code::NotFound => \"no such host\",", number: 13)),
        .line(PatchLine(kind: .removed, text: "      Code::InvalidPin => \"wrong PIN\",", number: 14)),
        .line(PatchLine(kind: .added, text: "      _ => \"could not pair, check the code\",", number: 13)),
        .line(PatchLine(kind: .context, text: "  }", number: 14)),
    ]

    /// A new file's whole body, every line added.
    static let created: [EditLine] = [
        .hunk(oldStart: 0, newStart: 1),
        .line(PatchLine(kind: .added, text: "//! Every reason a pairing is refused.", number: 1)),
        .line(PatchLine(kind: .added, text: "pub use super::Code;", number: 2)),
    ]

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
        Choice(outcome: .deny(stops: false), primary: false, takesNote: true),
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
        command: "cargo test -p amux-ui", state: .running, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [],
        durationMs: nil, exitCode: nil))

    static let askedCommand = ScriptedChat.row("x6", 60, .command(
        command: "cargo test -p amux-ui", state: .asking, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [],
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
            .command(command: command, state: state, outputHead: output, moreLines: more, outputTrimmed: false, outputTail: [], durationMs: ms, exitCode: exit)
        }
        func edit(_ path: String, _ added: UInt32, _ removed: UInt32) -> RowKind {
            .fileChange(files: [FileRow(path: path, change: .edited, added: added, removed: removed, line: nil)], state: .succeeded)
        }
        return [
            r("c01", 1, .boundary(kind: .started, cause: "")),
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
            r("w04", 23, .command(command: "cargo clippy -p amux-ui", state: .succeeded, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [], durationMs: 5_800, exitCode: 0)),
            r("w05", 24, .explore(verb: .search, subject: "pairing_copy", state: .succeeded, result: "")),
            r("w06", 25, .command(command: "cargo test --workspace", state: .running, outputHead: [], moreLines: 0, outputTrimmed: false, outputTail: [], durationMs: nil, exitCode: nil)),
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
        name: "docs-sweep", kind: .claudeSdk, attention: .exited, cwd: "", phaseSinceMs: 0,
        host: "Studio", hostReach: .online(.direct), children: 0, familyAttention: .exited,
        members: 1, descendants: 0, backgroundJobs: 0, stopHeld: false, branch: nil, exitCause: .finished)

    static let family = FamilyHeader(
        children: [
            FleetCard(agent: AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: 3, count: 16)),
                      name: "worker-2", kind: .claudeSdk, attention: .needsYou, cwd: "", phaseSinceMs: 0,
                      host: "Laptop", hostReach: .online(.direct), children: 0, familyAttention: .needsYou,
                      members: 1, descendants: 0, backgroundJobs: 0, stopHeld: false, branch: nil, exitCause: nil),
            FleetCard(agent: AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: 4, count: 16)),
                      name: "worker-3", kind: .codex, attention: .working, cwd: "", phaseSinceMs: 0,
                      host: "Studio", hostReach: .online(.direct), children: 0, familyAttention: .working,
                      members: 1, descendants: 0, backgroundJobs: 0, stopHeld: false, branch: nil, exitCause: nil),
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
            store.listed(
                .success(Directories(recent: [], repositories: [], roots: ["~/src"])), asked: store.asking())
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
            readinessValue: browsing ? "" : "on",
            // The system bar lays its items out a frame after the form.
            settlesLate: true
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
        hosts.show([HostView(
            hostId: desk.bytes, name: "desk", local: false, trusted: true, candidate: false,
            reach: .online(.direct), addrs: [], current: true,
            providers: [
                ProviderSignIn(provider: .claude, signedIn: signedIn),
                ProviderSignIn(provider: .codex, signedIn: true),
            ],
            lastDialError: nil, platform: .macos, signedIn: nil, version: nil),
        HostView(
            hostId: Array(repeating: 8, count: 16), name: "laptop", local: false, trusted: true,
            candidate: false, reach: .away, addrs: [], current: false, providers: [],
            lastDialError: nil, platform: .macos, signedIn: nil, version: nil)])
        let store = NewAgentStore()
        store.open(on: desk)
        store.listed(.success(Directories(
            recent: [
                Directory(path: "~/src/amux", name: "amux", lastUsedMs: 5),
                Directory(path: "~/src/amuxcloud", name: "amuxcloud", lastUsedMs: 4),
                Directory(path: "~/src/promo", name: "promo", lastUsedMs: 3),
            ],
            repositories: [
                Directory(path: "~/src/dotfiles", name: "dotfiles", lastUsedMs: 0),
                Directory(path: "~/src/skills", name: "skills", lastUsedMs: 0),
            ], roots: ["~/src"])), asked: store.asking())
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
        skills: [])

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
        skills: [])
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
        ComponentExample(id: "host.\(id)", family: .hosts, canvas: CGSize(width: 390, height: 844)) {
            hostPageView(host: host)
        }
    }

    /// One machine's page as the hosts tab pushes it.
    static func hostPageView(host: HostView, confirming: Bool = false) -> some View {
        NavigationStack {
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
    /// code that did not work, and one that reached a machine only the relay
    /// can carry; then an invitation link waiting, refused, and naming a
    /// machine that needs an account.
    fileprivate static let pairing: [ComponentExample] = [
        code("empty") { _ in },
        code("typing") { $0.enter("418") },
        code("checking") { store in
            store.enter("418207")
            _ = store.checking()
        },
        code("refused") { store in
            store.enter("418207")
            store.reached(.failure(RuntimeFailure("wrong secret")), attempt: store.checking())
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
            store.paired(.success(CatalogFixtures.pairedLaptop), attempt: attempt)
        },
        confirm("link-checking", machine: nil) { _ = $0.checking() },
        confirm("link-refused", machine: nil) { store in
            store.reached(.failure(RuntimeFailure("expired")), attempt: store.checking())
        },
        ComponentExample(id: "pair.needs-account", family: .pairing, canvas: CGSize(width: 390, height: 844)) {
            NavigationStack { PairNeedsAnAccount { _ in } }
        },
    ]

    /// The code in the system's number pad, as the pairing sheet asks for it
    /// for one machine. The field takes focus once drawn and the pad rises
    /// after, so the picture waits for the screen to stop changing.
    private static func code(
        _ id: String, machine: HostView = CatalogFixtures.nearbyLaptop,
        setUp: @escaping @MainActor (PairingStore) -> Void
    ) -> ComponentExample {
        ComponentExample(
            id: "pair.code-\(id)", family: .pairing, canvas: CGSize(width: 390, height: 844), settlesLate: true
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
        ComponentExample(id: "pair.\(id)", family: .pairing, canvas: CGSize(width: 390, height: 844)) {
            let store = PairingStore(clock: { CatalogFixtures.now })
            store.open(machine: machine)
            setUp(store)
            return NavigationStack { PairConfirmation(model: store) { _ in } }
        }
    }
}

extension ComponentCatalog {
    // MARK: - Settings

    /// The settings sheet with two accounts and with none, and every page it
    /// opens: signing in, subscribing, and deleting an account. Removing one
    /// asks in the system's dialog, which a flat capture cannot hold; it has
    /// a preview of its own.
    fileprivate static let settings: [ComponentExample] = [
        sheet("two-accounts") { StoryApp(settingsOpen: true) { StoryFleet.busy(accounts: 2) }.settings },
        sheet("signed-out") { StoryApp(settingsOpen: true) { StoryFleet.busy(accounts: 0) }.settings },
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
        sheet("sign-in") { SignIn(model: SignInStore()) { _ in } },
        sheet("sign-in-waiting") { SignIn(model: SignInStore(phase: .handingOff)) { _ in } },
        sheet("sign-in-failed") {
            SignIn(model: SignInStore(phase: .failed("amux.sh did not answer"))) { _ in }
        },
        sheet("sign-in-mismatched") {
            SignIn(
                model: SignInStore(
                    phase: .mismatched(wanted: CatalogFixtures.ada, got: CatalogFixtures.lovelace),
                    intent: .returning(CatalogFixtures.ada))) { _ in }
        },
        sheet("paywall") { Paywall(model: PaywallStore(plans: CatalogFixtures.plans)) { _ in } },
        sheet("paywall-confirming") {
            Paywall(model: PaywallStore(plans: CatalogFixtures.plans, phase: .confirming)) { _ in }
        },
        sheet("paywall-unconfirmed") {
            Paywall(
                model: PaywallStore(plans: CatalogFixtures.plans, phase: .unconfirmed(.unreachable))) { _ in }
        },
        sheet("paywall-subscribed") {
            Paywall(
                model: PaywallStore(
                    entitlement: .active(grant: .purchased(.appStore), renews: CatalogFixtures.now.addingTimeInterval(86_400 * 300)),
                    plans: CatalogFixtures.plans, phase: .entitled(.purchased(.appStore)))) { _ in }
        },
    ]

    private static func sheet(
        _ id: String, size: DynamicTypeSize = .large, height: CGFloat = 844,
        build: @escaping @MainActor () -> some View
    ) -> ComponentExample {
        ComponentExample(
            id: "settings.\(id)", family: .settings, canvas: CGSize(width: 390, height: height), dynamicTypeSize: size
        ) {
            NavigationStack { build() }
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
            id: "report.\(id)", family: .report, canvas: CGSize(width: 390, height: 844),
            // The system bar lays its items out a frame after the page.
            settlesLate: true
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
    static let nearbyLaptop = machine(5, "laptop", platform: .macos, reach: .online(.direct), trusted: false, candidate: true, version: nil, providers: [])
    static let relayLaptop = machine(5, "laptop", platform: .macos, reach: .online(.relay), trusted: false, candidate: true, version: nil, providers: [])
    static let pendingLaptop = PendingPair(
        token: [1, 2, 3], hostId: nearbyLaptop.hostId, name: "laptop", fingerprint: fingerprint(5),
        expiresAtMs: Int64((now.timeIntervalSince1970 + 4 * 60 + 12) * 1000), via: .direct)
    /// The runtime's answer to a confirmed pairing, which only it makes.
    static let pairedLaptop = try! JSONDecoder().decode(
        Paired.self,
        from: JSONSerialization.data(withJSONObject: [
            "host_id": nearbyLaptop.hostId.map(Int.init), "name": nearbyLaptop.name,
        ]))
}

extension CatalogFixtures {
    static func machine(
        _ index: UInt8, _ name: String, platform: Platform?, reach: Reach, trusted: Bool = true,
        candidate: Bool = false, addrs: [String] = [], version: String? = "0.8.0",
        providers: [ProviderSignIn] = [
            ProviderSignIn(provider: .claude, signedIn: true), ProviderSignIn(provider: .codex, signedIn: false),
        ]
    ) -> HostView {
        HostView(
            hostId: Array(repeating: index, count: 16), name: name, local: false, trusted: trusted,
            candidate: candidate, reach: reach, addrs: addrs, current: false, providers: providers,
            lastDialError: nil, platform: platform, signedIn: nil, version: version)
    }

    static let deskHost = machine(1, "desk", platform: .macos, reach: .online(.direct), addrs: ["192.168.1.20:4433", "[fe80::1c2b:9aff:fe31:7d10]:4433"])
    static let oldLaptop = machine(6, "old-laptop", platform: .macos, reach: .offline(.revoked))
    static let machines: [HostView] = [
        deskHost,
        machine(2, "build-box", platform: .linux, reach: .online(.relay)),
        machine(3, "mini", platform: .macos, reach: .away),
        machine(4, "garage-pi", platform: .linux, reach: .offline(.plain)),
        oldLaptop,
        machine(5, "laptop", platform: .macos, reach: .online(.direct), trusted: false, candidate: true, version: nil, providers: []),
    ]

    static func hostsStore(_ hosts: [HostView]) -> HostsStore {
        let store = HostsStore(clock: { now })
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
