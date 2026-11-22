import AmuxValues
import AmuxWords
import SwiftUI

/// An open agent: its chat down the reading width, and under it the ask it
/// waits on or the composer. Where it works, its relatives and the tools
/// that open its folder sit in the window's bar above, not over the chat.
public struct ChatPane: View {
    let chat: any ChatSurface
    /// The agent's row in the fleet: its name, host and place.
    let row: FleetRow?
    let actions: AgentActions

    public init(chat: any ChatSurface, row: FleetRow?, actions: AgentActions = AgentActions()) {
        self.chat = chat
        self.row = row
        self.actions = actions
    }

    public var body: some View {
        VStack(spacing: 0) {
            Feed(chat: chat, host: host, openChanges: actions.openChanges, openAgent: actions.openAgent)
            Footer(chat: chat, row: row, host: host, openReview: actions.openChanges)
        }
    }

    private var host: String { row.map { PlaceNames.host($0.hostName) } ?? "its device" }
}

/// The rows, oldest first, following the newest while the reader is there.
private struct Feed: View {
    let chat: any ChatSurface
    let host: String
    let openChanges: @MainActor () -> Void
    let openAgent: @MainActor (AgentKey) -> Void
    @State private var position = ScrollPosition(edge: .bottom)

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                if let older = chat.olderNotice {
                    Text(older).font(.ui(.meta)).foregroundStyle(Palette.textTertiary)
                        .frame(maxWidth: .infinity)
                        .padding(.bottom, 12)
                }
                if chat.empty {
                    emptyNotice
                }
                let working = workingPrompt
                ForEach(chat.ids, id: \.self) { id in
                    // The step the ask card is about is shown by the card
                    // under the feed, in full; it comes back once answered.
                    if id != chat.ask?.itemKey {
                        RowCell(chat: chat, id: id, openChanges: openChanges, openAgent: openAgent)
                    }
                    if id == working, let activity = chat.frame?.composer.activity {
                        WorkingLine(activity: activity)
                    }
                }
                ForEach(Array(chat.landing.enumerated()), id: \.offset) { _, prompt in
                    VStack(alignment: .trailing, spacing: 2) {
                        PromptBubble(segments: prompt.text.map(\.segment), steered: false)
                        Text(ChatWords.underway(prompt.underway, host: host))
                            .font(.ui(.caption))
                            .foregroundStyle(Palette.textTertiary)
                    }
                    .padding(.bottom, 12)
                }
                ActivityLine(chat: chat)
            }
            .environment(
                \.reviewRounds,
                ReviewRounds(read: { [chat] in await chat.reviewRound($0) }, openReview: openChanges))
            .frame(maxWidth: Metrics.reading)
            .padding(.horizontal, Metrics.gutter)
            .padding(.top, 24)
            .padding(.bottom, 12)
            .frame(maxWidth: .infinity)
        }
        .scrollPosition($position)
        .defaultScrollAnchor(.bottom, for: .sizeChanges)
        // What scrolls under the panel's top fades rather than being cut.
        .overlay(alignment: .top) {
            LinearGradient(colors: [Palette.content, Palette.content.opacity(0)], startPoint: .top, endPoint: .bottom)
                .frame(height: 16)
                .allowsHitTesting(false)
        }
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

    /// The prompt the turn at work answers: the newest, while the agent
    /// works. "Working for" goes under it, where "Worked for" will.
    private var workingPrompt: String? {
        guard case .working? = chat.frame?.phase else { return nil }
        return chat.ids.last { id in
            if case .prompt? = chat.row(id)?.kind { return true }
            return false
        }
    }

    @ViewBuilder private var emptyNotice: some View {
        if chat.loadingHint {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Loading chat from \(host)…").foregroundStyle(Palette.textSecondary)
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
                AnswerFoot(answer: answer, files: files, atMs: row.atMs, chat: chat, openChanges: openChanges)
            case .kept? where row.turn?.open == false:
                chatRow(row, placement: .own)
            default:
                chatRow(row, placement: RunPlacement.of(
                    row, open: row.run?.open ?? false,
                    counts: { row.run.flatMap(chat.counts(of:)) }))
            }
        }
    }

    /// The activity, when it is this row at work: the row then carries
    /// the working dot and the time, and the activity line stands aside.
    private func live(_ row: Row) -> Activity? {
        guard let activity = chat.frame?.composer.activity, case .running(let key) = activity.kind, key == row.id
        else { return nil }
        return activity
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
        .environment(\.liveStep, live(row))
    }
}

extension ActivityKind {
    var isRunningStep: Bool {
        if case .running = self { return true }
        return false
    }

    var isSubagents: Bool {
        if case .subagents = self { return true }
        return false
    }
}

extension EnvironmentValues {
    /// The activity, for the one row it names as at work.
    @Entry var liveStep: Activity?
    /// How a review sent into the chat opens its round.
    @Entry var reviewRounds = ReviewRounds()
}

/// What a sent review's chip opens: its round, read from the agent's host,
/// and from there the review as it is now.
struct ReviewRounds {
    var read: @MainActor (SentReviewRef) async -> ReviewPage? = { _ in nil }
    var openReview: @MainActor () -> Void = {}
}

/// The time a step has been at work, counting.
struct LiveTime: View {
    let activity: Activity

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            let elapsed = activity.sinceMs > 0
                ? activity.elapsedMs + Int64(max(0, context.date.timeIntervalSince1970 * 1000 - Double(activity.sinceMs)))
                : activity.elapsedMs
            Text(ChatWords.duration(elapsed / 1_000 * 1_000)).monospacedDigit()
        }
    }
}

/// What the agent is doing now, under the newest row.
private struct ActivityLine: View {
    let chat: any ChatSurface

    var body: some View {
        // A step at work says so itself, with its time.
        if let activity = chat.frame?.composer.activity, !activity.kind.isRunningStep {
            TimelineView(.periodic(from: .now, by: 1)) { context in
                // With no start time the elapsed time stands as given.
                let elapsed = activity.sinceMs > 0
                    ? activity.elapsedMs
                        + Int64(max(0, context.date.timeIntervalSince1970 * 1000 - Double(activity.sinceMs)))
                    : activity.elapsedMs
                let parts = ChatWords.activityParts(
                    activity.kind, elapsedMs: elapsed, subject: nil,
                    sinceMs: activity.sinceMs, subagents: chat.liveSubagents)
                // The subagents' group row above says how they stand; this
                // line only says the agent waits on them, and how long.
                let words = activity.kind.isSubagents ? "Waiting on subagents" : parts.words
                let time = activity.kind.isSubagents
                    ? (elapsed > 0 ? ChatWords.duration(elapsed / 1_000 * 1_000) : nil) : parts.time
                HStack(spacing: 10) {
                    Working(size: GridLine.cell).offset(y: 1.5)
                    Text(words).foregroundStyle(Palette.textSecondary)
                    if let subject = parts.subject {
                        Text(subject).font(.mono(.meta)).foregroundStyle(Palette.textSecondary)
                    }
                    if let time {
                        Text(time).foregroundStyle(Palette.textTertiary).monospacedDigit()
                    }
                }
                .font(.ui(.body))
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
    let openReview: @MainActor () -> Void

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
                Text(ended).font(.ui(.meta)).foregroundStyle(Palette.textSecondary)
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
                    Composer(
                        chat: chat, name: row?.name ?? chat.frame?.name ?? "agent", host: host, reach: row?.hostReach,
                        openReview: openReview)
                }
            }
        }
        .frame(maxWidth: Metrics.reading)
        .padding(.horizontal, Metrics.gutter)
        .padding(.bottom, 18)
        .frame(maxWidth: .infinity)
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
                Icon("bot", size: 15).foregroundStyle(Palette.textSecondary)
                VStack(alignment: .leading, spacing: 1) {
                    Text(ChatWords.nativeRunner(chat.frame?.kind)).foregroundStyle(.primary)
                    Text(ended ?? ChatWords.nativeTakesNoMessages).font(.ui(.meta)).foregroundStyle(Palette.textSecondary)
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
                .buttonStyle(PillButtonStyle())
                .keyboardShortcut(".", modifiers: .command)
                .help("Stop this subagent (⌘.)")
        case .stopping:
            Text("Stopping…").foregroundStyle(Palette.textSecondary)
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
                    Icon("clock", size: 12).foregroundStyle(Palette.textTertiary)
                    Text(ChatWords.text(of: queued.text)).lineLimit(1)
                    Text(ChatWords.queued(queued)).foregroundStyle(Palette.textTertiary)
                    Spacer()
                    if queued.canSendNow {
                        Button("Send now") { chat.sendNow(queued) }.buttonStyle(.link)
                    }
                    if queued.canWithdraw {
                        Button("Take back") { chat.withdraw(queued) }.buttonStyle(.link)
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
                        .foregroundStyle(worked.failed ? AnyShapeStyle(Palette.failure) : AnyShapeStyle(Palette.textSecondary))
                    Icon("chevron-right", size: 12)
                        .foregroundStyle(Palette.textTertiary)
                        .rotationEffect(.degrees(open ? 90 : 0))
                }
                .font(.ui(.prose))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(open ? "Hide steps" : "Show steps")
            Rectangle().fill(Palette.line).frame(height: 1)
        }
        .padding(.top, 24)
        .padding(.bottom, 12)
    }
}

/// "Working for 21s" under the prompt of the turn at work, ticking, over a
/// rule: where the turn's "Worked for" line will stand once it ends.
private struct WorkingLine: View {
    let activity: Activity

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            TimelineView(.periodic(from: .now, by: 1)) { context in
                let elapsed = activity.sinceMs > 0
                    ? activity.turnElapsedMs
                        + Int64(max(0, context.date.timeIntervalSince1970 * 1000 - Double(activity.sinceMs)))
                    : activity.turnElapsedMs
                Text("Working for \(ChatWords.duration(elapsed / 1_000 * 1_000))")
                    .font(.ui(.prose))
                    .foregroundStyle(Palette.textSecondary)
                    .monospacedDigit()
            }
            Rectangle().fill(Palette.line).frame(height: 1)
        }
        .padding(.top, 24)
        .padding(.bottom, 12)
    }
}

/// Under a finished turn's answer: the files it changed, as a card that
/// opens them in Review, then copying what it said.
private struct AnswerFoot: View {
    let answer: [String]
    let files: [FileRow]
    /// When the turn ended.
    let atMs: Int64
    let chat: any ChatSurface
    let openChanges: @MainActor () -> Void
    @State private var copied = false

    /// The most files the card lists before it counts the rest.
    private static let listed = 6

    var body: some View {
        if !files.isEmpty || !answer.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                if !files.isEmpty {
                    changed
                }
                HStack(spacing: 8) {
                    if !answer.isEmpty {
                        Button(action: copy) {
                            Icon(copied ? "check" : "copy", size: 14, optical: false)
                                .foregroundStyle(Palette.textTertiary)
                                .frame(width: 24, height: 24)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(HoverButtonStyle())
                        .help(copied ? "Copied" : "Copy")
                    }
                    if atMs > 0 {
                        Text(Date(timeIntervalSince1970: Double(atMs) / 1000), format: .dateTime.hour().minute())
                            .font(.ui(.meta))
                            .foregroundStyle(Palette.textTertiary)
                    }
                }
            }
            .padding(.top, 2)
            .padding(.bottom, 20)
        }
    }

    private var changed: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(files.count == 1 ? "1 changed file" : "\(files.count) changed files")
                    .font(.ui(.body, weight: .medium))
                Text("+\(files.reduce(0) { $0 + $1.added })").foregroundStyle(Palette.added)
                    + Text(" −\(files.reduce(0) { $0 + $1.removed })").foregroundStyle(Palette.removed)
                Spacer(minLength: 8)
                Button(action: openChanges) {
                    HStack(spacing: 5) {
                        Icon("file-diff", size: 12)
                        Text("Open in Review").font(.ui(.meta))
                    }
                    .foregroundStyle(Palette.textSecondary)
                    .padding(.horizontal, 6)
                    .frame(height: 24)
                    .contentShape(Rectangle())
                }
                .buttonStyle(HoverButtonStyle())
                .help("Open the changes in Review (⌥⌘I)")
            }
            .font(.ui(.body))
            .monospacedDigit()
            .padding(.leading, 12)
            .padding(.trailing, 6)
            .frame(height: 40)
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(files.prefix(Self.listed).enumerated()), id: \.offset) { _, file in
                    FileLine(file: file)
                }
                if files.count > Self.listed {
                    Text("\(files.count - Self.listed) more")
                        .font(.ui(.meta))
                        .foregroundStyle(Palette.textTertiary)
                        .frame(height: 24)
                }
            }
            .padding(.horizontal, 12)
            .padding(.bottom, 8)
        }
        .background(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).fill(Palette.inset))
        .overlay(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).strokeBorder(Palette.line))
        .contentShape(Rectangle())
        .onTapGesture(perform: openChanges)
    }

    private func copy() {
        let parts: [String] = answer.compactMap { chat.row($0)?.proseText }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(parts.joined(separator: "\n\n"), forType: .string)
        copied = true
        Task {
            try? await Task.sleep(for: .seconds(1.5))
            copied = false
        }
    }
}

/// One changed file: its folder quiet and its name plain, its counts at
/// the right.
private struct FileLine: View {
    let file: FileRow

    var body: some View {
        let parts = file.path.split(separator: "/", omittingEmptySubsequences: false)
        let name = parts.last.map(String.init) ?? file.path
        let folder = parts.count > 1 ? parts.dropLast().joined(separator: "/") + "/" : ""
        HStack(spacing: 8) {
            (Text(folder).foregroundStyle(Palette.textTertiary) + Text(name))
                .font(.mono(.small))
                .lineLimit(1)
                .truncationMode(.head)
            Spacer(minLength: 8)
            Text("+\(file.added)").foregroundStyle(Palette.added)
            Text("−\(file.removed)").foregroundStyle(Palette.removed)
        }
        .font(.mono(.small))
        .monospacedDigit()
        .frame(height: 24)
    }
}

extension Row {
    /// The words of a prose row.
    var proseText: String? {
        if case .prose(let text, _, _) = kind { return ChatWords.text(of: text) }
        return nil
    }
}
