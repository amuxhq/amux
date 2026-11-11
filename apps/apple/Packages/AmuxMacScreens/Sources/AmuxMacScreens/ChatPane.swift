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
    let inspectorOpen: Bool
    let openInspector: @MainActor (InspectorTab) -> Void

    public init(
        chat: any ChatSurface, row: FleetRow?, inspectorOpen: Bool,
        openInspector: @escaping @MainActor (InspectorTab) -> Void = { _ in }
    ) {
        self.chat = chat
        self.row = row
        self.inspectorOpen = inspectorOpen
        self.openInspector = openInspector
    }

    public var body: some View {
        GeometryReader { pane in
            let placing = CardPlacement(width: pane.size.width, shown: !inspectorOpen && row != nil)
            let beside = placing != .pill
            VStack(spacing: 0) {
                Feed(chat: chat, host: host, roomAbove: placing == .pill, trailing: placing.trailing)
                    .overlay(alignment: .topTrailing) {
                        if !inspectorOpen, let row {
                            Group {
                                if beside {
                                    FloatingCard(
                                        row: row, branch: chat.frame?.git?.branch ?? row.branch,
                                        changes: chat.changes, open: openInspector)
                                } else {
                                    FloatingPill(changes: chat.changes, open: openInspector)
                                }
                            }
                            .padding(.top, 8)
                            .padding(.trailing, 14)
                            .transition(.opacity.combined(with: .move(edge: .trailing)))
                        }
                    }
                Footer(chat: chat, row: row, host: host, trailing: placing.trailing)
            }
        }
        .animation(.snappy(duration: 0.18), value: inspectorOpen)
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
                    RowCell(chat: chat, id: id)
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
            .padding(.horizontal, 28)
            .padding(.top, roomAbove ? 48 : 16)
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
                    Label(chat.frame?.arrivalsHeld == true ? "New activity" : "Latest", systemImage: "arrow.down")
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
        .padding(.horizontal, 28)
        .padding(.bottom, 16)
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
                    Image(systemName: "clock").font(.system(size: 11)).foregroundStyle(.tertiary)
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
