import AmuxValues
import AmuxWords
import SwiftUI

/// One chat row, drawn from its value with the words every client says.
///
/// The person's prompts sit in a bubble on the right and the agent's prose
/// takes the reading width, as Mac chat apps set them. Work sits on one grid: a
/// glyph, then the verb, the subject in mono and the meta, joined to the
/// rows around it by a rail, so a turn's work reads as one run.
struct ChatRow: View {
    let row: Row
    let expanded: Bool
    let rail: RailJoin
    let placement: RunPlacement
    let bytes: (BlobRef) -> Data?
    /// The agent this row's call opens, as the fleet has it: nil when the
    /// row opens none or the fleet does not hold it yet.
    var opened: OpenedAgent?
    /// What the group this call heads comes to, by the fleet.
    var groupState: GroupState?
    let toggle: () -> Void
    let toggleRun: () -> Void
    var toggleGroup: () -> Void = {}
    var openAgent: (AgentKey) -> Void = { _ in }

    var body: some View {
        Group {
            if let group = row.group, row.id == group.first {
                // The calls of a turn that opened several agents: folded,
                // the first draws the group's one line; open, that line
                // heads the calls, each drawn as itself.
                if group.open {
                    VStack(alignment: .leading, spacing: 0) {
                        groupLine(group, rail: RailJoin(continues: true, nested: rail.nested))
                        kind
                    }
                } else {
                    groupLine(group, rail: rail)
                }
            } else {
                placed
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder private var placed: some View {
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
    }

    /// "4 subagents  2 running · 1 waiting on you": a group's one line,
    /// which opens to its calls. Opening one of their agents is a click on
    /// the call's own line, a gesture apart.
    private func groupLine(_ group: SubagentGroup, rail: RailJoin) -> some View {
        let standing = groupState.map(ChatWords.subagentGroupStanding)
        return GridLine(
            glyph: standing == .needsYou ? "shield" : "bot", failed: standing == .failed, you: standing == .needsYou,
            working: standing == .running, rail: rail,
            verb: ChatWords.subagentGroup(groupState?.agents ?? UInt32(group.agents.count)),
            metaText: groupState.map(Self.groupMeta), opens: true, open: group.open, toggle: toggleGroup)
    }

    /// Each count in the ink its own row would take.
    private static func groupMeta(_ state: GroupState) -> Text {
        ChatWords.subagentGroupParts(state).enumerated().reduce(Text("")) { text, item in
            let part = Text("\(item.element.count) \(item.element.standing.words)")
                .foregroundStyle(ink(item.element.standing))
            return item.offset == 0 ? text + part : text + Text(" · ").foregroundStyle(Palette.textTertiary) + part
        }
    }

    private static func ink(_ standing: AgentStanding) -> AnyShapeStyle {
        switch standing {
        case .needsYou: AnyShapeStyle(Palette.you)
        case .failed: AnyShapeStyle(Palette.failure)
        case .running, .idle, .done, .stopped: AnyShapeStyle(Palette.textTertiary)
        }
    }

    /// A call that started or continued a subagent. One that names its
    /// agent opens the agent's chat on a click and says how the agent
    /// stands, from the fleet: the call may have returned while its
    /// subagent runs on. Its answer, once given, reads under it; the whole
    /// of the subagent's work is in its own chat. A call from history names
    /// no agent, says what the call came to and opens to the steps filed
    /// under it.
    @ViewBuilder
    private func subagent(
        _ description: String, continued: Bool, state: CallPhase, answer: String, durationMs: Int64?
    ) -> some View {
        let standing = AgentStanding.of(opened, call: state)
        let verb = continued ? "Continued" : "Subagent"
        if row.opens != nil {
            GridLine(
                glyph: standing == .needsYou ? "shield" : "bot", failed: standing == .failed,
                you: standing == .needsYou, working: standing.live, rail: rail, verb: verb,
                subject: description.isEmpty ? opened?.name ?? "" : ChatWords.firstLine(description),
                proseSubject: true, metaText: Text(standing.words).foregroundStyle(Self.ink(standing)),
                detail: standing.live || answer.isEmpty ? nil : answer, prose: true, detailLines: 2,
                opens: opened != nil, onward: true,
                toggle: { if let opened { openAgent(opened.agent) } })
        } else {
            GridLine(
                glyph: "bot", failed: standing == .failed, rail: rail, verb: verb,
                subject: ChatWords.firstLine(description), proseSubject: true,
                meta: [standing.words, standing.live ? "" : durationMs.map(ChatWords.duration) ?? ""]
                    .filter { !$0.isEmpty }.joined(separator: " · "),
                detail: standing.live || answer.isEmpty ? nil : answer, prose: true,
                detailLines: expanded ? nil : 2, opens: true, open: expanded, toggle: toggle)
        }
    }

    private func runLine(_ words: String, open: Bool, rail: RailJoin) -> some View {
        GridLine(
            glyph: "layers", rail: rail, verb: words, quiet: true, opens: true, open: open, toggle: toggleRun)
        // A folded run's summary is not the step at work, even when its
        // first row is.
        .environment(\.liveStep, nil)
    }

    @ViewBuilder private var kind: some View {
        switch row.kind {
        case .prompt(let text, let steered):
            PromptBubble(segments: text, steered: steered)
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
                case (.start, .failed): "Couldn’t start"
                case (.stop, .failed): "Couldn’t stop"
                case (.start, _):
                    ChatWords.verb(state, row, wants: "Wants to start", doing: "Starting", done: "Started")
                case (.stop, _):
                    ChatWords.verb(state, row, wants: "Wants to stop", doing: "Stopping", done: "Stopped")
                }
            let who = name.isEmpty ? "a subagent" : name
            if let opened {
                // The agent this spawn started, as the fleet has it: a click
                // opens its chat, and the line says how it stands now.
                let standing = AgentStanding(opened.phase)
                GridLine(
                    glyph: standing == .needsYou ? "shield" : glyph(state, "user-plus"),
                    accented: accented(state), failed: state == .failed, you: standing == .needsYou,
                    rail: rail, verb: verb, subject: host.isEmpty ? who : "\(who) on \(host)",
                    metaText: Text(
                        [ChatWords.meta([kind], row, verbPhase: state, note: false), standing.words]
                            .filter { !$0.isEmpty }.joined(separator: " · ")
                    ).foregroundStyle(Self.ink(standing)),
                    quote: denial, opens: true, onward: true, toggle: { openAgent(opened.agent) })
            } else {
                GridLine(
                    glyph: glyph(state, action == .start ? "user-plus" : "user-minus"),
                    accented: accented(state), failed: state == .failed, rail: rail, verb: verb,
                    subject: host.isEmpty ? who : "\(who) on \(host)",
                    meta: ChatWords.meta([kind], row, verbPhase: state, note: false), quote: denial,
                    detail: expanded && !result.isEmpty ? result : nil, opens: !result.isEmpty, open: expanded,
                    toggle: toggle)
            }
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
                glyph: glyph(state, verb.symbol), accented: accented(state), failed: state == .failed,
                rail: rail, verb: ChatWords.explore(verb), subject: subject,
                meta: ChatWords.meta([ChatWords.state(state, decided: row.decision != nil) ?? ""], row, note: false),
                headTruncation: true, quote: denial, detail: expanded && !result.isEmpty ? result : nil,
                opens: !result.isEmpty, open: expanded, toggle: toggle)
        case .subagent(let description, let continued, let state, let answer, let durationMs):
            subagent(description, continued: continued, state: state, answer: answer, durationMs: durationMs)
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
                .foregroundStyle(failed ? AnyShapeStyle(Palette.failure) : AnyShapeStyle(Palette.textTertiary))
                .padding(.bottom, 18)
        case .stopped:
            GridLine(glyph: "circle-stop", rail: rail, verb: "You stopped it", quiet: true)
        case .compaction(let automatic, let idle, let after, let before):
            Rule(
                label: ChatWords.compaction(
                    before: before, after: after, automatic: automatic, idle: idle))
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
                glyph: "file-question", rail: rail, verb: "Can’t show", subject: what,
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
                        glyph: resolution == .open ? "shield" : "circle-help",
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
        case .asking: "shield"
        case .denied, .cancelled: "ban"
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
    /// It waits on the person: its glyph takes the coral kept for that.
    var you = false
    /// Its agent is at work: the glyph cell turns, as the sidebar's badge
    /// does for an agent working.
    var working = false
    let rail: RailJoin
    var verb = ""
    var subject = ""
    /// The subject is words, a subagent's task, rather than a path or code.
    var proseSubject = false
    var meta = ""
    /// The meta drawn in inks of its own, in place of `meta`.
    var metaText: Text?
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
    /// A click opens another chat, the agent the row names, rather than the
    /// row's own detail: its mark points onward.
    var onward = false
    var chevron = true
    var toggle: () -> Void = {}

    @State private var hovering = false
    @Environment(\.liveStep) private var live

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            mark
            VStack(alignment: .leading, spacing: 4) {
                line
                if let note {
                    Text(note).font(codeNote ? .mono(.meta) : .ui(.meta))
                        .foregroundStyle(Palette.textSecondary).lineLimit(3)
                }
                if let quote {
                    Text("“\(quote)”").font(.ui(.meta)).italic().foregroundStyle(Palette.textSecondary)
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
        .help(onward && opens ? "Open chat" : "")
    }

    private var ink: AnyShapeStyle {
        if failed { return AnyShapeStyle(Palette.failure) }
        if you { return AnyShapeStyle(Palette.you) }
        if accented { return AnyShapeStyle(Palette.accent) }
        return AnyShapeStyle(Palette.textSecondary)
    }

    /// The glyph, with the rail running down from it to the next row.
    private var mark: some View {
        Group {
            if working || live != nil {
                // A dot reads as a bullet, which sits on the lowercase
                // middle rather than the capitals'.
                Working(size: Self.cell).offset(y: 1.5)
            } else {
                Icon(glyph, size: 14).foregroundStyle(ink)
            }
        }
            .frame(width: Self.cell, height: 18)
            .frame(maxHeight: .infinity, alignment: .top)
    }

    private var line: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            if !verb.isEmpty {
                Text(verb)
                    .foregroundStyle(failed ? AnyShapeStyle(Palette.failure) : AnyShapeStyle(Palette.textSecondary))
                    .lineLimit(1)
                    .layoutPriority(1)
            }
            if !subject.isEmpty {
                // Quieter than the agent's prose, so a turn's answer is the
                // brightest thing in it; the step at work is lit.
                Text(subject)
                    .font(proseSubject ? .ui(.prose) : .mono(.callout))
                    .foregroundStyle(live != nil ? AnyShapeStyle(.primary) : AnyShapeStyle(Palette.textSecondary))
                    .lineLimit(1)
                    .truncationMode(headTruncation ? .head : .tail)
            }
            if let change {
                Text("+\(change.0)").foregroundStyle(Palette.added).monospacedDigit()
                if change.1 > 0 { Text("−\(change.1)").foregroundStyle(Palette.removed).monospacedDigit() }
            }
            if let live {
                LiveTime(activity: live).foregroundStyle(Palette.textTertiary)
            } else if let metaText {
                metaText.lineLimit(1)
            } else if !meta.isEmpty {
                Text(meta).foregroundStyle(Palette.textTertiary).lineLimit(1)
            }
            if opens, chevron {
                Icon("chevron-right", size: 12)
                    .foregroundStyle(hovering ? Palette.textSecondary : Palette.textTertiary)
                    .rotationEffect(.degrees(!onward && open ? 90 : 0))
                    // On the words' capitals' middle, not hung from its
                    // own foot on their baseline.
                    .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 5.25 }
            }
        }
        .font(.ui(.prose))
        .frame(minHeight: 18)
    }

    private var outputBlock: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(output.joined(separator: "\n"))
                .font(.mono(.small))
                .foregroundStyle(Palette.textSecondary)
                .textSelection(.enabled)
                .lineLimit(nil)
                .fixedSize(horizontal: false, vertical: true)
            if more > 0 || trimmed {
                Text(more > 0 ? "… \(more) more lines" : "…")
                    .font(.ui(.caption))
                    .foregroundStyle(Palette.textTertiary)
                    .padding(.top, 2)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).fill(Palette.inset))
        .overlay(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).strokeBorder(Palette.line))
    }

    @ViewBuilder private func detailBlock(_ text: String) -> some View {
        if prose {
            Text(text)
                .font(.ui(.body))
                .foregroundStyle(Palette.textSecondary)
                .lineLimit(detailLines)
                .textSelection(.enabled)
        } else {
            Text(text)
                .font(.mono(.small))
                .foregroundStyle(Palette.textSecondary)
                .lineLimit(detailLines)
                .textSelection(.enabled)
                .padding(.horizontal, 12)
                .padding(.vertical, 9)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).fill(Palette.inset))
                .overlay(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).strokeBorder(Palette.line))
        }
    }
}

/// The person's prompt, in a bubble on the right. A review sent with it
/// goes under the bubble as a chip, which opens the round it closed.
struct PromptBubble: View {
    let segments: [Segment]
    let steered: Bool

    var body: some View {
        let reviews = Self.reviews(segments)
        let text = ChatWords.text(of: reviews.isEmpty ? segments : segments.filter { Self.review($0) == nil })
        HStack {
            Spacer(minLength: Metrics.reading / 5)
            VStack(alignment: .trailing, spacing: 4) {
                if !text.isEmpty || reviews.isEmpty {
                    Text(text)
                        .font(.ui(.bubble))
                        .lineSpacing(6)
                        .textSelection(.enabled)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                        .background(RoundedRectangle(cornerRadius: Radius.panel, style: .continuous).fill(Palette.well))
                }
                ForEach(Array(reviews.enumerated()), id: \.offset) { _, review in
                    SentReviewChip(review: review)
                }
                if steered {
                    Text("steered").font(.ui(.caption)).foregroundStyle(Palette.textTertiary)
                }
            }
        }
        .padding(.top, 6)
    }

    /// The reviews sent with the prompt.
    static func reviews(_ segments: [Segment]) -> [SentReviewChip.Sent] {
        segments.compactMap(review)
    }

    static func review(_ segment: Segment) -> SentReviewChip.Sent? {
        if case .attachment(.sentReview(let comments, let sent, let text)) = segment {
            return SentReviewChip.Sent(comments: comments, sent: sent, text: text)
        }
        return nil
    }
}

/// A review sent into the chat: "Review · 3 comments", opening the round it
/// closed, read from the agent's host.
struct SentReviewChip: View {
    struct Sent: Identifiable {
        let comments: UInt32
        let sent: SentReviewRef
        /// What the agent read, shown once the host holds none of the round.
        let text: String
        var id: String { sent.snapshot }
    }

    let review: Sent
    @Environment(\.reviewRounds) private var rounds
    @State private var opened: Sent?

    var body: some View {
        Button {
            opened = review
        } label: {
            HStack(spacing: 5) {
                Icon("message-square-text", size: 12).foregroundStyle(Palette.textSecondary)
                Text(ChatWords.reviewChip(ReviewChip(comments: review.comments, moved: false))).lineLimit(1)
            }
            .font(.ui(.meta))
            .padding(.horizontal, 8)
            .frame(height: 24)
            .background(RoundedRectangle(cornerRadius: Radius.row, style: .continuous).fill(Palette.well))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("Show the round this review sent")
        .sheet(item: $opened) { review in
            ReviewRoundSheet(
                round: review.sent.round, text: review.text, read: { await rounds.read(review.sent) },
                openReview: rounds.openReview, close: { opened = nil })
        }
    }
}

/// A line across the feed with words on it: the session's own events.
struct Rule: View {
    let label: String

    var body: some View {
        HStack(spacing: 10) {
            Rectangle().fill(Palette.line).frame(height: 1)
            Text(label).font(.ui(.caption)).foregroundStyle(Palette.textTertiary).fixedSize()
            Rectangle().fill(Palette.line).frame(height: 1)
        }
        .padding(.vertical, 12)
    }
}

extension ExploreVerb {
    /// What a step of this kind is drawn with: reading a file looks
    /// different from searching for one, and both from the web.
    var symbol: String {
        switch self {
        case .read: "file-text"
        case .search: "search"
        case .list: "folder"
        case .fetch, .webSearch: "globe"
        }
    }
}
