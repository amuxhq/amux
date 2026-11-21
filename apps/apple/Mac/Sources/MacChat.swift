import AmuxCore
import AmuxMacScreens
import AmuxValues
import Foundation
import Observation
import UniformTypeIdentifiers

/// The shared chat model as the Mac's screens read it.
///
/// Every read goes straight to the model, so the screens observe exactly
/// what they read: one row's cell, the frame, the draft.
@MainActor
@Observable
final class MacChat: ChatSurface {
    @ObservationIgnored let model: ChatModel
    /// The host's name, for what the chat says about reaching it.
    @ObservationIgnored let host: String

    init(model: ChatModel, host: String) {
        self.model = model
        self.host = host
        // A desktop reads each finished turn answer first.
        model.foldTurns(true)
    }

    // MARK: The rows

    var ids: [String] {
        _ = model.sequence
        return model.ids
    }

    func row(_ id: String) -> Row? { model.cell(for: id).row }
    func shows(_ row: Row) -> Bool { model.shows(row) }
    func row(below id: String) -> Row? { model.row(below: id) }
    func isExpanded(_ id: String) -> Bool { model.isExpanded(id) }
    func toggle(_ id: String) { model.toggle(id) }
    func toggleRun(_ run: Run) { model.toggleRun(run) }
    func toggleGroup(_ group: SubagentGroup) { model.toggleGroup(group) }
    func opened(by row: Row) -> OpenedAgent? { model.opened(by: row) }
    func state(of group: SubagentGroup) -> GroupState? { model.state(of: group) }
    var liveSubagents: GroupState { model.liveSubagents }
    func toggleTurn(_ turn: TurnPlace) { model.toggleTurn(turn) }
    func counts(of run: Run) -> RunCounts? { model.counts(of: run) }
    func bytes(of blob: BlobRef) -> Data? { model.bytes(of: blob) }

    // MARK: Around the rows

    var frame: ChatFrame? { model.frame }
    var ask: AskCard? { model.ask }
    var empty: Bool { model.empty }
    var loadingHint: Bool { model.loadingHint }

    var olderNotice: String? {
        switch model.paging {
        case .idle: nil
        case .fetching: "Loading older messages…"
        case .unreachable: "Can’t reach \(host) for older messages"
        case .failed(let reason): reason
        }
    }

    var notice: String? { model.notice }
    var following: Bool { model.following }
    var toNewest: Int { model.toNewest }
    func reading(atNewest: Bool) { model.reading(atNewest: atNewest) }
    func jumpToNewest() { model.jumpToNewest() }
    func reachedTop() { model.reachedTop() }
    var landing: [SentPrompt] { model.landingInFeed }

    // MARK: The composer

    var draft: String {
        get { model.draft }
        // Through the model's typing, so a long paste becomes an attachment
        // as it does on the phone.
        set { model.type(newValue) }
    }

    var attachments: [DraftAttachment] { model.attachments }
    func removeAttachment(at index: Int) { model.removeAttachment(at: index) }
    var canSend: Bool { model.canSend }
    var hasMessage: Bool { model.hasMessage }
    var canResume: Bool { model.canResume }
    var sending: Bool { model.sending }
    var slashMatches: [CommandMatch] { model.slashMatches }
    func pick(_ match: CommandMatch) { model.pick(match) }
    func send() { _ = model.send() }
    func resume() { _ = model.resume() }
    func interrupt() { _ = model.interrupt() }
    func stopJob(_ key: String) { _ = model.stopJob(key) }
    func withdraw(_ queued: QueuedRow) { _ = model.withdraw(queued) }
    func sendNow(_ queued: QueuedRow) { _ = model.sendNow(queued) }

    // MARK: Asks

    func answer(choice: Int, note: String?) { _ = model.answer(choice: choice, note: note) }
    func answer(responses: [QuestionResponse]) { _ = model.answer(responses: responses) }
    func replyInstead(_ text: String, soFar: [QuestionResponse]) { _ = model.replyInstead(text, soFar: soFar) }

    // MARK: Changes

    var changes: ChangeTotals? { model.changes }
    var base: String? { model.base }
    var comparison: Comparison { model.comparison }
    var comparisons: [Comparison] { model.comparisons }
    func compare(_ comparison: Comparison) { model.compare(comparison) }

    // MARK: The agent's review

    /// The page drawn for the review open on the chat, kept for as long as
    /// that review is: what the person is writing on it lives here.
    @ObservationIgnored private var shown: MacReviewPage?

    /// A host without reviews has the page say so.
    var reviewPage: (any ReviewPageSurface)? {
        guard let session = model.reviewPage else { return nil }
        if shown?.session !== session { shown = MacReviewPage(session: session) }
        return shown
    }

    func openReviewPage() { model.openReviewPage() }
    func closeReviewPage() { model.closeReviewPage() }
    var reviewChip: ReviewChip? { model.reviewChip }
    func discardReviewDrafts() { model.discardReviewDrafts() }
    func reviewRound(_ sent: SentReviewRef) async -> ReviewPage? { await model.reviewRound(sent) }

    // MARK: Attaching and settings

    /// Each file is read here and stored on the agent's host; one that
    /// cannot be read is said in the chat's notice by the model.
    func attach(files: [URL]) {
        for url in files {
            guard let data = try? Data(contentsOf: url) else { continue }
            let type = UTType(filenameExtension: url.pathExtension)
            model.attach(
                data, name: url.lastPathComponent, mime: type?.preferredMIMEType ?? "application/octet-stream",
                image: type?.conforms(to: .image) ?? false)
        }
    }

    var settings: SettingsView? { model.settings }
    func change(_ setting: SettingChange) { model.change(setting) }
}

/// The shared review page as the Mac's screens read it, and the comment
/// being written on it.
@MainActor
@Observable
final class MacReviewPage: ReviewPageSurface {
    @ObservationIgnored let session: ReviewSession
    var writing = false
    var words = ""

    init(session: ReviewSession) { self.session = session }

    var page: ReviewPage { session.page }
    var selected: ClosedRange<Int>? { session.selected }
    var anchor: ReviewAnchor? { session.anchor }
    var waiting: Int { session.waiting }
    var notice: String? { session.notice }

    func refresh() { session.refresh() }
    func moreContext(_ ask: ReviewContextAsk) { session.moreContext(ask) }
    func show(_ view: ReviewView) { session.show(view) }
    func toggleFile(_ path: String) { session.toggleFile(path) }
    func toggleComment(_ id: [UInt8]) { session.toggleComment(id) }
    func select(_ row: Int) { session.select(row) }
    func extend(to row: Int) { session.extend(to: row) }

    func clearSelection() {
        session.clearSelection()
        writing = false
        words = ""
    }

    func comment(_ text: String) {
        guard let task = session.comment(text) else { return }
        Task { [weak self] in
            // Written: the field closes with the selection. Not: the words
            // stay to try again, with the page's notice saying why.
            guard await task.value == .done, let self, self.session.selected == nil else { return }
            self.writing = false
            self.words = ""
        }
    }

    func reword(_ id: [UInt8], as text: String) { session.reword(id, as: text) }
    func delete(_ id: [UInt8]) { session.delete(id) }
    func discardDrafts() { session.discardDrafts() }
    func startOver() { session.startOver() }
}
