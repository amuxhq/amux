import AmuxValues
import AmuxWords
import SwiftUI

/// One chat row, drawn from its value with the words every client says.
///
/// The person's prompts sit in a bubble on the right and the agent's prose
/// takes the reading width, as in the references. Work sits on one grid: a
/// glyph, then the verb, the subject in mono and the meta, joined to the
/// rows around it by a rail, so a turn's work reads as one run.
struct ChatRow: View {
    let row: Row
    let expanded: Bool
    let rail: RailJoin
    let placement: RunPlacement
    let bytes: (BlobRef) -> Data?
    let toggle: () -> Void
    let toggleRun: () -> Void

    var body: some View {
        Group {
            switch placement {
            case .own:
                kind
            case .step(let header):
                VStack(alignment: .leading, spacing: 0) {
                    runLine(header.words, open: header.open, rail: RailJoin(continues: true, nested: rail.nested))
                    kind
                }
            case .folded(let words):
                VStack(alignment: .leading, spacing: 0) {
                    let failed = row.run?.unresolvedFailure == true
                    runLine(words, open: false, rail: failed ? RailJoin(continues: true, nested: rail.nested) : rail)
                    if failed { kind }
                }
            case .failed:
                kind
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func runLine(_ words: String, open: Bool, rail: RailJoin) -> some View {
        GridLine(
            glyph: open ? "chevron-down" : "chevron-right", rail: rail, verb: words, quiet: true, opens: true,
            open: open, chevron: false, toggle: toggleRun)
    }

    @ViewBuilder private var kind: some View {
        switch row.kind {
        case .prompt(let text, let steered):
            PromptBubble(text: ChatWords.text(of: text), steered: steered)
                .padding(.bottom, 14)
        case .prose(let text, let streaming, let workingNote):
            ProseView(source: ChatWords.text(of: text), streaming: streaming, quiet: workingNote)
                .padding(.bottom, 14)
        case .thinking(let text, let open, let durationMs):
            GridLine(
                glyph: "brain", rail: rail, verb: ChatWords.thinking(open: open, durationMs: durationMs),
                quiet: true, detail: expanded && !text.isEmpty ? text : nil, prose: true, opens: !text.isEmpty,
                open: expanded, toggle: toggle)
        case .toolCall(let server, let tool, let fact, let state, let result):
            GridLine(
                glyph: glyph(state, "wrench"), accented: accented(state), failed: state == .failed,
                rail: rail,
                verb: ChatWords.verb(state, row, wants: "Wants to use", doing: "Using", done: "Used"),
                subject: server.isEmpty ? tool : "\(server) · \(tool)",
                meta: ChatWords.meta([fact, state == .failed ? "failed" : ""], row, verbPhase: state, note: false),
                quote: denial, detail: expanded && !result.isEmpty ? result : nil, opens: !result.isEmpty,
                open: expanded, toggle: toggle)
        case .child(let action, let name, let host, let kind, let state, let result):
            let verb =
                switch (action, state) {
                case (.start, .failed): "Could not start"
                case (.stop, .failed): "Could not stop"
                case (.start, _):
                    ChatWords.verb(state, row, wants: "Wants to start", doing: "Starting", done: "Started")
                case (.stop, _):
                    ChatWords.verb(state, row, wants: "Wants to stop", doing: "Stopping", done: "Stopped")
                }
            let who = name.isEmpty ? "a subagent" : name
            GridLine(
                glyph: glyph(state, action == .start ? "user-plus" : "user-minus"),
                accented: accented(state), failed: state == .failed, rail: rail, verb: verb,
                subject: host.isEmpty ? who : "\(who) on \(host)",
                meta: ChatWords.meta([kind], row, verbPhase: state, note: false), quote: denial,
                detail: expanded && !result.isEmpty ? result : nil, opens: !result.isEmpty, open: expanded,
                toggle: toggle)
        case .fileChange(let files, let state):
            fileChange(files, state)
        case .command(let command, let description, let state, let head, let more, let trimmed, _, let durationMs,
            let exitCode):
            let meta = commandMeta(state, exitCode, durationMs)
            if let said = ChatWords.described(description) {
                // The agent said what it is for: its words lead, the command
                // faint under them, and the meta says where it stands.
                GridLine(
                    glyph: glyph(state, "terminal"), accented: accented(state),
                    failed: state == .failed, rail: rail, verb: said,
                    meta: ChatWords.meta(
                        meta + [ChatWords.describedState(state, decided: row.decision != nil) ?? ""], row,
                        note: false),
                    note: ChatWords.commandLine(command), codeNote: true,
                    quote: denial, output: head, trimmed: trimmed, more: more)
            } else {
                GridLine(
                    glyph: glyph(state, "terminal"), accented: accented(state),
                    failed: state == .failed, rail: rail,
                    verb: ChatWords.verb(state, row, wants: "Wants to run", doing: "Running", done: "Ran"),
                    subject: ChatWords.firstLine(command),
                    meta: ChatWords.meta(meta, row, verbPhase: state, note: false),
                    quote: denial, output: head, trimmed: trimmed, more: more)
            }
        case .explore(let verb, let subject, let state, let result):
            GridLine(
                glyph: glyph(state, "search"), accented: accented(state), failed: state == .failed,
                rail: rail, verb: ChatWords.explore(verb), subject: subject,
                meta: ChatWords.meta([ChatWords.state(state, decided: row.decision != nil) ?? ""], row, note: false),
                headTruncation: true, quote: denial, detail: expanded && !result.isEmpty ? result : nil,
                opens: !result.isEmpty, open: expanded, toggle: toggle)
        case .subagent(let description, let running, let toolCount, let lastTool, let answer, let durationMs):
            GridLine(
                glyph: "bot", rail: rail, verb: "Agent",
                subject: ChatWords.firstLine(description),
                meta: ChatWords.subagent(toolCount: toolCount, durationMs: durationMs),
                note: running && !lastTool.isEmpty ? "└ \(lastTool)" : nil,
                detail: !running && !answer.isEmpty ? answer : nil, prose: true, detailLines: expanded ? nil : 2,
                opens: true, open: expanded, toggle: toggle)
        case .background(let command, let description, let running, _):
            let said = ChatWords.described(description)
            GridLine(
                glyph: "play", rail: rail, verb: said ?? "In background",
                subject: said == nil ? ChatWords.firstLine(command) : "",
                meta: said == nil
                    ? (running ? "running" : "finished")
                    : (running ? "in background · running" : "in background · finished"),
                note: said == nil ? nil : ChatWords.commandLine(command), codeNote: true)
        case .image(let path, let generated, let image):
            VStack(alignment: .leading, spacing: 6) {
                GridLine(
                    glyph: "image", rail: rail, verb: generated ? "Generated image" : "Image",
                    subject: path.isEmpty ? image?.name ?? "" : path,
                    meta: image.map { ChatWords.bytes($0.size) } ?? "")
                if let image, let data = bytes(image), let picture = NSImage(data: data) {
                    Image(nsImage: picture)
                        .resizable()
                        .scaledToFit()
                        .frame(maxWidth: 420, maxHeight: 320, alignment: .leading)
                        .clipShape(RoundedRectangle(cornerRadius: Radius.well, style: .continuous))
                        .padding(.leading, GridLine.text)
                        .padding(.bottom, 10)
                }
            }
        case .slashOutput(let command, let args, let output):
            GridLine(
                glyph: "slash", rail: rail, verb: command, subject: args,
                detail: output.isEmpty ? nil : output, detailLines: expanded ? nil : 6, opens: !output.isEmpty,
                open: expanded, toggle: toggle)
        case .ask(let ask):
            askRow(ask)
        case .turnEnd(let failed, let durationMs):
            Text(ChatWords.turnEnd(durationMs: durationMs, failed: failed))
                .font(.ui(.meta))
                .foregroundStyle(failed ? AnyShapeStyle(Palette.failure) : AnyShapeStyle(.tertiary))
                .padding(.bottom, 18)
        case .stopped:
            GridLine(glyph: "circle-stop", rail: rail, verb: "You stopped it", quiet: true)
        case .compaction(let automatic, let after, let before):
            Rule(label: ChatWords.compaction(before: before, after: after, automatic: automatic))
        case .error(let errorKind, let message, let attempts, let gaveUp):
            let detailed = !errorKind.isEmpty && !message.isEmpty
            GridLine(
                glyph: "triangle-alert", failed: true, rail: rail,
                verb: errorKind.isEmpty ? ChatWords.firstLine(message) : errorKind,
                meta: attempts <= 1 ? "" : gaveUp ? "gave up after \(attempts) tries" : "\(attempts) tries",
                detail: detailed ? message : nil, detailLines: expanded ? nil : 3, opens: detailed, open: expanded,
                toggle: toggle)
        case .modelSwitch(let to, let reason):
            GridLine(
                glyph: "arrow-left-right", rail: rail, verb: "Switched to", subject: to,
                note: reason.isEmpty ? nil : reason)
        case .boundary(let kind, let cause):
            Rule(label: ChatWords.boundary(kind, cause: cause))
        case .agentMessage(let from, let kind, let text, let to, let sent, let rejection):
            GridLine(
                glyph: to.isEmpty ? "corner-down-left" : "corner-up-right", failed: sent == .rejected,
                rail: rail, verb: to.isEmpty ? "From" : "To", subject: to.isEmpty ? from : to,
                meta: messageMeta(kind, sent, rejection),
                detail: ChatWords.text(of: text).trimmingCharacters(in: .whitespacesAndNewlines), prose: true,
                detailLines: expanded ? nil : 1, opens: true, open: expanded, toggle: toggle)
        case .autoReview(let status, let statusWord, let risk, let rationale, _):
            GridLine(
                glyph: "shield-check", rail: rail, verb: ChatWords.autoReview(status, statusWord),
                meta: risk.isEmpty ? "" : "\(risk) risk", detail: expanded && !rationale.isEmpty ? rationale : nil,
                prose: true, opens: !rationale.isEmpty, open: expanded, toggle: toggle)
        case .unrecognized(let what, let summary):
            GridLine(
                glyph: "file-question", rail: rail, verb: "Unreadable", subject: what,
                note: summary.isEmpty ? nil : ChatWords.firstLine(summary))
        case .hidden:
            EmptyView()
        }
    }

    @ViewBuilder private func fileChange(_ files: [FileRow], _ state: CallPhase) -> some View {
        let tail = ChatWords.meta([ChatWords.state(state, decided: row.decision != nil) ?? ""], row, note: false)
        if files.isEmpty {
            GridLine(
                glyph: glyph(state, "file-pen"), accented: accented(state), failed: state == .failed, rail: rail,
                verb: "Editing", meta: tail, quote: denial)
        } else {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(files.enumerated()), id: \.offset) { index, file in
                    let words = Self.words(file)
                    GridLine(
                        glyph: glyph(state, words.glyph), accented: accented(state), failed: state == .failed,
                        rail: index == files.count - 1 ? rail : RailJoin(continues: true, nested: rail.nested),
                        verb: words.verb, subject: file.path, meta: index == 0 ? tail : "",
                        change: words.change, headTruncation: true, quote: index == 0 ? denial : nil)
                }
            }
        }
    }

    private static func words(_ file: FileRow) -> (glyph: String, verb: String, change: (UInt32, UInt32)?) {
        switch file.change {
        case .edited: ("file-pen", "", (file.added, file.removed))
        case .created(let lines): ("file-plus", "Created", (lines, 0))
        case .deleted: ("file-x", "Deleted", nil)
        case .moved(let to): ("file-symlink", "Moved to \(to)", nil)
        case .writing(let lines): ("file-plus", "Writing", (lines, 0))
        }
    }

    @ViewBuilder private func askRow(_ ask: AskRow) -> some View {
        switch ask {
        case .plan(let body, let verdict, _, _, let writing, let note, let title):
            VStack(alignment: .leading, spacing: 8) {
                GridLine(
                    glyph: "list-checks", accented: verdict == .open, rail: rail,
                    verb: Self.planVerb(verdict), subject: title ?? "", meta: writing ? "writing" : "",
                    note: note)
                ProseView(source: body, streaming: writing, quiet: false)
                    .padding(12)
                    .background(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).fill(Palette.well))
                    .padding(.leading, GridLine.text)
                    .padding(.bottom, 12)
            }
        case .questions(let questions, let answers, _, let resolution, let reply):
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(questions.enumerated()), id: \.offset) { index, question in
                    let answered = index < answers.count ? ChatWords.answer(answers[index]) : ""
                    GridLine(
                        glyph: resolution == .open ? "hand" : "circle-help",
                        accented: resolution == .open,
                        rail: index == questions.count - 1 && reply == nil
                            ? rail : RailJoin(continues: true, nested: rail.nested),
                        verb: question.header.isEmpty ? "Asked" : question.header, subject: question.question,
                        meta: ChatWords.resolution(resolution, answered: answered), prose: true)
                }
                if let reply {
                    GridLine(glyph: "message-square-text", rail: rail, verb: "You replied", note: reply)
                }
            }
        case .form(let server, let message, let fields, let resolution):
            GridLine(
                glyph: "list-checks", accented: resolution == .open, rail: rail, verb: server,
                subject: message,
                meta: ChatWords.resolution(resolution, answered: "Sent \(fields.count) fields"))
        case .link(let server, let message, let url, let resolution):
            GridLine(
                glyph: "link", accented: resolution == .open, rail: rail, verb: server, subject: message,
                meta: ChatWords.resolution(resolution, answered: "Opened"), note: url)
        case .grant(let reason, _, _, _, _, let resolution, _):
            GridLine(
                glyph: "lock-open", accented: resolution == .open, rail: rail, verb: "Asked for access",
                subject: reason, meta: ChatWords.resolution(resolution, answered: "Granted"))
        case .unanswerable(let reason, let resolution):
            GridLine(
                glyph: "file-question", accented: resolution == .open, rail: rail,
                verb: "Asked in its terminal", subject: reason,
                meta: ChatWords.resolution(resolution, answered: "Answered"))
        }
    }

    private static func planVerb(_ verdict: PlanVerdict) -> String {
        switch verdict {
        case .open: "Plan proposed"
        case .approved, .approvedSwitching: "Plan approved"
        case .sentBack: "Plan sent back"
        case .dismissed: "Plan dismissed"
        }
    }

    private func glyph(_ phase: CallPhase, _ done: String) -> String {
        switch phase {
        case .denied, .asking: "hand"
        case .cancelled: "ban"
        case .pending, .running, .succeeded, .failed: done
        }
    }

    private func accented(_ phase: CallPhase) -> Bool { row.attention && phase == .asking }

    private var denial: String? {
        guard let decision = row.decision, decision.outcome == .denied, let note = decision.note, !note.isEmpty
        else { return nil }
        return ChatWords.firstLine(note)
    }

    private func commandMeta(_ state: CallPhase, _ exitCode: Int32?, _ durationMs: Int64?) -> [String] {
        var meta: [String] = []
        if let exitCode, exitCode != 0 {
            meta.append("exit \(exitCode)")
        } else if exitCode == nil, state == .failed {
            meta.append("failed")
        }
        if let durationMs { meta.append(ChatWords.duration(durationMs)) }
        return meta
    }

    private func messageMeta(_ kind: EnvelopeKind, _ sent: SendState, _ rejection: String) -> String {
        switch (kind, sent) {
        case (.finished, _): "finished"
        case (.failed, _): "failed"
        case (_, .sending): "sending"
        case (_, .rejected): "not delivered · \(rejection)"
        default: ""
        }
    }
}

/// A row on the grid: glyph and rail, then verb, subject and meta on one
/// line, then whatever the row says underneath.
struct GridLine: View {
    /// The glyph's column, and where every grid row's text starts.
    static let cell: CGFloat = 18
    static let text: CGFloat = cell + 10

    let glyph: String
    var accented = false
    var failed = false
    let rail: RailJoin
    var verb = ""
    var subject = ""
    var meta = ""
    var change: (UInt32, UInt32)?
    var note: String?
    /// The note is code, a command under the words that describe it.
    var codeNote = false
    var quiet = false
    var headTruncation = false
    var quote: String?
    var detail: String?
    var prose = false
    var detailLines: Int?
    var output: [String] = []
    var trimmed = false
    var more: UInt = 0
    var opens = false
    var open = false
    var chevron = true
    var toggle: () -> Void = {}

    @State private var hovering = false

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            mark
            VStack(alignment: .leading, spacing: 4) {
                line
                if let note {
                    Text(note).font(codeNote ? .mono(.meta) : .ui(.meta))
                        .foregroundStyle(.secondary).lineLimit(3)
                }
                if let quote {
                    Text("“\(quote)”").font(.ui(.meta)).italic().foregroundStyle(.secondary)
                }
                if !output.isEmpty { outputBlock }
                if let detail { detailBlock(detail) }
            }
            .padding(.bottom, rail.continues ? 8 : 14)
        }
        .padding(.leading, rail.nested ? 28 : 0)
        .contentShape(Rectangle())
        .onTapGesture { if opens { toggle() } }
        .onHover { hovering = opens && $0 }
    }

    private var ink: AnyShapeStyle {
        if failed { return AnyShapeStyle(Palette.failure) }
        if accented { return AnyShapeStyle(Palette.accent) }
        return AnyShapeStyle(.secondary)
    }

    /// The glyph, with the rail running down from it to the next row.
    private var mark: some View {
        Icon(glyph, size: 13)
            .foregroundStyle(ink)
            .frame(width: Self.cell, height: 18)
            .frame(maxHeight: .infinity, alignment: .top)
            .background(alignment: .top) {
                if rail.continues {
                    Rectangle()
                        .fill(Palette.line)
                        .frame(width: 1)
                        .padding(.top, 20)
                }
            }
    }

    private var line: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            if !verb.isEmpty {
                Text(verb)
                    .foregroundStyle(quiet ? AnyShapeStyle(.tertiary) : (failed ? AnyShapeStyle(Palette.failure) : AnyShapeStyle(.secondary)))
                    .lineLimit(1)
                    .layoutPriority(1)
            }
            if !subject.isEmpty {
                Text(subject)
                    .font(.mono(.meta))
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                    .truncationMode(headTruncation ? .head : .tail)
            }
            if let change {
                Text("+\(change.0)").foregroundStyle(Palette.added).monospacedDigit()
                if change.1 > 0 { Text("−\(change.1)").foregroundStyle(Palette.removed).monospacedDigit() }
            }
            if !meta.isEmpty {
                Text(meta).foregroundStyle(.tertiary).lineLimit(1)
            }
            if opens, chevron {
                Icon(open ? "chevron-down" : "chevron-right", size: 11)
                    .foregroundStyle(hovering ? .secondary : .tertiary)
            }
        }
        .font(.ui(.body))
        .frame(minHeight: 18)
    }

    private var outputBlock: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(output.joined(separator: "\n"))
                .font(.mono(.small))
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .lineLimit(nil)
                .fixedSize(horizontal: false, vertical: true)
            if more > 0 || trimmed {
                Text(more > 0 ? "… \(more) more lines" : "…")
                    .font(.ui(.caption))
                    .foregroundStyle(.tertiary)
                    .padding(.top, 2)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).fill(Palette.well))
    }

    @ViewBuilder private func detailBlock(_ text: String) -> some View {
        if prose {
            Text(text)
                .font(.ui(.body))
                .foregroundStyle(.secondary)
                .lineLimit(detailLines)
                .textSelection(.enabled)
        } else {
            Text(text)
                .font(.mono(.small))
                .foregroundStyle(.secondary)
                .lineLimit(detailLines)
                .textSelection(.enabled)
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).fill(Palette.well))
        }
    }
}

/// The person's prompt, in a bubble on the right.
struct PromptBubble: View {
    let text: String
    let steered: Bool

    var body: some View {
        HStack {
            Spacer(minLength: 80)
            VStack(alignment: .trailing, spacing: 4) {
                Text(text)
                    .font(.ui(.bubble))
                    .textSelection(.enabled)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 9)
                    .background(RoundedRectangle(cornerRadius: Radius.composer, style: .continuous).fill(Palette.raised))
                    .overlay(
                        RoundedRectangle(cornerRadius: Radius.composer, style: .continuous).strokeBorder(Palette.line.opacity(0.5)))
                if steered {
                    Text("steered").font(.ui(.caption)).foregroundStyle(.tertiary)
                }
            }
        }
        .padding(.top, 6)
    }
}

/// A line across the feed with words on it: the session's own events.
struct Rule: View {
    let label: String

    var body: some View {
        HStack(spacing: 10) {
            Rectangle().fill(Palette.line).frame(height: 1)
            Text(label).font(.ui(.caption)).foregroundStyle(.tertiary).fixedSize()
            Rectangle().fill(Palette.line).frame(height: 1)
        }
        .padding(.vertical, 12)
    }
}
