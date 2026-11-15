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
                            } else if let changes = chat.changes, changes.files > 0 {
                                FloatingPill(changes: changes, open: actions.openChanges)
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
                ForEach(chat.ids, id: \.self) { id in
                    // The step the ask card is about is shown by the card
                    // under the feed, in full; it comes back once answered.
                    if id != chat.ask?.itemKey {
                        RowCell(chat: chat, id: id, openChanges: openChanges)
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

/// One row, read on its own so that only it redraws when it moves. A row
/// of a finished turn draws as its place in the turn says: the prompt with
/// the line over the turn's work, the turn's end as the foot under its
/// answer, and a failure the turn left in view without the run around it.
private struct RowCell: View {
    let chat: any ChatSurface
    let id: String
    let openChanges: @MainActor () -> Void

    var body: some View {
        if let row = chat.row(id), chat.shows(row) {
            switch row.turn?.part {
            case .prompt(let worked)?:
                VStack(alignment: .leading, spacing: 0) {
                    chatRow(row, placement: .own)
                    if let worked, let turn = row.turn {
                        WorkedLine(worked: worked, open: turn.open) { chat.toggleTurn(turn) }
                    }
                }
            case .end(let answer, let files)?:
                AnswerFoot(answer: answer, files: files, chat: chat, openChanges: openChanges)
            case .kept? where row.turn?.open == false:
                chatRow(row, placement: .own)
            default:
                chatRow(row, placement: RunPlacement.of(
                    row, open: row.run?.open ?? false,
                    counts: { row.run.flatMap(chat.counts(of:)) }))
            }
        }
    }

    private func chatRow(_ row: Row, placement: RunPlacement) -> some View {
        ChatRow(
            row: row, expanded: chat.isExpanded(row.id),
            rail: RailJoin.of(row, next: chat.row(below: row.id)),
            placement: placement,
            bytes: chat.bytes(of:),
            toggle: { chat.toggle(row.id) },
            toggleRun: { if let run = row.run { chat.toggleRun(run) } })
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
                HStack(spacing: 7) {
                    Working(size: 11)
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
                Text(notice).font(.system(size: 12)).foregroundStyle(Palette.failure)
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
        .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Palette.well))
    }
}

// MARK: - Answer-first turns

/// "Worked 3m 14s ›" under a finished turn's prompt, opening its work.
private struct WorkedLine: View {
    let worked: Worked
    let open: Bool
    let toggle: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Button(action: toggle) {
                HStack(spacing: 4) {
                    Text(ChatWords.turnEnd(durationMs: worked.durationMs, failed: worked.failed))
                        .foregroundStyle(worked.failed ? AnyShapeStyle(Palette.failure) : AnyShapeStyle(.secondary))
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

/// Under a finished turn's answer: copying what it said, and the files it
/// changed, opening the changes.
private struct AnswerFoot: View {
    let answer: [String]
    let files: [FileRow]
    let chat: any ChatSurface
    let openChanges: @MainActor () -> Void
    @State private var copied = false

    var body: some View {
        if !files.isEmpty || !answer.isEmpty {
            HStack(spacing: 6) {
                if !answer.isEmpty {
                    Button {
                        let parts: [String] = answer.compactMap { chat.row($0)?.proseText }
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(parts.joined(separator: "\n\n"), forType: .string)
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
                            Icon("diff", size: 13).foregroundStyle(.tertiary)
                            Text(files.count == 1 ? "Changed \(files[0].path.split(separator: "/").last ?? "")"
                                : "Changed \(files.count) files")
                            Text("+\(files.reduce(0) { $0 + $1.added })").foregroundStyle(Palette.added)
                            Text("−\(files.reduce(0) { $0 + $1.removed })").foregroundStyle(Palette.removed)
                        }
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                        .monospacedDigit()
                        .padding(.horizontal, 6)
                        .frame(height: 22)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(HoverButtonStyle())
                    .help("Show the changes (⌥⌘I)")
                }
            }
            .padding(.top, -6)
            .padding(.bottom, 18)
        }
    }
}

extension Row {
    /// The words of a prose row.
    var proseText: String? {
        if case .prose(let text, _, _) = kind { return ChatWords.text(of: text) }
        return nil
    }
}
