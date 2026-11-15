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
        settings: SettingsView? = nil
    ) {
        self.settings = settings
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
        return row
    }
    func shows(_ row: Row) -> Bool { !row.collapsed || row.parent.map(open.contains) == true }

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

    var canSend: Bool { true }
    var canResume: Bool {
        if case .resume? = frame?.composer.mode { return true }
        return false
    }
    var sending: Bool { false }
    var slashMatches: [CommandMatch] { [] }
    func pick(_ match: CommandMatch) {}
    func send() { draft = "" }
    func resume() { draft = "" }
    func interrupt() {}
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

    init(doc: ReviewDoc, selected: ReviewSpot? = nil) {
        self.doc = doc
        anchor = selected
        head = selected
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
