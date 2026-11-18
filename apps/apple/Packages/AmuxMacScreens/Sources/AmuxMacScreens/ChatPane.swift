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
                    openChanges: actions.openChanges, openAgent: actions.openAgent
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
    let openAgent: @MainActor (AgentKey) -> Void
    @State private var position = ScrollPosition(edge: .bottom)

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                if let older = chat.olderNotice {
                    Text(older).font(.ui(.meta)).foregroundStyle(.tertiary)
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
                        RowCell(chat: chat, id: id, openChanges: openChanges, openAgent: openAgent)
                    }
                }
                ForEach(Array(chat.landing.enumerated()), id: \.offset) { _, prompt in
                    VStack(alignment: .trailing, spacing: 2) {
                        PromptBubble(text: ChatWords.text(of: prompt.text), steered: false)
                        Text(ChatWords.underway(prompt.underway, host: host))
                            .font(.ui(.caption))
                            .foregroundStyle(.tertiary)
                    }
                    .padding(.bottom, 12)
                }
                ActivityLine(chat: chat)
            }
            .frame(maxWidth: Metrics.reading)
            .padding(.horizontal, Metrics.gutter)
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
                        .font(.ui(.meta, weight: .medium))
                        .padding(.horizontal, 12)
                        .padding(.vertical, 6)
                        .background(Capsule().fill(Palette.raised))
                        .overlay(Capsule().strokeBorder(Palette.line))
                        .elevation(.raised)
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
            .font(.ui(.body))
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
    let openAgent: @MainActor (AgentKey) -> Void

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
            opened: chat.opened(by: row),
            groupState: row.group.flatMap(chat.state(of:)),
            toggle: { chat.toggle(row.id) },
            toggleRun: { if let run = row.run { chat.toggleRun(run) } },
            toggleGroup: { if let group = row.group { chat.toggleGroup(group) } },
            openAgent: { openAgent($0) })
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
                    sinceMs: activity.sinceMs, subagents: chat.liveSubagents)
                HStack(spacing: 7) {
                    Working(size: 11)
                    Text(parts.words).foregroundStyle(.secondary)
                    if let subject = parts.subject {
                        Text(subject).font(.mono(.meta)).foregroundStyle(.secondary)
                    }
                    if let time = parts.time {
                        Text(time).foregroundStyle(.tertiary).monospacedDigit()
                    }
                }
                .font(.ui(.callout))
                .padding(.top, 2)
                .padding(.bottom, 8)
            }
        }
    }
}

/// What stands under the feed, one thing at a time, as on the phone: an
/// open ask takes the composer's place until it is answered; a chat that
/// ended says so instead; otherwise the composer, with the prompts queued
/// for the agent above it. An ask the agent exited with stays above the
/// composer as a record. The draft outlives all of them.
private struct Footer: View {
    let chat: any ChatSurface
    let row: FleetRow?
    let host: String
    let trailing: CGFloat

    var body: some View {
        VStack(spacing: 8) {
            if let notice = chat.notice {
                Text(notice).font(.ui(.meta)).foregroundStyle(Palette.failure)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if let ended = chat.frame?.ended {
                // The ask the agent ended with stays as a record above it.
                if let ask = chat.ask {
                    AskCardView(card: ask, chat: chat)
                }
                Text(ended).font(.ui(.callout)).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.vertical, 8)
            } else if let ask = chat.ask, ask.state != .dismissed {
                AskCardView(card: ask, chat: chat)
            } else {
                if let ask = chat.ask {
                    AskCardView(card: ask, chat: chat)
                }
                if let stop = chat.frame?.composer.mode.nativeStop {
                    NativeBar(chat: chat, stop: stop)
                } else {
                    if let signIn = chat.frame?.signIn {
                        SignInNotice(signIn: signIn, kind: chat.frame?.kind, host: host)
                    }
                    if let frame = chat.frame, !frame.queue.isEmpty {
                        QueueList(chat: chat, queue: frame.queue)
                    }
                    Composer(chat: chat, name: row?.name ?? chat.frame?.name ?? "agent", host: host, reach: row?.hostReach)
                }
            }
        }
        .frame(maxWidth: Metrics.reading)
        .padding(.horizontal, Metrics.gutter)
        .padding(.bottom, 18)
        .frame(maxWidth: .infinity)
        .padding(.trailing, trailing)
    }
}

/// Where the composer stands in a provider subagent's chat. Claude and
/// Codex let only the parent's model talk to a subagent they run, so there
/// is no field: one quiet bar says what runs it and that it takes no
/// messages, with its own Stop where the provider stops one subagent on
/// request. ⌘. stops it, as it stops a turn elsewhere.
private struct NativeBar: View {
    let chat: any ChatSurface
    let stop: JobStop

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                Icon("bot", size: 15).foregroundStyle(.secondary)
                VStack(alignment: .leading, spacing: 1) {
                    Text(ChatWords.nativeRunner(chat.frame?.kind)).foregroundStyle(.primary)
                    Text(ended ?? ChatWords.nativeTakesNoMessages).font(.ui(.meta)).foregroundStyle(.secondary)
                }
                Spacer(minLength: 12)
                control
            }
            if case .refused(_, let error) = stop {
                Text(error).font(.ui(.meta)).foregroundStyle(Palette.failure)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.leading, 25)
            }
        }
        .font(.ui(.body))
        .padding(.horizontal, 14)
        .padding(.vertical, 11)
        .background(RoundedRectangle(cornerRadius: Radius.composer, style: .continuous).fill(Palette.raised))
        .overlay(RoundedRectangle(cornerRadius: Radius.composer, style: .continuous).strokeBorder(Palette.line))
    }

    /// How it ended, once it has, in place of the reason there is no field.
    private var ended: String? {
        guard case .exited(let cause)? = chat.frame?.phase else { return nil }
        return ChatWords.exited(cause)
    }

    @ViewBuilder private var control: some View {
        switch stop {
        case .offered(let key), .refused(let key, _):
            Button("Stop") { chat.stopJob(key) }
                .controlSize(.regular)
                .keyboardShortcut(".", modifiers: .command)
                .help("Stop this subagent (⌘.)")
        case .stopping:
            Text("Stopping…").foregroundStyle(.secondary)
        case .unavailable:
            EmptyView()
        }
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
                .font(.ui(.meta))
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).fill(Palette.well))
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
                .font(.ui(.body))
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
                        .font(.ui(.meta))
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
