import AmuxCore
import AmuxMacScreens
import AmuxValues
import Foundation
import Observation
import UniformTypeIdentifiers

/// The shared chat model as the Mac's screens read it.
///
/// Every read goes straight to the model, so the screens observe exactly
/// what they read: one row's cell, the frame, the draft. What the Mac adds
/// is the review's loading, which the phone keeps on its review page.
@MainActor
@Observable
final class MacChat: ChatSurface {
    @ObservationIgnored let model: ChatModel
    /// The host's name, for what the chat says about reaching it.
    @ObservationIgnored let host: String
    private(set) var review: ReviewState = .idle
    @ObservationIgnored private var loading: Task<Void, Never>?
    /// The comparison the review on show was read for.
    @ObservationIgnored private var loadedFor: Comparison?

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
    var canResume: Bool { model.canResume }
    var sending: Bool { model.sending }
    var slashMatches: [CommandMatch] { model.slashMatches }
    func pick(_ match: CommandMatch) { model.pick(match) }
    func send() { _ = model.send() }
    func resume() { _ = model.resume() }
    func interrupt() { _ = model.interrupt() }
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

    /// Freezes the changes as they stand on the host. A review already being
    /// written on the same patch is kept, comments and all.
    func loadReview(keepingComments: Bool) {
        // Held here rather than in the pane, so switching tabs or closing
        // the pane does not forget there are comments to keep.
        if keepingComments, loadedFor == model.comparison, case .ready(let open) = review,
            open.count > 0 || open.selecting
        {
            return
        }
        loadedFor = model.comparison
        loading?.cancel()
        if case .ready = review {} else { review = .loading }
        loading = Task { [weak self] in
            guard let self else { return }
            let opened = await model.openReview()
            guard !Task.isCancelled else { return }
            switch opened {
            case .success(let model?): review = .ready(MacReview(model: model))
            case .success(nil): review = .nothing
            case .failure(let failure): review = .failed(failure.description)
            }
        }
    }

    func attachReview() {
        guard case .ready(let surface) = review, let open = surface as? MacReview else { return }
        model.attach(open.model)
    }

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

/// The shared review model as the Mac's screens read it.
@MainActor
final class MacReview: ReviewSurface {
    let model: ReviewModel

    init(model: ReviewModel) { self.model = model }

    var doc: ReviewDoc { model.doc }
    var count: Int { model.count }
    func folded(_ file: Int) -> Bool { model.folded.contains(file) }
    func toggle(file: Int) { model.toggle(file: file) }
    func isSelected(_ spot: ReviewSpot) -> Bool { model.isSelected(line(spot)) }
    var selecting: Bool { model.selection != nil }
    func begin(at spot: ReviewSpot) { model.begin(at: line(spot)) }
    func extend(to spot: ReviewSpot) { model.extend(to: line(spot)) }
    func clearSelection() { model.clearSelection() }
    func comment(_ text: String) -> Bool { model.comment(text) }
    var canAttach: Bool { model.canAttach }

    private func line(_ spot: ReviewSpot) -> ReviewLine {
        ReviewLine(file: spot.file, hunk: spot.hunk, line: spot.line)
    }
}
