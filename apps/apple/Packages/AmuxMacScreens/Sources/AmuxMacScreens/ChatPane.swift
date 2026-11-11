import AmuxValues
import AmuxWords
import SwiftUI

/// An open agent: its chat down the reading width, the ask it waits on and
/// the composer under it, and while the right-hand pane is closed, a card at
/// the top right that says where it works and opens that pane.
public struct ChatPane: View {
    let chat: any ChatSurface
    /// The agent's row in the fleet: its name, host and place.
    let row: FleetRow?
    let family: FamilyHeader?
    let changesOpen: Bool
    let actions: AgentActions

    public init(
        chat: any ChatSurface, row: FleetRow?, family: FamilyHeader? = nil, changesOpen: Bool,
        actions: AgentActions = AgentActions()
    ) {
        self.chat = chat
        self.row = row
        self.family = family
        self.changesOpen = changesOpen
        self.actions = actions
    }

    public var body: some View {
        GeometryReader { pane in
            let placing = CardPlacement(width: pane.size.width, shown: !changesOpen && row != nil)
            let beside = placing != .pill
            VStack(spacing: 0) {
                Feed(
                    chat: chat, host: host, roomAbove: placing == .pill, trailing: placing.trailing,
                    openChanges: actions.openChanges
                )
                .overlay(alignment: .topTrailing) {
                    if !changesOpen, let row {
                        Group {
                            if beside {
                                FloatingCard(
                                    row: row, branch: chat.frame?.git?.branch ?? row.branch,
                                    changes: chat.changes, family: family, actions: actions)
                            } else {
                                FloatingPill(changes: chat.changes, open: actions.openChanges)
                            }
                        }
                        .padding(.top, 10)
                        .padding(.trailing, 14)
                        .transition(.opacity.combined(with: .move(edge: .trailing)))
                    }
                }
                Footer(chat: chat, row: row, host: host, trailing: placing.trailing)
            }
        }
        .animation(.snappy(duration: 0.18), value: changesOpen)
    }

    private var host: String { row.map { PlaceNames.host($0.hostName) } ?? "its host" }
}

/// Where the card goes: beside a centred column where the margin holds it,
/// as the references' does; beside a column moved left to make room while
/// the column stays readable; otherwise folded to a pill above the column.
/// It never covers what is being read.
enum CardPlacement: Equatable {
    case none
    case margin
    case moved
    case pill

    static let room = FloatingCard.width + 28
    static let narrowest: CGFloat = 520

    init(width: CGFloat, shown: Bool) {
        if !shown {
            self = .none
        } else if (width - Metrics.reading) / 2 >= Self.room {
            self = .margin
        } else if width - Self.room - 28 >= Self.narrowest {
            self = .moved
        } else {
            self = .pill
        }
    }

    /// What the column keeps clear on its right for the card.
    var trailing: CGFloat { self == .moved ? Self.room : 0 }
}

/// The rows, oldest first, following the newest while the reader is there.
private struct Feed: View {
    let chat: any ChatSurface
    let host: String
    /// Leave room above the first row for the pill.
    let roomAbove: Bool
    /// Kept clear on the right for the card.
    let trailing: CGFloat
    let openChanges: @MainActor () -> Void
    @State private var position = ScrollPosition(edge: .bottom)
    /// Finished turns whose work the reader opened.
    @State private var opened: Set<String> = []
    @State private var kinds = TurnFold.Kinds()

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                if let older = chat.olderNotice {
                    Text(older).font(.system(size: 12)).foregroundStyle(.tertiary)
                        .frame(maxWidth: .infinity)
                        .padding(.bottom, 12)
                }
                if chat.empty {
                    emptyNotice
                }
                let fold = TurnFold(ids: chat.ids, opened: opened, kinds: kinds, chat: chat)
                ForEach(fold.items) { item in
                    switch item {
                    case .row(let id):
                        RowCell(chat: chat, id: id)
                    case .worked(let turn):
                        WorkedLine(turn: turn, open: opened.contains(turn.end)) {
                            if opened.contains(turn.end) { opened.remove(turn.end) } else { opened.insert(turn.end) }
                        }
                    case .answered(let turn):
                        AnswerFoot(turn: turn, chat: chat, latest: turn.end == fold.latest, openChanges: openChanges)
                    }
                }
                ForEach(Array(chat.landing.enumerated()), id: \.offset) { _, prompt in
                    VStack(alignment: .trailing, spacing: 2) {
                        PromptBubble(text: ChatWords.text(of: prompt.text), steered: false)
                        Text(ChatWords.underway(prompt.underway, host: host))
                            .font(.system(size: 11))
                            .foregroundStyle(.tertiary)
                    }
                    .padding(.bottom, 12)
                }
                ActivityLine(chat: chat)
            }
            .frame(maxWidth: Metrics.reading)
            .padding(.horizontal, 32)
            .padding(.top, roomAbove ? 52 : 24)
            .padding(.bottom, 12)
            .frame(maxWidth: .infinity)
            .padding(.trailing, trailing)
        }
        .scrollPosition($position)
        .defaultScrollAnchor(.bottom, for: .sizeChanges)
        .onScrollGeometryChange(for: Bool.self) { geometry in
            geometry.contentOffset.y + geometry.containerSize.height >= geometry.contentSize.height - 24
        } action: { _, atNewest in
            chat.reading(atNewest: atNewest)
        }
        .onScrollGeometryChange(for: Bool.self) { geometry in
            geometry.contentOffset.y < 80
        } action: { _, atTop in
            if atTop { chat.reachedTop() }
        }
        .onChange(of: chat.toNewest) { _, _ in
            withAnimation(.snappy(duration: 0.2)) { position.scrollTo(edge: .bottom) }
        }
        .overlay(alignment: .bottom) {
            if !chat.following {
                Button {
                    chat.jumpToNewest()
                } label: {
                    HStack(spacing: 5) {
                        Icon("arrow-down", size: 12)
                        Text(chat.frame?.arrivalsHeld == true ? "New activity" : "Latest")
                    }
                        .font(.system(size: 12, weight: .medium))
                        .padding(.horizontal, 12)
                        .padding(.vertical, 6)
                        .background(Capsule().fill(Palette.raised))
                        .overlay(Capsule().strokeBorder(Palette.line))
                        .shadow(color: .black.opacity(0.12), radius: 6, y: 2)
                }
                .buttonStyle(.plain)
                .padding(.bottom, 10)
            }
        }
    }

    @ViewBuilder private var emptyNotice: some View {
        if chat.loadingHint {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Loading this chat from \(host)…").foregroundStyle(.secondary)
            }
            .font(.system(size: 13))
            .frame(maxWidth: .infinity)
            .padding(.top, 40)
        }
    }
}

/// One row, read on its own so that only it redraws when it moves.
private struct RowCell: View {
    let chat: any ChatSurface
    let id: String

    var body: some View {
        if let row = chat.row(id), chat.shows(row) {
            ChatRow(
                row: row, expanded: chat.isExpanded(row.id),
                rail: RailJoin.of(row, next: chat.row(below: row.id)),
                placement: RunPlacement.of(
                    row, open: row.run?.open ?? false,
                    counts: { row.run.flatMap(chat.counts(of:)) }),
                bytes: chat.bytes(of:),
                toggle: { chat.toggle(row.id) },
                toggleRun: { if let run = row.run { chat.toggleRun(run) } })
        }
    }
}

/// What the agent is doing now, under the newest row.
private struct ActivityLine: View {
    let chat: any ChatSurface

    var body: some View {
        if let activity = chat.frame?.composer.activity {
            TimelineView(.periodic(from: .now, by: 1)) { context in
                let elapsed = activity.elapsedMs
                    + Int64(max(0, context.date.timeIntervalSince1970 * 1000 - Double(activity.sinceMs)))
                let parts = ChatWords.activityParts(
                    activity.kind, elapsedMs: activity.sinceMs > 0 ? elapsed : activity.elapsedMs, subject: nil,
                    sinceMs: activity.sinceMs)
                HStack(spacing: 6) {
                    ProgressView().controlSize(.mini)
                    Text(parts.words).foregroundStyle(.secondary)
                    if let subject = parts.subject {
                        Text(subject).font(.system(size: 12, design: .monospaced)).foregroundStyle(.secondary)
                    }
                    if let time = parts.time {
                        Text(time).foregroundStyle(.tertiary).monospacedDigit()
                    }
                }
                .font(.system(size: 12.5))
                .padding(.top, 2)
                .padding(.bottom, 8)
            }
        }
    }
}

/// Under the feed: the queue, the head ask and the composer.
private struct Footer: View {
    let chat: any ChatSurface
    let row: FleetRow?
    let host: String
    let trailing: CGFloat

    var body: some View {
        VStack(spacing: 8) {
            if let frame = chat.frame, !frame.queue.isEmpty {
                QueueList(chat: chat, queue: frame.queue)
            }
            if let ask = chat.ask {
                AskCardView(card: ask, chat: chat)
            }
            if let notice = chat.notice {
                Text(notice).font(.system(size: 12)).foregroundStyle(.red)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if let ended = chat.frame?.ended {
                Text(ended).font(.system(size: 12.5)).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.vertical, 8)
            } else {
                Composer(chat: chat, name: row?.name ?? chat.frame?.name ?? "agent", host: host, reach: row?.hostReach)
            }
        }
        .frame(maxWidth: Metrics.reading)
        .padding(.horizontal, 32)
        .padding(.bottom, 18)
        .frame(maxWidth: .infinity)
        .padding(.trailing, trailing)
    }
}

/// Prompts waiting their turn, each to take back or send now.
private struct QueueList: View {
    let chat: any ChatSurface
    let queue: [QueuedRow]

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(Array(queue.enumerated()), id: \.offset) { _, queued in
                HStack(spacing: 8) {
                    Icon("clock", size: 12).foregroundStyle(.tertiary)
                    Text(ChatWords.text(of: queued.text)).lineLimit(1)
                    Text(ChatWords.queued(queued)).foregroundStyle(.tertiary)
                    Spacer()
                    if queued.canSendNow {
                        Button("Send Now") { chat.sendNow(queued) }.buttonStyle(.link)
                    }
                    if queued.canWithdraw {
                        Button("Take Back") { chat.withdraw(queued) }.buttonStyle(.link)
                    }
                }
                .font(.system(size: 12))
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Palette.hover))
    }
}

// MARK: - Answer-first turns

/// A chat's rows with each finished turn read answer first, as the
/// references read: the prompt, "Worked 3m 14s ›", then what the agent
/// said last. The work between folds under that line and opens on a
/// click; errors stay in sight; the turn underway shows its work as it
/// happens.
struct TurnFold {
    /// What a finished turn did, for its line and its foot.
    struct Turn: Equatable {
        /// The turn's last row, which names it.
        let end: String
        let durationMs: Int64?
        let failed: Bool
        /// Its final words, for copying.
        let answer: [String]
        /// The rows that changed files, for the foot's summary.
        let edits: [String]
    }

    enum Item: Identifiable {
        case row(String)
        case worked(Turn)
        case answered(Turn)

        var id: String {
            switch self {
            case .row(let id): id
            case .worked(let turn): "worked." + turn.end
            case .answered(let turn): "answered." + turn.end
            }
        }
    }

    /// How a row takes part in a turn.
    enum Part {
        case prompt
        case answer
        /// Said whatever the turn's fold: an error, a stop, a boundary.
        case kept
        case work
        case edit
        case end(failed: Bool, durationMs: Int64?)
        case none
    }

    /// Each row's part, read once: a row keeps its kind, so the fold is
    /// worked out without reading every row again as one streams.
    final class Kinds {
        var parts: [String: Part] = [:]
    }

    let items: [Item]
    /// The newest finished turn.
    let latest: String?

    @MainActor
    init(ids: [String], opened: Set<String>, kinds: Kinds, chat: any ChatSurface) {
        func part(_ id: String) -> Part {
            if let known = kinds.parts[id] { return known }
            guard let row = chat.row(id) else { return .none }
            let part = Self.part(row)
            kinds.parts[id] = part
            return part
        }
        var items: [Item] = []
        var latest: String?
        var start = 0
        for (index, id) in ids.enumerated() {
            switch part(id) {
            case .prompt:
                start = index + 1
            case .end(let failed, let durationMs):
                let turn = Array(ids[start..<index])
                // The answer is the turn's last stretch of prose, wherever
                // it falls; the rest is the work, folded, in its order.
                var answerEnd = turn.count
                while answerEnd > 0, !Self.isAnswer(part(turn[answerEnd - 1])) { answerEnd -= 1 }
                var answerStart = answerEnd
                while answerStart > 0, Self.isAnswer(part(turn[answerStart - 1])) || Self.isNone(part(turn[answerStart - 1])) {
                    answerStart -= 1
                }
                let answerRange = answerStart..<answerEnd
                let work = turn.indices.filter { !answerRange.contains($0) }.map { turn[$0] }
                let answer = turn[answerRange].filter { Self.isAnswer(part($0)) }
                let edits = work.filter { if case .edit = part($0) { true } else { false } }
                let summary = Turn(end: id, durationMs: durationMs, failed: failed, answer: answer, edits: edits)
                // Replace the turn's rows, already listed, with its fold.
                items.removeLast(turn.count)
                if work.contains(where: { !Self.isNone(part($0)) }) {
                    items.append(.worked(summary))
                    if opened.contains(id) {
                        items += work.map(Item.row)
                    } else {
                        items += work.filter { if case .kept = part($0) { true } else { false } }.map(Item.row)
                    }
                }
                items += turn[answerRange].map(Item.row)
                if !answer.isEmpty || !edits.isEmpty { items.append(.answered(summary)) }
                latest = id
                start = index + 1
                continue
            default:
                break
            }
            items.append(.row(id))
        }
        self.items = items
        self.latest = latest
    }

    private static func isAnswer(_ part: Part) -> Bool {
        if case .answer = part { return true }
        return false
    }

    private static func isNone(_ part: Part) -> Bool {
        if case .none = part { return true }
        return false
    }

    private static func part(_ row: Row) -> Part {
        switch row.kind {
        case .prompt: .prompt
        case .prose(_, _, let workingNote): workingNote ? .work : .answer
        case .turnEnd(let failed, let durationMs): .end(failed: failed, durationMs: durationMs)
        case .error, .stopped, .boundary, .compaction: .kept
        case .fileChange: .edit
        case .hidden: .none
        default: .work
        }
    }
}

/// "Worked 3m 14s ›" over a finished turn's answer, opening its work.
private struct WorkedLine: View {
    let turn: TurnFold.Turn
    let open: Bool
    let toggle: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Button(action: toggle) {
                HStack(spacing: 4) {
                    Text(ChatWords.turnEnd(durationMs: turn.durationMs, failed: turn.failed))
                        .foregroundStyle(turn.failed ? AnyShapeStyle(.red) : AnyShapeStyle(.secondary))
                    Icon(open ? "chevron-down" : "chevron-right", size: 12)
                        .foregroundStyle(.tertiary)
                }
                .font(.system(size: 13))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(open ? "Hide the work" : "Show the work")
            Rectangle().fill(Palette.line).frame(height: 1)
        }
        .padding(.bottom, 14)
    }
}

/// Under a finished turn's answer: the files it changed, opening the
/// changes, and for the newest, copying what it said.
private struct AnswerFoot: View {
    let turn: TurnFold.Turn
    let chat: any ChatSurface
    let latest: Bool
    let openChanges: @MainActor () -> Void
    @State private var copied = false

    var body: some View {
        let files = edited
        if !files.isEmpty || latest {
            HStack(spacing: 6) {
                if latest, !turn.answer.isEmpty {
                    Button {
                        let parts: [String] = turn.answer.compactMap { chat.row($0)?.proseText }
                        let text = parts.joined(separator: "\n\n")
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(text, forType: .string)
                        copied = true
                    } label: {
                        Icon(copied ? "check" : "copy", size: 13)
                            .foregroundStyle(.tertiary)
                            .frame(width: 24, height: 22)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(HoverButtonStyle())
                    .help(copied ? "Copied" : "Copy")
                }
                if !files.isEmpty {
                    Button(action: openChanges) {
                        HStack(spacing: 6) {
                            Icon("diff", size: 13).foregroundStyle(.secondary)
                            Text(files.count == 1 ? "Changed \(files[0].path.split(separator: "/").last ?? "")"
                                : "Changed \(files.count) files")
                            Text("+\(files.reduce(0) { $0 + $1.added })").foregroundStyle(.green)
                            Text("−\(files.reduce(0) { $0 + $1.removed })").foregroundStyle(.red)
                        }
                        .font(.system(size: 12))
                        .monospacedDigit()
                        .padding(.horizontal, 8)
                        .frame(height: 24)
                        .background(RoundedRectangle(cornerRadius: 6, style: .continuous).fill(Palette.well))
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help("Show the changes")
                }
            }
            .padding(.top, -6)
            .padding(.bottom, 18)
        }
    }

    /// Each file the turn changed, once, with its last counts.
    private var edited: [FileRow] {
        var files: [String: FileRow] = [:]
        var order: [String] = []
        for id in turn.edits {
            guard let row = chat.row(id), case .fileChange(let changed, _) = row.kind else { continue }
            for file in changed {
                if files[file.path] == nil { order.append(file.path) }
                files[file.path] = file
            }
        }
        return order.compactMap { files[$0] }
    }
}

extension Row {
    /// The words of a prose row.
    var proseText: String? {
        if case .prose(let text, _, _) = kind { return ChatWords.text(of: text) }
        return nil
    }
}
