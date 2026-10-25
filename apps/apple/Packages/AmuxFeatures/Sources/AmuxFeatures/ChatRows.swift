import AmuxCore
import AmuxDesign
import ImageIO
import SwiftUI

/// One chat row, drawn from its value.
///
/// Work and decisions sit on one grid: a glyph cell, then a line of verb,
/// subject in mono and meta on the trailing edge, then what the row says
/// underneath. Consecutive grid rows hang off one rail drawn in the glyph
/// column, so a turn's work reads as one run. The person's prompts sit in a
/// bubble, the agent's prose takes the full width, and session events are
/// rules and quiet footers. The accent is kept for the glyph of a row an open
/// ask points at, a failure and a refusal. A row that opens says so with a
/// chevron and opens in place.
public struct ChatRowView: View {
    @Environment(\.design) private var design
    let row: Row
    let expanded: Bool
    let rail: RailJoin
    let placement: RunPlacement
    let bytes: (BlobRef) -> Data?
    let toggle: () -> Void
    let toggleRun: () -> Void

    public init(
        row: Row, expanded: Bool, rail: RailJoin = .none, placement: RunPlacement = .own,
        bytes: @escaping (BlobRef) -> Data? = { _ in nil }, toggle: @escaping () -> Void = {},
        toggleRun: @escaping () -> Void = {}
    ) {
        self.row = row
        self.expanded = expanded
        self.rail = rail
        self.placement = placement
        self.bytes = bytes
        self.toggle = toggle
        self.toggleRun = toggleRun
    }

    public var body: some View {
        content.frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private var content: some View {
        switch placement {
        case .own:
            kindView
        case .step(let header):
            VStack(alignment: .leading, spacing: 0) {
                runLine(header.words, open: header.open, rail: RailJoin(continues: true, nested: rail.nested))
                kindView
            }
        case .folded(let words):
            VStack(alignment: .leading, spacing: 0) {
                runLine(
                    words, open: false,
                    rail: row.run?.unresolvedFailure == true
                        ? RailJoin(continues: true, nested: rail.nested) : rail)
                // The run's newest step failed and nothing fixed it: it
                // stays in view under the line, where it ran.
                if row.run?.unresolvedFailure == true {
                    kindView.environment(\.unresolvedFailure, true)
                }
            }
        case .failed:
            kindView.environment(\.unresolvedFailure, true)
        }
    }

    /// A run's one line: folded to what its steps did, or the line above an
    /// open or live run's steps that folds it again.
    private func runLine(_ words: String, open: Bool, rail: RailJoin) -> some View {
        GridRow(
            kind: "run", glyph: open ? "chevron.down" : "chevron.right", rail: rail, verb: words,
            quiet: true, opens: true, open: open, trailingChevron: false, toggle: toggleRun)
    }

    @ViewBuilder
    private var kindView: some View {
        switch row.kind {
        case .prompt(let text, let steered):
            PromptBubble(text: text, steered: steered, bytes: bytes).padding(.bottom, RowGrid.prose)
        case .prose(let text, let streaming, let workingNote):
            ProseRow(text: text, streaming: streaming, workingNote: workingNote)
                .padding(.bottom, RowGrid.prose)
        case .thinking(let text, let open, let durationMs):
            GridRow(
                kind: "thinking", glyph: "ellipsis", rail: rail,
                verb: ChatWords.thinking(open: open, durationMs: durationMs), quiet: true,
                detail: expanded && !text.isEmpty ? text : nil, detailFace: .text,
                opens: !text.isEmpty, open: expanded, toggle: toggle)
        case .toolCall(let server, let tool, let fact, let state, let result):
            let verb = ChatWords.verb(
                state, row, wants: String(localized: "Wants to use"),
                doing: String(localized: "Using"), done: String(localized: "Used"))
            GridRow(
                kind: "tool", glyph: glyph(state, "wrench.adjustable"), accented: accented(state), failed: state == .failed,
                rail: rail, verb: verb, subject: server.isEmpty ? tool : "\(server) · \(tool)",
                meta: RowMeta(ChatWords.meta(
                    [fact, state == .failed ? String(localized: "failed") : ""], row,
                    verbPhase: state, note: false)),
                quote: denialNote,
                detail: expanded && !result.isEmpty ? result : nil,
                opens: !result.isEmpty, open: expanded, toggle: toggle)
        case .child(let action, let name, let host, let kind, let state, let result):
            // "Started payment-tests on build · Codex", as the terminal
            // says it: the machine only when it is another one.
            let verb = switch (action, state) {
            case (.start, .failed): String(localized: "Could not start")
            case (.stop, .failed): String(localized: "Could not stop")
            case (.start, _): ChatWords.verb(
                state, row, wants: String(localized: "Wants to start"),
                doing: String(localized: "Starting"), done: String(localized: "Started"))
            case (.stop, _): ChatWords.verb(
                state, row, wants: String(localized: "Wants to stop"),
                doing: String(localized: "Stopping"), done: String(localized: "Stopped"))
            }
            let who = name.isEmpty ? String(localized: "a subagent") : name
            GridRow(
                kind: "child", glyph: glyph(state, action == .start ? "person.badge.plus" : "person.badge.minus"),
                accented: accented(state), failed: state == .failed,
                rail: rail, verb: verb, subject: host.isEmpty ? who : String(localized: "\(who) on \(host)"),
                meta: RowMeta(ChatWords.meta([kind], row, verbPhase: state, note: false)),
                quote: denialNote,
                detail: expanded && !result.isEmpty ? result : nil,
                opens: !result.isEmpty, open: expanded, toggle: toggle)
        case .fileChange(let files, let state):
            fileChange(files, state)
        case .command(
            let command, let state, let outputHead, let moreLines, let outputTrimmed, _, let durationMs, let exitCode):
            let verb = ChatWords.verb(
                state, row, wants: String(localized: "Wants to run"),
                doing: String(localized: "Running"), done: String(localized: "Ran"))
            GridRow(
                kind: "command", glyph: glyph(state, "chevron.left.forwardslash.chevron.right"),
                accented: accented(state), failed: state == .failed, rail: rail, verb: verb,
                subject: ChatWords.firstLine(command),
                meta: RowMeta(ChatWords.meta(
                    commandMeta(state, exitCode: exitCode, durationMs: durationMs), row,
                    verbPhase: state, note: false)),
                quote: denialNote, output: outputHead, trimmed: outputTrimmed, more: moreLines)
        case .explore(let verb, let subject, let state, let result):
            // It opens what it found, which is kept even when the step failed.
            GridRow(
                kind: "explore", glyph: glyph(state, "magnifyingglass"), accented: accented(state), failed: state == .failed,
                rail: rail, verb: ChatWords.explore(verb), subject: subject,
                meta: RowMeta(ChatWords.meta(
                    [ChatWords.state(state, decided: row.decision != nil) ?? ""], row, note: false)),
                truncation: .head, quote: denialNote,
                detail: expanded && !result.isEmpty ? result : nil,
                opens: !result.isEmpty, open: expanded, toggle: toggle)
        case .subagent(let description, let running, let toolCount, let lastTool, let answer, let durationMs):
            GridRow(
                kind: "subagent", glyph: "arrow.triangle.branch", rail: rail,
                verb: String(localized: "Agent"), subject: ChatWords.firstLine(description),
                meta: RowMeta(ChatWords.subagent(toolCount: toolCount, durationMs: durationMs)),
                truncation: .tail,
                note: running && !lastTool.isEmpty ? "└ \(lastTool)" : nil,
                detail: !running && !answer.isEmpty ? answer : nil, detailFace: .text,
                detailLines: expanded ? nil : 2, opens: true, open: expanded, toggle: toggle)
        case .background(let command, let running, _):
            GridRow(
                kind: "background", glyph: "play", rail: rail,
                verb: String(localized: "In background"), subject: ChatWords.firstLine(command),
                meta: RowMeta(running ? String(localized: "running") : String(localized: "finished")))
        case .image(let path, let generated, let image):
            GridRow(
                kind: "image", glyph: "photo", rail: rail,
                verb: generated ? String(localized: "Generated image") : String(localized: "Image"),
                subject: path.isEmpty ? image?.name ?? "" : path,
                meta: RowMeta(image.map { ChatWords.bytes($0.size) } ?? ""),
                below: image.flatMap { Thumbnail.image(bytes($0)) }.map { picture in
                    AnyView(picture.resizable().scaledToFit()
                        .frame(maxWidth: 240, maxHeight: 240, alignment: .leading)
                        .clipShape(RoundedRectangle(cornerRadius: design.metrics.controlRadius))
                        .padding(.top, 4))
                })
        case .slashOutput(let command, let args, let output):
            GridRow(
                kind: "slash", glyph: "slash.circle", rail: rail, verb: command, subject: args,
                detail: output.isEmpty ? nil : output, detailLines: expanded ? nil : 6,
                opens: !output.isEmpty, open: expanded, toggle: toggle)
        case .ask(let ask):
            AskRowView(ask: ask, rail: rail, expanded: expanded, toggle: toggle)
        case .turnEnd(let failed, let costUsd, let durationMs):
            Footer(
                kind: "turn-end",
                text: ChatWords.turnEnd(durationMs: durationMs, costUsd: costUsd, failed: failed),
                failed: failed)
        case .stopped:
            GridRow(
                kind: "stopped", glyph: "stop", rail: rail,
                verb: String(localized: "You stopped it"), quiet: true)
        case .compaction(let automatic, let after, let before):
            FeedRule(
                kind: "compaction",
                label: ChatWords.compaction(before: before, after: after, automatic: automatic))
        case .error(let errorKind, let message, let attempts, let gaveUp):
            let detailed = !errorKind.isEmpty && !message.isEmpty
            GridRow(
                kind: "error", glyph: "exclamationmark.triangle", failed: true, rail: rail,
                verb: errorKind.isEmpty ? ChatWords.firstLine(message) : errorKind,
                meta: RowMeta(gaveUp && attempts > 1
                    ? String(localized: "gave up after \(attempts) tries") : ""),
                detail: detailed ? message : nil, detailLines: expanded ? nil : 3,
                opens: detailed, open: expanded, toggle: toggle)
        case .modelSwitch(_, let to, let reason):
            GridRow(
                kind: "model-switch", glyph: "arrow.left.arrow.right", rail: rail,
                verb: String(localized: "Switched to"), subject: to,
                note: reason.isEmpty ? nil : reason)
        case .boundary(let kind, let cause):
            FeedRule(kind: "boundary", label: ChatWords.boundary(kind, cause: cause))
        case .agentMessage(let from, let kind, let text, let to, let sent, let rejection):
            GridRow(
                kind: "agent-message",
                glyph: to.isEmpty ? "arrow.turn.down.left" : "arrow.turn.up.right",
                failed: sent == .rejected, rail: rail,
                verb: to.isEmpty ? String(localized: "From") : String(localized: "To"),
                subject: to.isEmpty ? from : to, meta: RowMeta(messageMeta(kind, sent, rejection)),
                detail: ChatWords.text(of: text).trimmingCharacters(in: .whitespacesAndNewlines),
                detailFace: .text,
                detailLines: expanded ? nil : 1, opens: true, open: expanded, toggle: toggle)
        case .autoReview(let status, let statusWord, let risk, let rationale, _):
            GridRow(
                kind: "auto-review", glyph: "checkmark.shield", rail: rail,
                verb: ChatWords.autoReview(status, statusWord),
                meta: RowMeta(risk.isEmpty ? "" : String(localized: "\(risk) risk")),
                detail: expanded && !rationale.isEmpty ? rationale : nil, detailFace: .text,
                opens: !rationale.isEmpty, open: expanded, toggle: toggle)
        case .unrecognized(let what, let summary):
            GridRow(
                kind: "unrecognized", glyph: "questionmark.square.dashed", rail: rail,
                verb: String(localized: "Unreadable"), subject: what,
                note: summary.isEmpty ? nil : ChatWords.firstLine(summary))
        case .hidden:
            EmptyView()
        }
    }

    /// Edited, created, deleted and moved files: one grid row each, joined to
    /// one another on the rail.
    @ViewBuilder
    private func fileChange(_ files: [FileRow], _ state: CallPhase) -> some View {
        let tail = [ChatWords.state(state, decided: row.decision != nil) ?? ""]
        if files.isEmpty {
            GridRow(
                kind: "file-change", glyph: glyph(state, "plusminus"), accented: accented(state), failed: state == .failed,
                rail: rail, verb: String(localized: "Editing"),
                meta: RowMeta(ChatWords.meta(tail, row, note: false)), quote: denialNote)
        }
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(files.enumerated()), id: \.offset) { index, file in
                let last = index == files.count - 1
                let extra = index == 0 ? ChatWords.meta(tail, row, note: false) : ""
                let words = Self.words(file)
                GridRow(
                    kind: "file-change", glyph: glyph(state, words.glyph),
                    accented: accented(state), failed: state == .failed,
                    rail: last ? rail : RailJoin(continues: true, nested: rail.nested),
                    verb: words.verb, subject: words.subject,
                    subjectFace: words.verb.isEmpty ? .path : .mono,
                    meta: words.meta.then(extra), truncation: .head,
                    quote: index == 0 ? denialNote : nil)
            }
        }
    }

    /// An edit is its path and its counts, as the diff names it; the other
    /// changes say what happened first.
    private static func words(_ file: FileRow) -> (glyph: String, verb: String, subject: String, meta: RowMeta) {
        switch file.change {
        case .edited:
            ("plusminus", "", file.path, .change(added: file.added, removed: file.removed))
        case .created(let lines):
            ("square.and.pencil", String(localized: "Created"), file.path,
             RowMeta(ChatWords.lines(lines)))
        case .deleted:
            ("trash", String(localized: "Deleted"), file.path, RowMeta())
        case .moved(let to):
            ("arrow.right", String(localized: "Moved"), "\(file.path) → \(to)", RowMeta())
        case .writing(let lines):
            ("square.and.pencil", String(localized: "Writing"), file.path,
             RowMeta(ChatWords.lines(lines)))
        }
    }

    /// A refused call's mark is the hand; one asking the person waits on
    /// the same hand. Otherwise the kind's own glyph, in the accent when it failed.
    private func glyph(_ phase: CallPhase, _ done: String) -> String {
        switch phase {
        case .denied, .asking: "hand.raised"
        case .cancelled: "nosign"
        case .pending, .running, .succeeded, .failed: done
        }
    }

    /// The accent is for what waits on the person: a step asking. A step
    /// that failed says so in the removed ink instead, where its glyph is
    /// drawn.
    private func accented(_ phase: CallPhase) -> Bool {
        row.attention && phase == .asking
    }

    /// What the person said when they refused, under the row in their words.
    private var denialNote: String? {
        guard let decision = row.decision, decision.outcome == .denied,
              let note = decision.note, !note.isEmpty
        else { return nil }
        return ChatWords.firstLine(note)
    }

    /// "exit 101 · 4.2s": a failure names its code where the agent gives one.
    private func commandMeta(_ state: CallPhase, exitCode: Int32?, durationMs: Int64?) -> [String] {
        var meta: [String] = []
        if let exitCode, exitCode != 0 {
            meta.append(String(localized: "exit \(exitCode)"))
        } else if exitCode == nil, state == .failed {
            meta.append(String(localized: "failed"))
        }
        if let durationMs { meta.append(ChatWords.duration(durationMs)) }
        return meta
    }

    private func messageMeta(_ kind: EnvelopeKind, _ sent: SendState, _ rejection: String) -> String {
        switch (kind, sent) {
        case (.finished, _): String(localized: "finished")
        case (.failed, _): String(localized: "failed")
        case (_, .sending): String(localized: "sending")
        case (_, .rejected): String(localized: "not delivered · \(rejection)")
        default: ""
        }
    }
}

/// How a grid row joins the rail: whether the next row drawn below it is on
/// the rail too, and whether it is a subagent's step, set in under its
/// parent's text.
public struct RailJoin: Equatable, Sendable {
    public var continues = false
    public var nested = false

    public init(continues: Bool = false, nested: Bool = false) {
        self.continues = continues
        self.nested = nested
    }

    public static let none = RailJoin()

    /// The join for `row` with `next` drawn below it.
    public static func of(_ row: Row, next: Row?) -> RailJoin {
        guard onRail(row) else { return .none }
        return RailJoin(continues: next.map(onRail) ?? false, nested: row.parent != nil)
    }

    /// Everything drawn on the grid hangs off the rail; what is read rather
    /// than scanned — the prompt, the prose — and the rules and footers that
    /// close a turn break it.
    public static func onRail(_ row: Row) -> Bool {
        switch row.kind {
        case .prompt, .prose, .turnEnd, .compaction, .boundary, .hidden: false
        default: true
        }
    }
}

/// The grid's measures, shared by every row on it.
enum RowGrid {
    /// The glyph cell: wide enough for the widest glyph, and no wider.
    static let cell: CGFloat = 20
    /// Between the glyph cell and the line.
    static let spacing: CGFloat = 10
    /// Where the line begins: every row's text starts here.
    static var text: CGFloat { cell + spacing }
    /// Under a row whose rail runs on to the next.
    static let inRun: CGFloat = 10
    /// Under the last row of a run.
    static let afterRun: CGFloat = 14
    /// Under a prompt or the agent's prose.
    static let prose: CGFloat = 12
}

/// A row's meta: plain words, or a change's counts in the added and removed
/// colours.
struct RowMeta: Equatable {
    enum Ink: Equatable { case faint, added, removed }
    struct Run: Equatable {
        let text: String
        let ink: Ink
    }

    var runs: [Run] = []

    init(_ text: String = "") {
        runs = text.isEmpty ? [] : [Run(text: text, ink: .faint)]
    }

    /// "+9 −14".
    static func change(added: UInt32, removed: UInt32) -> RowMeta {
        var meta = RowMeta()
        meta.runs = [
            Run(text: "+\(added)", ink: .added), Run(text: " ", ink: .faint),
            Run(text: "−\(removed)", ink: .removed),
        ]
        return meta
    }

    /// This meta with more words after it.
    func then(_ text: String) -> RowMeta {
        guard !text.isEmpty else { return self }
        guard !runs.isEmpty else { return RowMeta(text) }
        var meta = self
        meta.runs.append(Run(text: " · \(text)", ink: .faint))
        return meta
    }

    var isEmpty: Bool { runs.isEmpty }
    var string: String { runs.map(\.text).joined() }

    func attributed(_ design: Design) -> AttributedString {
        runs.reduce(into: AttributedString()) { text, run in
            var piece = AttributedString(run.text)
            piece.foregroundColor = switch run.ink {
            case .faint: design.inkFaint.color
            case .added: design.added.color
            case .removed: design.removed.color
            }
            text += piece
        }
    }
}

/// The one row primitive: a glyph cell on the rail, then one line — the
/// verb in the text face, the subject in mono, the meta on the trailing edge
/// — then whatever the row says underneath: a one-line note, the person's
/// words in quotes, a command's output head, or the detail it opens to.
struct GridRow: View {
    @Environment(\.design) private var design
    @Environment(\.dynamicTypeSize) private var typeSize
    enum SubjectFace { case mono, path, text }
    enum DetailFace { case mono, text }

    let kind: String
    let glyph: String
    var accented = false
    /// It went wrong: its glyph takes the removed ink.
    var failed = false
    var rail = RailJoin.none
    var verb = ""
    /// A verb that reports rather than acts, drawn a step quieter.
    var quiet = false
    var subject = ""
    var subjectFace = SubjectFace.mono
    var meta = RowMeta()
    /// Which end of a long subject is kept: a path keeps its file name, a
    /// command its start.
    var truncation: Text.TruncationMode = .tail
    var note: String?
    var quote: String?
    var output: [String] = []
    /// The output's start was dropped to keep it bounded.
    var trimmed = false
    var more: UInt = 0
    var detail: String?
    var detailFace = DetailFace.mono
    /// How many lines of the detail show; all of it when nil.
    var detailLines: Int?
    var below: AnyView?
    var opens = false
    var open = false
    /// Whether an opening row says so at its trailing edge; a run's line
    /// says it with its own mark instead.
    var trailingChevron = true
    var toggle: () -> Void = {}
    @Environment(\.unresolvedFailure) private var unresolvedFailure

    var body: some View {
        Group {
            if opens {
                // A one-line row is under 44 pt: its target reaches into the
                // gaps around it, which leaves the chat drawn as it was.
                Button(action: toggle) { layout.thumbTarget(y: GridRow.reach).frame(minHeight: 44) }
                    .buttonStyle(.amuxRow)
            } else {
                layout
            }
        }
        .accessibilityElement(children: .combine)
        .identified(
            "chat.row.\(kind)",
            label: [verb, subject, meta.string, quote ?? ""].filter { !$0.isEmpty }
                .joined(separator: ", "),
            value: opens ? (open ? "open" : "folded") : nil)
        .reclaimingThumbTarget(y: opens ? GridRow.reach : 0)
        .padding(.bottom, rail.continues ? RowGrid.inRun : RowGrid.afterRun)
        .padding(.leading, rail.nested ? RowGrid.text : 0)
        .background(alignment: .topLeading) { railLine }
        .fixedSize(horizontal: false, vertical: true)
    }

    private static let reach: CGFloat = 12

    /// The hairline from under this row's glyph to the top of the next row's.
    /// A step set in under its parent carries the parent's rail past its
    /// own glyph instead.
    @ViewBuilder private var railLine: some View {
        if rail.continues {
            GeometryReader { geometry in
                Path { path in
                    let x = RowGrid.cell / 2
                    path.move(to: CGPoint(x: x, y: rail.nested ? 0 : RowGrid.cell - 2))
                    path.addLine(to: CGPoint(x: x, y: geometry.size.height))
                }
                .stroke(design.hairline.color, lineWidth: design.metrics.hairline)
            }
        }
    }

    private var layout: some View {
        HStack(alignment: .top, spacing: RowGrid.spacing) {
            Image(systemName: unresolvedFailure ? "xmark" : glyph)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(glyphInk)
                .frame(width: RowGrid.cell, height: RowGrid.cell)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    if typeSize.isAccessibilitySize {
                        stacked
                    } else {
                        RowLine {
                            if !verb.isEmpty { verbText.lineLimit(1).rowLine(.verb) }
                            if !subject.isEmpty {
                                subjectText
                                    .lineLimit(1)
                                    .truncationMode(subjectFace == .text ? .tail : truncation)
                                    .rowLine(.subject)
                            }
                            if !meta.isEmpty { metaText.lineLimit(1).rowLine(.meta) }
                        }
                    }
                    if opens && trailingChevron {
                        Image(systemName: open ? "chevron.up" : "chevron.down")
                            .font(.system(size: 10, weight: .medium))
                            .foregroundStyle(design.inkFaint.color)
                    }
                }
                .frame(minHeight: RowGrid.cell)
                underneath
            }
        }
        .contentShape(Rectangle())
    }

    /// At an accessibility size the verb, its subject and the meta cannot
    /// share one line without each losing most of itself: a file name was
    /// cut to its extension. They stack, the subject wrapping to two lines
    /// before it loses its middle.
    private var stacked: some View {
        VStack(alignment: .leading, spacing: 2) {
            if !verb.isEmpty { verbText.fixedSize(horizontal: false, vertical: true) }
            if !subject.isEmpty {
                subjectText
                    .lineLimit(2)
                    .truncationMode(subjectFace == .text ? .tail : truncation)
            }
            if !meta.isEmpty { metaText.fixedSize(horizontal: false, vertical: true) }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var glyphInk: Color {
        if unresolvedFailure || failed { return design.removed.color }
        return accented ? design.accent.color : design.inkFaint.color
    }

    private var verbText: some View {
        Text(verb)
            .designFont(.detail, design)
            .foregroundStyle(
                unresolvedFailure ? design.removed.color
                    : quiet ? design.inkMuted.color : design.ink.color)
    }

    private var subjectText: some View {
        Text(subject)
            .designFont(subjectFace == .text ? .detail : .monoSmall, design)
            .foregroundStyle(subjectFace == .path ? design.ink.color : design.inkMuted.color)
    }

    private var metaText: some View {
        Text(meta.attributed(design))
            .designFont(.caption, design)
            // Usually a measured duration: a compared screenshot masks it.
            .reported("chat.row.meta.volatile")
    }

    @ViewBuilder private var underneath: some View {
        if let note {
            Text(note)
                .designFont(.monoSmall, design)
                .foregroundStyle(design.inkFaint.color)
                .lineLimit(1)
        }
        if let quote {
            Text("“\(quote)”")
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
        }
        if !output.isEmpty {
            VStack(alignment: .leading, spacing: 1) {
                if trimmed {
                    Text(String(localized: "··· earlier output trimmed"))
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkFaint.color)
                }
                ForEach(Array(output.enumerated()), id: \.offset) { _, line in
                    Text(line)
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkMuted.color)
                        .lineLimit(1)
                }
                if more > 0 {
                    Text(String(localized: "··· \(more) more lines"))
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkFaint.color)
                }
            }
        }
        if let detail {
            Text(detail)
                .designFont(detailFace == .text ? .detail : .monoSmall, design)
                .foregroundStyle(detailFace == .text ? design.ink.color : design.inkMuted.color)
                .lineLimit(detailLines)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        }
        if let below { below }
    }
}

/// One line of a grid row: a verb, the thing it acted on, and the meta on
/// the trailing edge, all on one baseline.
///
/// A stack gives a line that does not fit to whichever text has the layout
/// priority, and the other is squeezed to nothing. Neither can afford that
/// here: a tool's meta can be a whole sentence, and a long command must not
/// push off the "exit 1" that says how it went. So the subject is served
/// first and the meta gives up width until it is down to three fifths of the
/// contested line; below that the two truncate together, the subject giving
/// way first as it does in the terminal.
struct RowLine: Layout {
    enum Role: Int { case verb, subject, meta }

    struct RoleKey: LayoutValueKey {
        static let defaultValue = Role.verb
    }

    /// The most of a contested line the trailing meta may hold: enough for
    /// "exit 101 · 4.2s" or "12s · this session" beside a long command.
    static let metaShare: CGFloat = 0.6
    /// Between the verb and the subject.
    private let spacing: CGFloat = 7
    /// The clear space between the subject and the meta.
    private let gap: CGFloat = 16
    /// What a line with nothing on its trailing edge keeps there anyway.
    private let trailing: CGFloat = 8

    static func split(
        content: CGFloat, subject: CGFloat, meta: CGFloat
    ) -> (subject: CGFloat, meta: CGFloat) {
        guard content > 0 else { return (0, 0) }
        guard subject + meta > content else { return (subject, meta) }
        let allowed = max(content - subject, content * metaShare)
        let meta = min(meta, allowed)
        return (min(subject, content - meta), meta)
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let widths = widths(subviews, within: proposal.width)
        let line = widths.values.reduce(0, +) + fixed(subviews)
        let heights = heights(subviews, widths: widths)
        return CGSize(
            width: proposal.width.map { $0.isFinite ? $0 : line } ?? line,
            height: heights.ascent + heights.descent)
    }

    func placeSubviews(
        in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()
    ) {
        let widths = widths(subviews, within: bounds.width)
        let heights = heights(subviews, widths: widths)
        var leading = bounds.minX
        for (index, subview) in subviews.enumerated() {
            guard let width = widths[index] else { continue }
            let role = subview[RoleKey.self]
            let x = role == .meta ? bounds.maxX - width : leading
            if role != .meta { leading = x + width + spacing }
            let size = ProposedViewSize(width: width, height: nil)
            let baseline = subview.dimensions(in: size)[.firstTextBaseline]
            subview.place(
                at: CGPoint(x: x, y: bounds.minY + heights.ascent - baseline),
                anchor: .topLeading, proposal: size)
        }
    }

    private func widths(_ subviews: Subviews, within available: CGFloat?) -> [Int: CGFloat] {
        var ideal: [Role: (index: Int, width: CGFloat)] = [:]
        for (index, subview) in subviews.enumerated() {
            ideal[subview[RoleKey.self]] = (index, subview.sizeThatFits(.unspecified).width)
        }
        var widths = ideal.values.reduce(into: [Int: CGFloat]()) { $0[$1.index] = $1.width }
        guard let available, available.isFinite else { return widths }
        // Without a subject the verb is what the meta shares the line with.
        let hasSubject = ideal[.subject] != nil
        let lead = hasSubject ? (ideal[.verb].map { $0.width + spacing } ?? 0) : 0
        switch (ideal[.subject] ?? ideal[.verb], ideal[.meta]) {
        case let (.some(main), .some(meta)):
            let content = max(0, available - lead - gap)
            let share = Self.split(content: content, subject: main.width, meta: meta.width)
            widths[main.index] = share.subject
            widths[meta.index] = share.meta
        case let (.some(main), .none):
            widths[main.index] = min(main.width, max(0, available - lead - trailing))
        case let (.none, .some(meta)):
            widths[meta.index] = min(meta.width, max(0, available - gap))
        case (.none, .none):
            break
        }
        if hasSubject, let verb = ideal[.verb] {
            // A verb longer than the whole line keeps what is left of it.
            widths[verb.index] = min(verb.width, max(0, available - trailing))
        }
        return widths
    }

    private func fixed(_ subviews: Subviews) -> CGFloat {
        let roles = Set(subviews.map { $0[RoleKey.self] })
        return (roles.contains(.verb) && roles.contains(.subject) ? spacing : 0)
            + (roles.contains(.meta) ? gap : trailing)
    }

    private func heights(
        _ subviews: Subviews, widths: [Int: CGFloat]
    ) -> (ascent: CGFloat, descent: CGFloat) {
        var ascent: CGFloat = 0
        var descent: CGFloat = 0
        for (index, subview) in subviews.enumerated() {
            let dimensions = subview.dimensions(in: ProposedViewSize(width: widths[index], height: nil))
            let baseline = dimensions[.firstTextBaseline]
            ascent = max(ascent, baseline)
            descent = max(descent, dimensions.height - baseline)
        }
        return (ascent, descent)
    }
}

extension View {
    /// Says which of a row line's three texts this one is.
    func rowLine(_ role: RowLine.Role) -> some View {
        layoutValue(key: RowLine.RoleKey.self, value: role)
    }
}


/// What the person said, in a bubble set in from the leading edge.
struct PromptBubble: View {
    @Environment(\.design) private var design
    let text: [Segment]
    var steered = false
    var bytes: (BlobRef) -> Data? = { _ in nil }

    var body: some View {
        HStack {
            Spacer(minLength: 44)
            VStack(alignment: .trailing, spacing: 6) {
                SegmentsView(segments: text, bytes: bytes)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .background {
                        RoundedRectangle(
                            cornerRadius: design.metrics.controlRadius + 3, style: .continuous)
                        .fill(design.sunken.color)
                    }
                if steered {
                    Text("steered")
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                }
            }
        }
        .accessibilityElement(children: .combine)
        .identified(
            "chat.row.prompt", label: ChatWords.text(of: text), value: steered ? "steered" : nil)
    }
}

/// Text with attachment chips at their places.
struct SegmentsView: View {
    @Environment(\.design) private var design
    let segments: [Segment]
    var bytes: (BlobRef) -> Data? = { _ in nil }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(segments.enumerated()), id: \.offset) { _, segment in
                switch segment {
                case .text(let text):
                    Text(text)
                        .designFont(.body, design)
                        .foregroundStyle(design.ink.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                case .attachment(let view):
                    AttachmentChip(view: view, bytes: bytes)
                }
            }
        }
    }
}

/// An attachment as the composer and the rows show it: its name and size,
/// and a photo's thumbnail once its bytes are here.
public struct AttachmentChip: View {
    @Environment(\.design) private var design
    @Environment(\.dynamicTypeSize) private var typeSize
    /// The remove mark grows with the chip's words.
    @ScaledMetric(relativeTo: .caption) private var removeGlyph: CGFloat = 10
    let view: AttachmentView
    let bytes: (BlobRef) -> Data?
    var remove: (() -> Void)?

    public init(
        view: AttachmentView, bytes: @escaping (BlobRef) -> Data? = { _ in nil },
        remove: (() -> Void)? = nil
    ) {
        self.view = view
        self.bytes = bytes
        self.remove = remove
    }

    public var body: some View {
        HStack(spacing: 7) {
            if case .image(let blob) = view, let image = Thumbnail.image(bytes(blob)) {
                image.resizable().scaledToFill()
                    .frame(width: 28, height: 28)
                    .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
            } else {
                Image(systemName: ChatWords.chipGlyph(view))
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(design.inkMuted.color)
            }
            words
            if let remove {
                // The mark is drawn small, but a thumb finds it: its target
                // is grown to 44 points around it, whatever the text size.
                let reach = max(0, (44 - removeGlyph) / 2).rounded(.up)
                Button(action: remove) {
                    Image(systemName: "xmark")
                        .font(.system(size: removeGlyph, weight: .semibold))
                        .foregroundStyle(design.inkFaint.color)
                        .frame(width: removeGlyph, height: removeGlyph)
                        .thumbTarget(x: reach, y: reach)
                }
                .buttonStyle(.amuxControl)
                .accessibilityLabel("Remove")
                .identified("chat.attachment.remove", label: "Remove")
                .reclaimingThumbTarget(x: reach, y: reach)
            }
        }
        .padding(.horizontal, 9)
        .padding(.vertical, 5)
        .background {
            Capsule().fill(design.raised.color)
                .overlay(Capsule().strokeBorder(design.hairline.color, lineWidth: 1))
        }
        .identified("chat.attachment", label: ChatWords.chip(view))
    }
}

extension AttachmentChip {
    /// The chip's words, read out as the one phrase they are, however many
    /// texts draw them.
    private var words: some View {
        wordsLaidOut
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(ChatWords.chip(view))
    }

    /// The name, cut in its middle so its extension stays, and what it
    /// measures, never cut. At an accessibility size the name may take two
    /// lines and the size goes under it.
    @ViewBuilder
    private var wordsLaidOut: some View {
        let (name, detail) = ChatWords.chipParts(view)
        if typeSize.isAccessibilitySize {
            VStack(alignment: .leading, spacing: 1) {
                Text(verbatim: name)
                    .lineLimit(2)
                    .truncationMode(.middle)
                if !detail.isEmpty {
                    Text(verbatim: detail)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                }
            }
            .designFont(.caption, design)
            .foregroundStyle(design.inkMuted.color)
            .fixedSize(horizontal: false, vertical: true)
        } else {
            HStack(spacing: 0) {
                Text(verbatim: name)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if !detail.isEmpty {
                    Text(verbatim: " · " + detail)
                        .lineLimit(1)
                        .fixedSize()
                }
            }
            .designFont(.caption, design)
            .foregroundStyle(design.inkMuted.color)
        }
    }
}

/// Decodes an image without reaching for a platform image type.
enum Thumbnail {
    static func image(_ data: Data?) -> Image? {
        guard let data, let source = CGImageSourceCreateWithData(data as CFData, nil),
              let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                  kCGImageSourceCreateThumbnailFromImageAlways: true,
                  kCGImageSourceThumbnailMaxPixelSize: 900,
                  kCGImageSourceCreateThumbnailWithTransform: true,
              ] as CFDictionary)
        else { return nil }
        return Image(decorative: image, scale: 1)
    }
}

/// What the agent said, as markdown, parsed away from the main thread.
struct ProseRow: View {
    @Environment(\.design) private var design
    let text: [Segment]
    let streaming: Bool
    let workingNote: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(text.enumerated()), id: \.offset) { _, segment in
                switch segment {
                case .text(let markdown):
                    Prose(markdown: markdown, muted: workingNote)
                case .attachment(let view):
                    AttachmentChip(view: view)
                }
            }
        }
        .identified(
            "chat.row.prose", label: ChatWords.text(of: text),
            value: streaming ? "streaming" : (workingNote ? "working-note" : "final"))
    }
}

/// Markdown, rendered: headings, paragraphs, lists, quotes, rules, code
/// and tables that scroll sideways.
struct Prose: View {
    @Environment(\.design) private var design
    @Environment(\.photographed) private var photographed
    let markdown: String
    var muted = false
    /// A heading drawn above the document, for text whose title arrives
    /// apart from it, such as a plan's.
    var title: String?
    @State private var document: MarkdownDocument?
    @State private var parsed: String?

    /// What is drawn: the parsed document, or in front of a camera, which
    /// cannot wait for the parse, the same document parsed on the spot.
    private var blocks: [MarkdownBlock] {
        if parsed == markdown, let document { return document.blocks }
        if photographed { return MarkdownDocument.parse(markdown).blocks }
        return document?.blocks ?? []
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let title {
                MarkdownBlockView(block: .heading(level: 1, text: AttributedString(title)), muted: muted)
            }
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                MarkdownBlockView(block: block, muted: muted)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .tint(design.accent.color)
        .task(id: markdown) {
            let source = markdown
            let document = await Task.detached(priority: .userInitiated) {
                MarkdownDocument.parse(source)
            }.value
            guard !Task.isCancelled else { return }
            self.document = document
            parsed = source
        }
        .reported(
            "chat.prose.render", value: parsed == markdown || photographed ? "rendered" : "pending")
    }
}

private struct MarkdownBlockView: View {
    @Environment(\.design) private var design
    let block: MarkdownBlock
    let muted: Bool

    private var ink: Color { muted ? design.inkMuted.color : design.ink.color }

    var body: some View {
        switch block {
        case .heading(let level, let text):
            Text(inline(text))
                .font(heading(level))
                .foregroundStyle(ink)
                .fixedSize(horizontal: false, vertical: true)
        case .paragraph(let text):
            Text(inline(text))
                .designFont(.body, design)
                .foregroundStyle(ink)
                .lineSpacing(4)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        case .list(_, let items):
            // Each item is its own row so a wrapped line hangs under the item's text, not
            // under its marker.
            VStack(alignment: .leading, spacing: 6) {
                ForEach(items) { item in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(item.marker)
                            .foregroundStyle(design.inkFaint.color)
                        Text(inline(item.text))
                            .foregroundStyle(ink)
                            .lineSpacing(4)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.leading, CGFloat(item.depth) * 16)
                }
            }
            .designFont(.body, design)
        case .code(let language, let text):
            VStack(alignment: .leading, spacing: 0) {
                if let language, !language.isEmpty {
                    Text(language)
                        .designFont(.sectionTitle, design)
                        .foregroundStyle(design.inkFaint.color)
                        .padding(.horizontal, 12)
                        .padding(.top, 8)
                }
                ScrollView(.horizontal) {
                    Text(text)
                        .designFont(.mono, design)
                        .foregroundStyle(design.ink.color)
                        .textSelection(.enabled)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 10)
                }
                .scrollIndicators(.hidden)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .background {
                let shape = RoundedRectangle(
                    cornerRadius: design.metrics.controlRadius, style: .continuous)
                shape.fill(design.sunken.color)
                    .overlay(shape.strokeBorder(design.hairline.color, lineWidth: 1))
            }
        case .quote(let lines):
            HStack(alignment: .top, spacing: 10) {
                Capsule().fill(design.inkFaint.color.opacity(0.45)).frame(width: 2.5)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                        Text(inline(line))
                            .designFont(.body, design)
                            .foregroundStyle(design.inkMuted.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            .fixedSize(horizontal: false, vertical: true)
        case .table(let header, let rows):
            ScrollView(.horizontal) {
                VStack(alignment: .leading, spacing: 0) {
                    tableLine(header, emphasis: true)
                    ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                        Rectangle().fill(design.hairline.color)
                            .frame(height: design.metrics.hairline)
                        tableLine(row, emphasis: false)
                    }
                }
                .background {
                    let shape = RoundedRectangle(
                        cornerRadius: design.metrics.controlRadius, style: .continuous)
                    shape.fill(design.sunken.color.opacity(0.6))
                        .overlay(shape.strokeBorder(design.hairline.color, lineWidth: 1))
                }
            }
            .scrollIndicators(.hidden)
        case .rule:
            Rectangle().fill(design.hairline.color).frame(height: design.metrics.hairline)
        }
    }

    private func tableLine(_ cells: [AttributedString], emphasis: Bool) -> some View {
        HStack(alignment: .top, spacing: 0) {
            ForEach(Array(cells.enumerated()), id: \.offset) { index, cell in
                Text(inline(cell))
                    .designFont(emphasis ? .caption : .monoSmall, design)
                    .foregroundStyle(emphasis ? design.inkMuted.color : design.ink.color)
                    .frame(width: index == 0 ? 146 : 128, alignment: .leading)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
            }
        }
    }

    private func inline(_ source: AttributedString) -> AttributedString {
        design.inline(source)
    }

    private func heading(_ level: Int) -> Font {
        BundledFonts.register()
        let size: CGFloat = level == 1 ? 21 : (level == 2 ? 18 : 15.5)
        return .custom(design.faces.display, size: size, relativeTo: level == 1 ? .title3 : .headline)
            .weight(.semibold)
    }
}


/// A rule across the chat with what it marks written into it, the words
/// starting where every row's text starts.
private struct FeedRule: View {
    @Environment(\.design) private var design
    let kind: String
    let label: String

    var body: some View {
        HStack(spacing: RowGrid.spacing) {
            Rectangle().fill(design.hairline.color)
                .frame(width: RowGrid.cell, height: design.metrics.hairline)
            // The label wins the width over the trailing rule, which only fills what is left.
            Text(label)
                .designFont(.caption, design)
                .foregroundStyle(design.inkFaint.color)
                .lineLimit(1)
                .layoutPriority(1)
            Rectangle().fill(design.hairline.color).frame(height: design.metrics.hairline)
        }
        .padding(.top, 4)
        .padding(.bottom, RowGrid.afterRun)
        .accessibilityElement(children: .combine)
        .identified("chat.row.\(kind)", label: label)
    }
}

/// A quiet line under the last reply, on the reply's own left edge: it
/// speaks for the whole turn, not for one row of its runs.
private struct Footer: View {
    @Environment(\.design) private var design
    let kind: String
    let text: String
    var failed = false

    var body: some View {
        // Named on the text itself, so the surface a check masks as volatile
        // (a duration) is where the glyphs are drawn: the negative padding
        // below pulls them above the padded frame.
        Text(text)
            .designFont(.caption, design)
            .foregroundStyle(failed ? design.removed.color : design.inkFaint.color)
            .identified("chat.row.\(kind)", label: text)
            .padding(.top, -4)
            .padding(.bottom, RowGrid.afterRun)
    }
}

/// An ask that is the work, resolved in place on the grid: questions and
/// their answers, a plan and its verdict, a form, a link, an access grant, or
/// a dialog this build could not read.
private struct AskRowView: View {
    @Environment(\.design) private var design
    let ask: AskRow
    let rail: RailJoin
    let expanded: Bool
    let toggle: () -> Void

    var body: some View {
        switch ask {
        case .questions(let questions, let answers, let skipped, let resolution, let reply):
            questionsRow(questions, answers, skipped: Int(skipped), resolution, reply)
        case .plan(let body, let verdict, let mode, let modeName, _, let note, let title):
            // An open plan reads whole under its heading while the decision
            // stands in the composer's place; once decided it folds to its
            // heading and opens on a tap.
            let open = verdict == .open
            let sentBack = verdict == .sentBack && !(note ?? "").isEmpty
            GridRow(
                kind: "plan", glyph: "list.bullet.rectangle", accented: open,
                rail: rail, verb: planVerb(verdict), subject: sentBack ? "" : title ?? "",
                subjectFace: .text,
                meta: RowMeta(verdict == .approvedSwitching
                    ? String(localized: "switched to \(modeName.isEmpty ? mode : modeName)") : ""),
                quote: sentBack ? ChatWords.firstLine(note ?? "") : nil,
                below: open || expanded ? AnyView(planBody(title, body)) : nil,
                opens: !open, open: expanded, toggle: toggle)
        case .form(let server, let message, let fields, let resolution):
            GridRow(
                kind: "form", glyph: "list.bullet.clipboard", accented: resolution == .open,
                rail: rail, verb: formVerb(resolution, count: fields.count, server: server),
                subject: resolution == .answered ? server : "",
                detail: expanded && !message.isEmpty ? message : nil, detailFace: .text,
                opens: !message.isEmpty, open: expanded, toggle: toggle)
        case .link(let server, let message, let url, let resolution):
            GridRow(
                kind: "link", glyph: "link", accented: resolution == .open, rail: rail,
                verb: resolution == .answered
                    ? String(localized: "Opened link from")
                    : resolution == .open
                        ? String(localized: "Link from")
                        : ChatWords.resolution(resolution, answered: ""),
                subject: server, detail: expanded ? "\(message)\n\(url)" : nil,
                opens: true, open: expanded, toggle: toggle)
        case .grant(let reason, let read, let write, let network, let hosts, let resolution, let granted):
            let asked = access(read: read, write: write, network: network, hosts: hosts)
            GridRow(
                kind: "grant", glyph: "lock", accented: resolution == .open, rail: rail,
                verb: resolution == .answered && granted != nil
                    ? String(localized: "Granted")
                    : resolution == .open
                        ? String(localized: "Wants access")
                        : ChatWords.resolution(resolution, answered: String(localized: "Denied")),
                subject: granted.map {
                    access(read: $0.read, write: $0.write, network: $0.network, hosts: hosts)
                } ?? asked,
                meta: RowMeta(granted.map {
                    $0.forSession ? String(localized: "this session") : String(localized: "this turn")
                } ?? ""),
                detail: expanded && !reason.isEmpty ? reason : nil, detailFace: .text,
                opens: !reason.isEmpty, open: expanded, toggle: toggle)
        case .unanswerable(let reason, let resolution):
            GridRow(
                kind: "unanswerable", glyph: "exclamationmark.bubble",
                accented: resolution == .open, rail: rail,
                verb: resolution == .open
                    ? String(localized: "Can’t answer this here")
                    : String(localized: "\(ChatWords.resolution(resolution, answered: String(localized: "Answered"))) a dialog this build can’t read"),
                detail: expanded && !reason.isEmpty ? reason : nil, detailFace: .text,
                opens: !reason.isEmpty, open: expanded, toggle: toggle)
        }
    }

    /// One question names itself on the line; several say how many. The
    /// answers sit in a card under the line: each question's header, then
    /// the picks as pills and a typed answer in quotes, then the note.
    /// Says what happened to the questions ("Answered 2 of 3 questions"),
    /// with each answer under it and "Skipped" where one was left. A reply
    /// instead lists only what was answered before it.
    private func questionsRow(
        _ questions: [QuestionView], _ answers: [AnswerView], skipped: Int, _ resolution: Resolution,
        _ reply: String?
    ) -> some View {
        let single = questions.count == 1
        let given = answers.count - skipped
        let some = resolution == .answered && skipped > 0
        var pairs = Array(zip(questions, answers))
        if resolution == .replied { pairs = pairs.filter { !$0.1.skipped } }
        return GridRow(
            kind: "question", glyph: "questionmark.circle", accented: resolution == .open,
            rail: rail,
            verb: some && given == 0
                ? String(localized: "Skipped")
                : ChatWords.resolution(resolution, answered: String(localized: "Answered")),
            subject: some && given > 0
                ? String(localized: "\(given) of \(questions.count) questions")
                : single ? "" : String(localized: "\(questions.count) questions"),
            subjectFace: .text,
            below: pairs.isEmpty && reply == nil ? nil : AnyView(answerCard(pairs, reply: reply)))
    }

    private func answerCard(_ pairs: [(QuestionView, AnswerView)], reply: String?) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(pairs.enumerated()), id: \.offset) { index, pair in
                if index > 0 {
                    Rectangle().fill(design.hairline.color).frame(height: design.metrics.hairline)
                }
                VStack(alignment: .leading, spacing: 3) {
                    Text(design.inline(pairs.count == 1 || pair.0.header.isEmpty ? pair.0.question : pair.0.header))
                        .designFont(.detail, design)
                        .foregroundStyle(design.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                    answer(pair.1)
                    if let note = pair.1.note, !note.isEmpty {
                        Text(String(localized: "Note: \(ChatWords.firstLine(note))"))
                            .designFont(.detail, design)
                            .foregroundStyle(design.inkMuted.color)
                    }
                }
            }
            if let reply {
                Text(String(localized: "Replied: \(ChatWords.firstLine(reply))"))
                    .designFont(.detail, design)
                    .foregroundStyle(design.inkMuted.color)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            let shape = RoundedRectangle(cornerRadius: design.metrics.controlRadius + 2, style: .continuous)
            shape.fill(design.raised.color)
                .overlay(shape.strokeBorder(design.hairline.color, lineWidth: 1))
        }
        .padding(.top, 4)
    }

    /// Several picks are pills; one pick is the answer itself, and words the
    /// person typed read in quotes.
    @ViewBuilder
    private func answer(_ answer: AnswerView) -> some View {
        if answer.hidden || answer.picked.count + (answer.other == nil ? 0 : 1) < 2 {
            Text(answer.picked.isEmpty ? AttributedString(ChatWords.answer(answer)) : design.inline(answer.picked[0]))
                .designFont(.bodyEmphasis, design)
                .italic(answer.picked.isEmpty && !answer.hidden)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
        } else {
            HStack(spacing: 6) {
                ForEach(answer.picked, id: \.self) { pick in
                    Text(design.inline(pick))
                        .designFont(.detail, design)
                        .foregroundStyle(design.ink.color)
                        .lineLimit(1)
                        .padding(.horizontal, 9)
                        .padding(.vertical, 2)
                        .overlay(Capsule().strokeBorder(design.hairline.color, lineWidth: 1))
                }
                if let other = answer.other {
                    Text("“\(other)”")
                        .designFont(.detail, design)
                        .italic()
                        .foregroundStyle(design.ink.color)
                        .lineLimit(1)
                }
            }
        }
    }

    private func planBody(_ title: String?, _ body: String) -> some View {
        Prose(markdown: body, title: title)
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.sunken.color)
            }
            .padding(.top, 4)
    }

    private func access(read: [String], write: [String], network: Bool, hosts: [String]) -> String {
        var parts: [String] = []
        if !write.isEmpty { parts.append(String(localized: "write \(write.joined(separator: ", "))")) }
        if !read.isEmpty { parts.append(String(localized: "read \(read.joined(separator: ", "))")) }
        if network {
            parts.append(hosts.isEmpty
                ? String(localized: "network")
                : String(localized: "network \(hosts.joined(separator: ", "))"))
        }
        return parts.joined(separator: " · ")
    }

    private func formVerb(_ resolution: Resolution, count: Int, server: String) -> String {
        switch resolution {
        case .answered:
            count == 1
                ? String(localized: "Sent 1 field to")
                : String(localized: "Sent \(count) fields to")
        case .open: String(localized: "\(server) needs details")
        default:
            String(localized: "\(ChatWords.resolution(resolution, answered: "")) \(server)’s form")
        }
    }

    private func planVerb(_ verdict: PlanVerdict) -> String {
        switch verdict {
        case .open: String(localized: "Plan proposed")
        case .approved, .approvedSwitching: String(localized: "Plan approved")
        case .sentBack: String(localized: "Plan sent back")
        case .dismissed: String(localized: "Plan dismissed")
        }
    }
}

/// Where a row stands in its run of tool steps, placed as the terminal
/// places it from the same shared fold.
public enum RunPlacement: Equatable {
    /// Outside a run, or a step of an opened run with no line above it:
    /// drawn as itself.
    case own
    /// A step drawn as itself under its run's line: the opened run's first
    /// step, or the oldest of a live run's newest steps.
    case step(header: Header)
    /// A finished run's newest step, which draws as the run's one line.
    case folded(words: String)
    /// A step a folded run keeps in view: a failure its turn never fixed.
    case failed

    public struct Header: Equatable {
        public let words: String
        public let open: Bool
    }

    /// The placement of a row the chat draws: `open` is whether its run is
    /// open, `counts` what the run's steps did.
    public static func of(_ row: Row, open: Bool, counts: () -> RunCounts?) -> RunPlacement {
        guard let run = row.run else { return .own }
        if open {
            // Opened, a run lists every step on its own line, under the
            // line that folds it again.
            guard row.id == run.first else { return .own }
            return .step(header: Header(words: ChatWords.run(run, counts: counts()), open: true))
        }
        if run.live {
            // Under way: the newest few steps, with how many came before.
            let shown = min(run.steps, liveSteps)
            let earlier = run.steps - shown
            guard earlier > 0, run.recent == shown - 1 else { return .own }
            return .step(header: Header(words: ChatWords.earlier(earlier), open: false))
        }
        if run.recent == 0 { return .folded(words: ChatWords.run(run, counts: counts())) }
        return .failed
    }

    /// While a run is under way, how many of its newest steps show: the
    /// shared fold's own number.
    static let liveSteps: UInt32 = 3
}

extension EnvironmentValues {
    /// The step drawn is a failure its turn left unfixed, kept in view by a
    /// folded run: its mark and verb take the removed ink.
    @Entry var unresolvedFailure = false
}
