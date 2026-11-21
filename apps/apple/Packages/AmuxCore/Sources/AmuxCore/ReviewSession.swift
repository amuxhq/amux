import AmuxValues
import AmuxWords
import Foundation
import Observation

/// The agent's review page as both Apple apps draw it: the page the shared
/// runtime composes, handed over whole each time it moves, and what the
/// person does on it, sent to the agent's host.
///
/// Nothing here works the page out again. Its rows, banners and header are
/// drawn as handed; what the page still needs is read from its
/// `toFetch` and asked for; and a selection of rows becomes the lines a
/// comment is on through the runtime, which counts the rows of the page it
/// handed over last. A selection is held by its rows' keys, which name a
/// row across compositions, so a page that moves under it keeps it on the
/// same rows or drops it when they go.
///
/// The chat model owns one while its page is open and hands it every page
/// the chat's deltas carry. Each act returns the task that waits on the
/// agent's host, which answers what it came to; a failure the page itself
/// does not say is put into `notice`.
@MainActor
@Observable
public final class ReviewSession {
    /// What the runtime draws before the review's summary arrives: the page
    /// opening, with nothing wrong.
    public static let opening = ReviewPage(
        banners: [], rows: [], toFetch: [], opening: true, empty: nil, header: nil)

    @ObservationIgnored public let source: ChatSource
    /// The page as the runtime handed it last.
    public private(set) var page: ReviewPage
    /// The rows selected, first to last, by index into `page.rows`.
    public private(set) var selected: ClosedRange<Int>?
    /// Where a comment on the selection goes; nil with nothing selected,
    /// and for a selection with no line or across files.
    public private(set) var anchor: ReviewAnchor?
    /// Acts on their way to the agent's host.
    public private(set) var waiting = 0
    /// Why the last act came to nothing, when the page does not say it
    /// itself; cleared by the next act.
    public private(set) var notice: String?
    /// The page was closed: nothing more is asked of the host for it.
    public private(set) var closed = false

    /// Where the selection started and the row it reaches, by key.
    @ObservationIgnored private var from: String?
    @ObservationIgnored private var to: String?
    /// Each row's index in the page, by key.
    @ObservationIgnored private var places: [String: Int] = [:]
    /// The files asked for, each as the comparison (the commits and view) and
    /// path its body is read for, so a page that moves before they land asks
    /// nothing twice.
    /// A fetch that failed stays here until the page is opened again or the
    /// host can be reached again, so a failing host is not asked in a loop.
    @ObservationIgnored private var asked: Set<Asked> = []
    /// The newest act, so a test can wait for its answer rather than guess
    /// how long the host takes.
    @ObservationIgnored private(set) var latest: Task<ReviewOutcome?, Never>?

    /// A page on the review open on `source`'s chat, starting from the page
    /// the runtime holds there, if one is open already.
    public init(source: ChatSource) {
        self.source = source
        page = source.reviewPage() ?? Self.opening
        placeRows()
    }

    // MARK: - Reading

    public func isSelected(_ row: Int) -> Bool {
        selected?.contains(row) ?? false
    }

    /// A comment can be written on what is selected.
    public var canComment: Bool { anchor != nil && !closed }

    // MARK: - Following the chat

    /// The page as the runtime handed it now, from the chat's delta. The
    /// selection is kept on its rows, and the files the page still needs
    /// are asked for.
    func apply(_ page: ReviewPage) {
        let unreachable = self.page.banners.contains(.unreachable)
        if page != self.page { self.page = page }
        placeRows()
        // The host answers again: what failed to come from it is asked for
        // once more.
        if unreachable, !page.banners.contains(.unreachable) { asked.removeAll() }
        if from != nil { resolve() }
        fetchWanted()
    }

    private func placeRows() {
        places = Dictionary(
            page.rows.enumerated().map { ($1.key, $0) }, uniquingKeysWith: { first, _ in first })
    }

    // MARK: - Opening

    /// Opens the review at the agent's host for this look: on a new snapshot
    /// when the tree changed and no drafts wait, else on the drafts' own,
    /// which the page says has changed since. `base` picks the branch the
    /// change is counted against anew.
    @discardableResult
    public func open(base: String? = nil) -> Task<ReviewOutcome?, Never>? {
        reopen(base: base, refresh: false)
    }

    /// Takes a new snapshot even with drafts, moving them onto it; the page
    /// says how many moved.
    @discardableResult
    public func refresh() -> Task<ReviewOutcome?, Never>? {
        reopen(base: nil, refresh: true)
    }

    private func reopen(base: String?, refresh: Bool) -> Task<ReviewOutcome?, Never>? {
        asked.removeAll()
        return act(changing: false) { await $0.openReview(baseBranch: base, refresh: refresh) }
    }

    // MARK: - Reading more

    /// Asks for every file the page lists as needed and not yet asked for,
    /// in page order, which is reading order: the runtime asks the host in
    /// batches and the page moves as each lands, the first on screen first.
    private func fetchWanted() {
        guard !closed, let header = page.header else { return }
        let keys = page.toFetch
            .map { Asked(from: header.comparedWith, snapshot: header.snapshot, view: header.view, path: $0) }
            .filter { !asked.contains($0) }
        guard !keys.isEmpty else { return }
        asked.formUnion(keys)
        let paths = keys.map(\.path)
        let source = self.source
        let task = Task { [weak self] () -> ReviewOutcome? in
            let outcome = await source.reviewFiles(paths)
            guard let self else { return outcome }
            if outcome == .done {
                // Fetched for whichever view the runtime showed when they
                // were asked, which a choice may have moved since: one a
                // later page still needs is asked for again. The page that
                // says what landed came before this answer, so nothing is
                // asked again for it now.
                self.asked.subtract(keys)
            } else if case .failed = outcome {
                // The host's own words. A host out of reach, or a review
                // that cannot be had, the page's banners say instead.
                self.notice = ChatWords.reviewNotice(outcome)
            }
            return outcome
        }
        latest = task
    }

    /// A file's body as asked for: the comparison it is read for.
    private struct Asked: Hashable {
        var from: String
        var snapshot: String
        var view: ReviewView
        var path: String
    }

    /// Loads the lines one of a gap's asks names; the gap says it is loading
    /// until they arrive.
    @discardableResult
    public func moreContext(_ ask: ReviewContextAsk) -> Task<ReviewOutcome?, Never>? {
        act(changing: false) { await $0.reviewContext(ask) }
    }

    // MARK: - Choosing

    /// Switches to one of the views the header offers.
    public func show(_ view: ReviewView) {
        choose(.view(view))
    }

    /// Folds an open file or opens a folded one.
    public func toggleFile(_ path: String) {
        choose(.toggleFile(path))
    }

    /// Opens a folded outdated comment, or folds it again.
    public func toggleComment(_ id: [UInt8]) {
        choose(.toggleComment(id))
    }

    private func choose(_ choice: ReviewChoice) {
        guard !closed else { return }
        source.chooseInReview(choice)
    }

    // MARK: - Selecting

    /// Starts a selection on one row: a line, or a file's head for a
    /// comment on the whole file.
    public func select(_ row: Int) {
        guard page.rows.indices.contains(row) else { return }
        from = page.rows[row].key
        to = from
        resolve()
    }

    /// Extends the selection to the row reached; across files it holds no
    /// lines, and nothing can be written on it.
    public func extend(to row: Int) {
        guard from != nil, page.rows.indices.contains(row) else { return }
        to = page.rows[row].key
        resolve()
    }

    public func clearSelection() {
        from = nil
        to = nil
        if selected != nil { selected = nil }
        if anchor != nil { anchor = nil }
    }

    /// The selection as indices into the page handed over last, and the
    /// lines the runtime says they are; a row that left the page ends it.
    private func resolve() {
        guard let from, let to, let first = places[from], let last = places[to] else {
            clearSelection()
            return
        }
        let range = min(first, last)...max(first, last)
        if selected != range { selected = range }
        let anchor = source.reviewAnchor(from: first, to: last)
        if self.anchor != anchor { self.anchor = anchor }
    }

    // MARK: - Commenting

    /// Writes a draft on the selection; once the host has it the selection
    /// ends. Blank words write nothing.
    @discardableResult
    public func comment(_ text: String) -> Task<ReviewOutcome?, Never>? {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, let anchor, !closed else { return nil }
        let selection = (from, to)
        return act(done: { [weak self] in
            guard let self, (self.from, self.to) == selection else { return }
            self.clearSelection()
        }) { await $0.editReview([.comment(anchor: anchor, text: text)]) }
    }

    /// New words for a comment, draft or sent; it stays where it was
    /// written. Blank words change nothing.
    @discardableResult
    public func reword(_ id: [UInt8], as text: String) -> Task<ReviewOutcome?, Never>? {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return nil }
        return act { await $0.editReview([.reword(id: id, text: text)]) }
    }

    /// Deletes a comment, draft or sent; the chat keeps a sent one.
    @discardableResult
    public func delete(_ id: [UInt8]) -> Task<ReviewOutcome?, Never>? {
        act { await $0.editReview([.delete(id)]) }
    }

    /// Deletes every draft, leaving what was sent, once the person said so.
    @discardableResult
    public func discardDrafts() -> Task<ReviewOutcome?, Never>? {
        act { await $0.editReview([.discardDrafts]) }
    }

    /// Deletes the review, once the person said so: the next look is a
    /// first look.
    @discardableResult
    public func startOver() -> Task<ReviewOutcome?, Never>? {
        act { await $0.editReview([.startOver]) }
    }

    // MARK: - Closing

    /// The page left the screen: the chat stops following the review for
    /// it. Acts already on their way are answered all the same.
    public func close() {
        guard !closed else { return }
        closed = true
        clearSelection()
        source.closeReview()
    }

    // MARK: - Acting

    /// Starts an act on the agent's host; what it moved arrives in the
    /// chat's deltas before its answer does. An act that changes nothing
    /// says no more of a host out of reach than the page's banner does.
    private func act(
        changing: Bool = true,
        done: (@MainActor () -> Void)? = nil,
        _ body: @escaping @Sendable (ChatSource) async -> ReviewOutcome?
    ) -> Task<ReviewOutcome?, Never>? {
        guard !closed else { return nil }
        notice = nil
        waiting += 1
        let source = self.source
        let task = Task { [weak self] () -> ReviewOutcome? in
            let outcome = await body(source)
            guard let self else { return outcome }
            self.waiting -= 1
            if outcome == .done { done?() }
            if changing || outcome != .unreachable { self.notice = ChatWords.reviewNotice(outcome) }
            return outcome
        }
        latest = task
        return task
    }
}
