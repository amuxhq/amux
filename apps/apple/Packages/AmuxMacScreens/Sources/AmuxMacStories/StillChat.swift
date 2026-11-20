import AmuxMacScreens
import AmuxValues
import Foundation
import Observation

/// A chat that holds still: rows, a frame and perhaps an ask, for a story.
/// What the person does to it changes what it shows, so a story can be
/// clicked through, but nothing is sent anywhere.
@MainActor
@Observable
final class StillChat: ChatSurface {
    private var rows: [Row]
    private var open: Set<String> = []
    /// The finished turns opened, by their ends.
    private var openTurns: Set<String> = []
    /// The groups of calls opened, by their first calls.
    private var openGroups: Set<String> = []
    /// What the fleet says of the agents the rows open, and of each group.
    private let agents: ChatAgents
    var frame: ChatFrame?
    var ask: AskCard?
    var draft = ""
    var attachments: [DraftAttachment] = []
    func removeAttachment(at index: Int) { attachments.remove(at: index) }
    func attach(files: [URL]) {}
    var settings: SettingsView?
    func change(_ setting: SettingChange) {}
    var loadingHint = false
    var landing: [SentPrompt] = []
    var changes: ChangeTotals?

    init(
        rows: [Row], frame: ChatFrame?, ask: AskCard? = nil, changes: ChangeTotals? = nil, loading: Bool = false,
        settings: SettingsView? = nil, agents: ChatAgents = ChatAgents(
            agents: [], groups: [],
            live: GroupState(
                agents: 0, running: 0, needsYou: 0, idle: 0, finished: 0, ended: 0, failed: 0, unknown: 0))
    ) {
        self.settings = settings
        self.agents = agents
        self.rows = rows
        self.frame = frame
        self.ask = ask
        self.changes = changes
        self.loadingHint = loading
    }

    var ids: [String] { rows.map(\.id) }

    func toggleTurn(_ turn: TurnPlace) {
        if openTurns.remove(turn.end) == nil { openTurns.insert(turn.end) }
    }
    /// A row as the runtime hands it over: a story's turn placements are
    /// authored closed, and its work folds until the turn is opened.
    func row(_ id: String) -> Row? {
        guard var row = rows.first(where: { $0.id == id }) else { return nil }
        if let end = row.turn?.end {
            let opened = openTurns.contains(end)
            row.turn?.open = opened
            if row.turn?.part == .work, !opened { row.collapsed = true }
        }
        if let first = row.group?.first {
            let opened = openGroups.contains(first)
            row.group?.open = opened
            if !opened, row.id != first { row.collapsed = true }
        }
        return row
    }
    func shows(_ row: Row) -> Bool {
        if let turn = row.turn, !turn.open, turn.part == .work { return false }
        return !row.collapsed || row.parent.map(open.contains) == true
    }

    func row(below id: String) -> Row? {
        guard let at = rows.firstIndex(where: { $0.id == id }) else { return nil }
        return rows[(at + 1)...].lazy.compactMap { self.row($0.id) }.first { row in
            if case .hidden = row.kind { return false }
            return shows(row)
        }
    }

    func isExpanded(_ id: String) -> Bool { open.contains(id) }

    func toggle(_ id: String) {
        if open.contains(id) { open.remove(id) } else { open.insert(id) }
    }

    func toggleRun(_ run: Run) {}
    func toggleGroup(_ group: SubagentGroup) {
        if openGroups.remove(group.first) == nil { openGroups.insert(group.first) }
    }
    func opened(by row: Row) -> OpenedAgent? {
        row.opens.flatMap { id in agents.agents.first { $0.agent.agent == id } }
    }
    func state(of group: SubagentGroup) -> GroupState? {
        agents.groups.first { $0.first == group.first }?.state
    }
    var liveSubagents: GroupState { agents.live }
    func counts(of run: Run) -> RunCounts? { row(run.last)?.run?.counts }
    func bytes(of blob: BlobRef) -> Data? { nil }

    var empty: Bool { rows.isEmpty }
    var olderNotice: String? { nil }
    var notice: String? { nil }
    var following: Bool { true }
    var toNewest: Int { 0 }
    func reading(atNewest: Bool) {}
    func jumpToNewest() {}
    func reachedTop() {}

    var canSend: Bool {
        if case .native? = frame?.composer.mode { return false }
        return true
    }
    var canResume: Bool {
        if case .resume? = frame?.composer.mode { return true }
        return false
    }
    var hasMessage: Bool {
        !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty || reviewChip != nil
    }
    var sending: Bool { false }
    var slashMatches: [CommandMatch] { [] }
    func pick(_ match: CommandMatch) {}
    func send() { draft = "" }
    func resume() { draft = "" }
    func interrupt() {}
    func stopJob(_ key: String) {}
    func withdraw(_ queued: QueuedRow) {}
    func sendNow(_ queued: QueuedRow) {}
    func answer(choice: Int, note: String?) { ask = nil }
    func answer(responses: [QuestionResponse]) { ask = nil }
    func replyInstead(_ text: String, soFar: [QuestionResponse]) { ask = nil }
    var base: String? { "main" }
    var comparison: Comparison = .uncommitted
    var comparisons: [Comparison] { [.uncommitted, .onBranch] }
    func compare(_ comparison: Comparison) { self.comparison = comparison }
    var review: ReviewState = .idle
    /// The review a story shows once the pane asks for it.
    var reviewing: StillReview?
    func loadReview(keepingComments: Bool) {
        if keepingComments, case .ready = review { return }
        review = reviewing.map { .ready($0) } ?? .nothing
    }
    func attachReview() {}

    /// The agent's review page a story shows, and the chip its drafts put
    /// in the composer.
    var page: StillReviewPage?
    var reviewPage: (any ReviewPageSurface)? { page }
    func openReviewPage() {}
    func closeReviewPage() {}
    var reviewChip: ReviewChip?
    func discardReviewDrafts() { reviewChip = nil }
    /// The round a sent review in the story's chat opens.
    var round: ReviewPage?
    func reviewRound(_ sent: SentReviewRef) async -> ReviewPage? { round }
}

/// The agent's review page holding still, as the runtime would hand it:
/// lines can be picked and a comment opened on them, but nothing is
/// written anywhere.
@MainActor
@Observable
final class StillReviewPage: ReviewPageSurface {
    var page: ReviewPage
    private(set) var selected: ClosedRange<Int>?
    private var from: Int?
    var writing: Bool
    var words: String
    var waiting = 0
    var notice: String?

    init(page: ReviewPage, selected: ClosedRange<Int>? = nil, words: String? = nil) {
        self.page = page
        self.selected = selected
        from = selected?.lowerBound
        writing = words != nil
        self.words = words ?? ""
    }

    /// The selection's lines, worked out as the runtime does for a story:
    /// a file's head alone is the whole file, else the snapshot's lines of
    /// one file.
    var anchor: ReviewAnchor? {
        guard let selected, let header = page.header else { return nil }
        let rows = selected.map { page.rows[$0].of }
        if rows.count == 1, case .file(let file) = rows[0] {
            return ReviewAnchor(commit: header.snapshot, path: file.path, start: 0, end: 0)
        }
        let lines: [ReviewCodeLine] = rows.compactMap {
            if case .line(let line) = $0 { return line }
            return nil
        }
        guard let path = lines.first?.path, lines.allSatisfy({ $0.path == path }) else { return nil }
        let numbers = lines.compactMap { $0.newLine ?? $0.oldLine }
        guard let start = numbers.min(), let end = numbers.max() else { return nil }
        return ReviewAnchor(commit: header.snapshot, path: path, start: start, end: end)
    }

    func refresh() {}
    func moreContext(_ ask: ReviewContextAsk) {}
    func show(_ view: ReviewView) {}
    func toggleFile(_ path: String) {}
    func toggleComment(_ id: [UInt8]) {}

    func select(_ row: Int) {
        from = row
        selected = row...row
    }

    func extend(to row: Int) {
        guard let from else { return }
        selected = min(from, row)...max(from, row)
    }

    func clearSelection() {
        from = nil
        selected = nil
        writing = false
        words = ""
    }

    func comment(_ text: String) { clearSelection() }
    func reword(_ id: [UInt8], as text: String) {}
    func delete(_ id: [UInt8]) {}
    func discardDrafts() {}
    func startOver() {}
}

/// A review that holds still, but takes comments.
@MainActor
@Observable
final class StillReview: ReviewSurface {
    var doc: ReviewDoc
    private var shut: Set<Int> = []
    private var anchor: ReviewSpot?
    private var head: ReviewSpot?
    private(set) var count = 0

    init(doc: ReviewDoc, selected: ReviewSpot? = nil, folded: Set<Int> = []) {
        self.doc = doc
        anchor = selected
        head = selected
        shut = folded
    }

    func folded(_ file: Int) -> Bool { shut.contains(file) }
    func toggle(file: Int) {
        if shut.contains(file) { shut.remove(file) } else { shut.insert(file) }
    }

    func isSelected(_ spot: ReviewSpot) -> Bool {
        guard let anchor, let head else { return false }
        return min(anchor, head) <= spot && spot <= max(anchor, head)
    }

    var selecting: Bool { anchor != nil }
    func begin(at spot: ReviewSpot) {
        anchor = spot
        head = spot
    }
    func extend(to spot: ReviewSpot) { head = spot }
    func clearSelection() {
        anchor = nil
        head = nil
    }

    func comment(_ text: String) -> Bool {
        guard let anchor, let head, !text.isEmpty else { return false }
        let at = max(anchor, head)
        doc.files[at.file].hunks[at.hunk].lines[at.line].comments.append(text)
        count += 1
        clearSelection()
        return true
    }

    var canAttach: Bool { count > 0 && anchor == nil }
}
