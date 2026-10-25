import AmuxValues
import Foundation
import Observation

/// What a chat screen reads and acts on: an open chat on the runtime, or a
/// scripted stand-in where there is no runtime to open one on. The chat
/// opens whole; after that, whoever owns the model hands it each
/// `ChatDelta` the source moves by.
public protocol ChatSource: AnyObject, Sendable {
    /// The chat whole, for a model opening on it: every key, the frame and
    /// the views around it. Every delta after it says what moved since.
    func opening() -> ChatDelta
    /// Where the reader is: at the newest row, or in history.
    func follow(_ following: Bool)
    /// Rows for these keys, each run folded to its newest step unless the
    /// reader opened it: for a cell drawn for the first time, or after a
    /// reload.
    func rows(for keys: [String]) -> [Row]
    func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure>
    func answer(_ ask: String, choice: Int, note: String?) async -> ActOutcome?
    func answer(_ ask: String, responses: [QuestionResponse]) async -> ActOutcome?
    func replyInstead(_ ask: String, text: String, soFar: [QuestionResponse]) async -> ActOutcome?
    /// Opens or closes the run `member` sits in; the next delta carries its
    /// rows drawn so, and it stays open as it grows.
    func toggleRun(_ member: String)
    func answerForm(_ ask: String, values: [FormValue]) async -> ActOutcome?
    func withdraw(_ input: [UInt8]) async -> ActOutcome?
    /// A queued or sent prompt as the draft it came from, attachments whole.
    func draft(of input: [UInt8]) -> Draft?
    func sendNow(_ input: [UInt8]) async -> ActOutcome?
    func resend(_ input: [UInt8]) async -> SendOutcome?
    func discard(_ input: [UInt8])
    func interrupt() async -> ActOutcome?
    func change(_ setting: SettingChange) async -> ActOutcome?
    func resume(with draft: Draft) async -> ActOutcome?
    func pageOlder(_ rows: UInt32) async -> PageOutcome?
    func putBlob(_ data: Data, name: String, mime: String) async -> Result<BlobRef, RuntimeFailure>
    func blob(_ hash: [UInt8]) -> Data?
    func review(_ comparison: Comparison) async -> Result<FrozenReview, RuntimeFailure>
    /// Fetches the files changed for `comparison`; the overview lists them
    /// from then on.
    func openOverview(_ comparison: Comparison) async -> Result<Overview, RuntimeFailure>
    /// Runs a draft that names a command the client runs itself (`/model`,
    /// `/effort`, `/rename`) or one the agent does not run; `.send` says to
    /// send it as typed.
    func command(_ draft: Draft) async -> CommandOutcome?
    /// Brings terminal Claude's own view back to this agent's conversation.
    func switchBack() async -> ActOutcome?
    /// The draft this device kept for the agent when the chat opened.
    func keptDraft() -> Draft?
    /// Keeps the draft as it now stands, past the chat and the app.
    func keep(_ draft: Draft)
}

extension ChatSource {
    /// A source with no client commands of its own sends every draft.
    public func command(_ draft: Draft) async -> CommandOutcome? { .send }
    /// A source whose agent never leaves its conversation has nothing to
    /// switch back to.
    public func switchBack() async -> ActOutcome? { .done }
    /// A source that keeps no drafts opens with none and forgets what is
    /// typed.
    public func keptDraft() -> Draft? { nil }
    public func keep(_ draft: Draft) {}
}

/// What a client command asks of the screen: a picker or the rename card.
public enum ChatAsk: Equatable, Sendable {
    case settings
    case rename
}

/// One row of the list, held by its key. A cell observes only its own row,
/// so a change to one item redraws one cell.
@MainActor
@Observable
public final class RowCell: Identifiable {
    public let id: String
    public fileprivate(set) var row: Row?
    /// The chat's revision when the row was last read: the list compares
    /// it with the revision it last laid out to find the rows to measure
    /// again, so a row read twice before the list looks is not missed.
    @ObservationIgnored public fileprivate(set) var revision: Int

    init(id: String, row: Row?, revision: Int) {
        self.id = id
        self.row = row
        self.revision = revision
    }
}

/// Asking for older rows, as the top of the list says it.
public enum ChatPaging: Equatable, Sendable {
    case idle
    case fetching
    /// Older history is held only by the agent's machine, which cannot be
    /// reached; the next scroll to the top asks again.
    case unreachable
    case failed(String)
}

/// One open chat as the phone's list holds it.
///
/// The chat holds the sequence of row keys as the runtime's deltas move it,
/// only ever at its two edges until a reload, and each cell holds its row:
/// read by key when the cell is first drawn, and handed over again by the
/// delta of every change to it, so only that cell redraws. A Reset's swap,
/// or the reload of a head that moved on while the reader was in history,
/// is the one time the whole sequence is handed over again.
///
/// The sequence is the session's window. While the reader follows, the
/// window keeps the newest rows up to its cap and drops the oldest as new
/// ones arrive; in history it stays put and grows only by pages, and what
/// arrives meanwhile is held by the session, which the frame reports and
/// New activity shows. The model tells the session each time the reader
/// leaves or returns to the newest row.
///
/// The list lays out only the rows near the screen, so the whole sequence
/// is its data and nothing here bounds what it draws. It watches
/// `sequence` for the keys changing and reads `ids` when it does; a cell
/// watches its own row and never the sequence, so an arrival redraws the
/// cell it lands in and nothing else.
@MainActor
@Observable
public final class ChatModel {
    /// A page of older rows, and the rows a chat opens with.
    public static let pageRows: UInt32 = 40
    /// The most a page asks for at a run that continues below the window.
    public static let largestPage: UInt32 = 1000
    /// How long an empty chat waits before saying it is loading, so a chat
    /// that fills at once never flashes the hint.
    public static let loadingHintDelay: Duration = .milliseconds(300)

    @ObservationIgnored public let source: ChatSource
    /// Every row key the chat holds, oldest first. Not observed, so a cell
    /// that reads past its own row (for its rail) does not redraw when the
    /// sequence changes; the list watches `sequence` instead.
    @ObservationIgnored public private(set) var ids: [String] = []
    /// Bumped whenever `ids` changes: at either edge, or swapped whole.
    public private(set) var sequence = 0
    /// Bumped whenever cells were read again, each stamped with it: a cell
    /// redraws itself on its row, but the list decides its height and
    /// measures the rows stamped since it last laid out.
    public private(set) var revision = 0
    /// Whether the chat holds no rows: what an empty chat's notices read,
    /// without watching the sequence.
    public private(set) var empty = true
    /// Each held key's place, counted so that a page landing before the
    /// oldest row renumbers nothing: a key's index in `ids` is its place
    /// less `firstPlace`.
    @ObservationIgnored private var places: [String: Int] = [:]
    @ObservationIgnored private var firstPlace = 0
    /// How many rows the chat holds. Only a cell with nothing drawn below it
    /// reads this, so a row arriving under the newest drawn row redraws
    /// just that cell's rail.
    public private(set) var heldCount = 0
    @ObservationIgnored private var cells: [String: RowCell] = [:]
    public private(set) var frame: ChatFrame?
    public private(set) var ask: AskCard?
    /// What the person picked and typed on the head ask's questions and has
    /// not sent, kept like the draft so leaving the chat loses none of it,
    /// until that ask closes.
    @ObservationIgnored private var questionKept: (ask: String, draft: QuestionDraft)?
    /// What the person entered in the head ask's tool server form and has
    /// not submitted, one value per field, kept the same way.
    @ObservationIgnored private var formKept: (ask: String, values: [FormValue])?
    /// The tasks, background jobs, failed tool servers and usage near a
    /// limit around the chat.
    public private(set) var overview: Overview?
    /// What the agent offers to change, the current values marked.
    public private(set) var settings: SettingsView?
    /// Rows the reader opened: a subagent's steps, or a row's detail.
    public private(set) var expanded: Set<String> = []
    public private(set) var paging: ChatPaging = .idle
    /// The chat is still empty a moment after opening.
    public private(set) var loadingHint = false
    /// The reader is at the newest row, so new rows are followed.
    public private(set) var following = true
    /// Rows arrived while the reader is in history: the session holds them
    /// apart until the reader returns.
    public var newActivity: Bool { !following && frame?.arrivalsHeld == true }
    /// Bumped whenever the list should go to its newest row: a swap, a send,
    /// or the reader asking.
    public private(set) var toNewest = 0
    public var draft = "" {
        didSet { if draft != oldValue { keepDraft() } }
    }
    public private(set) var attachments: [DraftAttachment] = [] {
        didSet { if attachments != oldValue { keepDraft() } }
    }
    /// Told the draft as it stands each time it changes, so it can be kept
    /// past the session and the app: the phone's drafts live with its own
    /// daemon.
    /// It carries the words of prompts held for a host that cannot be
    /// reached too, ahead of what is typed: a held prompt goes back to the
    /// draft if the phone closes before the host is back.
    @ObservationIgnored public var keeping: (@MainActor (Draft) -> Void)?
    /// The prompts the chat holds for a host that cannot be reached, oldest
    /// first, each as the draft it came from.
    @ObservationIgnored private var heldDrafts: [(id: [UInt8], draft: Draft)] = []
    /// Whether the chat holds a prompt for a host that cannot be reached:
    /// its words are in the kept draft until it goes.
    public var holdsPrompts: Bool { !heldDrafts.isEmpty }
    /// Attachments still being stored.
    public private(set) var uploading = 0
    /// A send or resume on its way.
    public private(set) var sending = false
    /// What went wrong with the last thing the person did, until the next.
    public private(set) var notice: String?
    /// What the header's totals, the overview's changed files and the
    /// review count against.
    public private(set) var comparison: Comparison = .uncommitted
    /// Whether the overview is on screen: its changed files are fetched only
    /// then, and again when the comparison or its totals move.
    @ObservationIgnored private var overviewShown = false
    @ObservationIgnored private var listed: Listed?
    /// The review being written on this agent's changes, kept while the
    /// chat is open so leaving the page loses no comment.
    public private(set) var reviewing: ReviewModel?
    /// Dictation into the draft; the app's speech recogniser drives it.
    public var dictation = DictationState()
    /// The field's text when a paste last left it, and when.
    /// The last paste taken out of the draft, where the field still shows it.
    @ObservationIgnored private var pasted: (at: Int, text: [Character])?

    /// - Parameter loadingHintAfter: how long an empty chat waits before
    ///   saying it is loading; zero says so from the first frame.
    public init(source: ChatSource, loadingHintAfter: Duration = ChatModel.loadingHintDelay) {
        self.source = source
        let opening = source.opening()
        if case .reload(let keys)? = opening.keys { hold(keys: keys) }
        show(opening)
        if ids.isEmpty { waitForRows(loadingHintAfter) }
    }

    // MARK: - Reading

    /// What moved in the chat since the last delta: the sequence at its
    /// edges or whole, the rows that changed, and the frame, the card and
    /// the strip where they moved.
    public func apply(_ delta: ChatDelta) {
        if delta.keys != nil || !delta.rows.isEmpty {
            Signposts.emit(.transcriptCommit)
        }
        switch delta.keys {
        case .reload(let keys)?: swap(keys)
        case .edges(let above, let below, let dropped)?: move(above: above, below: below, dropped: Int(dropped))
        case nil: break
        }
        fill(delta.rows)
        show(delta)
    }

    /// The cell for a key, its row read now if it never was.
    public func cell(for id: String) -> RowCell {
        if let cell = cells[id] { return cell }
        let cell = RowCell(
            id: id, row: source.rows(for: [id]).first, revision: revision)
        cells[id] = cell
        return cell
    }

    /// The revision a drawn row was last read at; a row not drawn has none
    /// to measure again.
    public func revision(of id: String) -> Int {
        cells[id]?.revision ?? 0
    }

    /// Whether a row draws: collapsed rows do not, except a subagent's steps
    /// under a subagent the reader opened. Inside a run only its steps
    /// draw, as in the terminal: the thinking and retries between them are
    /// part of the run's work, not things to read.
    public func shows(_ row: Row) -> Bool {
        if row.run != nil, row.kind.passesThroughRuns { return false }
        return !row.collapsed || row.parent.map(expanded.contains) == true
    }

    /// The nearest row below this one that draws: what decides whether its
    /// rail runs on. Collapsed rows and rows with nothing to draw are
    /// passed over.
    public func row(below id: String) -> Row? {
        guard let place = places[id] else { return nil }
        let start = place - firstPlace + 1
        if start < ids.count {
            for id in ids[start...] {
                guard let row = cell(for: id).row else { continue }
                if case .hidden = row.kind { continue }
                if shows(row) { return row }
            }
        }
        // Nothing below yet: the next row to arrive may be the one.
        _ = heldCount
        return nil
    }

    /// Opens or closes a row's own detail, or a subagent's steps. Every
    /// drawn row is measured again, since what draws moved without the rows
    /// themselves changing.
    public func toggle(_ id: String) {
        if expanded.contains(id) {
            expanded.remove(id)
        } else {
            expanded.insert(id)
        }
        guard !cells.isEmpty else { return }
        revision += 1
        for cell in cells.values { cell.revision = revision }
    }

    public func isExpanded(_ id: String) -> Bool {
        expanded.contains(id)
    }

    /// Opens or closes a run, by the shared fold; its rows say which, as
    /// the next delta hands them over.
    public func toggleRun(_ run: Run) {
        source.toggleRun(run.last)
    }

    /// What a run's steps did: carried on its newest step's row only.
    public func counts(of run: Run) -> RunCounts? {
        cell(for: run.last).row?.run?.counts
    }

    /// An attachment's bytes where this phone holds them; asking starts the
    /// fetch, and the row changes when they land.
    public func bytes(of blob: BlobRef) -> Data? {
        source.blob(blob.hash)
    }

    /// The window moved at its edges: keys new above the newest, a page
    /// below the oldest, and the oldest the window dropped as it trimmed.
    private func move(above: [String], below: [String], dropped: Int) {
        var moved = dropOlder(than: dropped)
        if !below.isEmpty {
            ids.insert(contentsOf: below, at: 0)
            firstPlace -= below.count
            for (offset, key) in below.enumerated() { places[key] = firstPlace + offset }
            moved = true
        }
        if !above.isEmpty {
            for (offset, key) in above.enumerated() { places[key] = firstPlace + ids.count + offset }
            ids.append(contentsOf: above)
            loadingHint = false
            moved = true
        }
        if moved { sequenceChanged() }
    }

    /// The window dropped its oldest rows: the first `count` keys go, and
    /// their cells with them.
    private func dropOlder(than count: Int) -> Bool {
        guard count > 0 else { return false }
        for key in ids[..<count] {
            places[key] = nil
            cells[key] = nil
        }
        ids.removeFirst(count)
        firstPlace += count
        return true
    }

    /// A Reset's transcript was swapped in: every key may be new. Cells the
    /// new sequence keeps are read again in place, and the list goes to the
    /// newest row.
    private func swap(_ keys: [String]) {
        hold(keys: keys)
        cells = cells.filter { places[$0.key] != nil }
        refresh(Array(cells.keys))
        loadingHint = false
        setFollowing(true)
        toNewest += 1
    }

    /// Where the reader is, told to the session whenever it changes.
    private func setFollowing(_ value: Bool) {
        guard following != value else { return }
        following = value
        source.follow(value)
    }

    private func hold(keys: [String]) {
        ids = keys
        firstPlace = 0
        places = Dictionary(uniqueKeysWithValues: keys.enumerated().map { ($1, $0) })
        sequenceChanged()
    }

    /// `ids` changed: what watches the sequence, and what watches its
    /// count or its emptiness, is told only if that moved.
    private func sequenceChanged() {
        sequence += 1
        if heldCount != ids.count { heldCount = ids.count }
        if empty != ids.isEmpty { empty = ids.isEmpty }
    }

    /// Reads every held cell again in place and stamps it, so the list
    /// measures it again: after a swap, what each kept key draws may have
    /// moved.
    private func refresh(_ keys: [String]) {
        guard !keys.isEmpty else { return }
        let rows = source.rows(for: keys)
        revision += 1
        for row in rows {
            guard let cell = cells[row.id] else { continue }
            cell.row = row
            cell.revision = revision
        }
    }

    /// Rows a delta handed over, each into its cell if it was drawn; a
    /// cell drawn later reads its row then. Making cells here for rows the
    /// list has not drawn costs it more layout than the reads it saves. A
    /// row that reads as it did passes: an item arrives as it streams and
    /// again whole, usually drawing the same row, and measuring it again
    /// would lay the whole list out again for nothing.
    private func fill(_ rows: [Row]) {
        let next = revision + 1
        var moved = false
        for row in rows {
            guard places[row.id] != nil else { continue }
            guard let cell = cells[row.id] else { continue }
            guard cell.row != row else { continue }
            cell.row = row
            cell.revision = next
            moved = true
        }
        if moved { revision = next }
    }

    /// The views a delta says moved. The runtime hands over only what
    /// moved, since an assignment redraws everything that reads it.
    private func show(_ delta: ChatDelta) {
        let before = frame
        if var frame = delta.frame {
            if !frame.refused.isEmpty {
                frame.refused.forEach(takeBack)
                frame.refused = []
            }
            self.frame = frame
            keepHeld(frame)
        }
        if let moved = delta.ask {
            ask = moved.card
            if let kept = questionKept?.ask, kept != ask?.key { questionKept = nil }
            if let kept = formKept?.ask, kept != ask?.key { formKept = nil }
        }
        if let overview = delta.overview { self.overview = overview }
        if let settings = delta.settings { self.settings = settings }
        if frame?.git != before?.git { listChangedFiles() }
    }

    // MARK: - Changes

    /// The changed files the overview last asked for, and the totals they
    /// were asked at.
    private struct Listed: Equatable {
        var comparison: Comparison
        var totals: ChangeTotals?
    }

    /// The agent's totals for the chosen comparison as of its last turn end;
    /// nil with nothing to count.
    public var changes: ChangeTotals? {
        totals(comparison).flatMap { $0.files > 0 || $0.added + $0.removed > 0 ? $0 : nil }
    }

    /// The branch everything on the agent's branch is counted against.
    public var base: String? { frame?.git?.baseBranch }

    /// The comparisons there are: everything on the branch only once its
    /// base is known.
    public var comparisons: [Comparison] {
        base == nil ? [.uncommitted] : [.uncommitted, .onBranch]
    }

    private func totals(_ comparison: Comparison) -> ChangeTotals? {
        switch comparison {
        case .uncommitted: frame?.git?.uncommitted
        case .onBranch: frame?.git?.onBranch
        }
    }

    public func compare(_ comparison: Comparison) {
        guard self.comparison != comparison else { return }
        self.comparison = comparison
        listChangedFiles()
    }

    /// The overview came on screen or left it.
    public func showOverview(_ shown: Bool) {
        overviewShown = shown
        if shown {
            listed = nil
            listChangedFiles()
        }
    }

    /// The newest listing of the changed files, so a test can wait for its
    /// answer to land rather than guess how long the host takes.
    @ObservationIgnored private(set) var listing: Task<Void, Never>?

    /// Asks the agent's host for the changed files while the overview shows
    /// them, once per comparison and totals.
    private func listChangedFiles() {
        guard overviewShown else { return }
        let asked = Listed(comparison: comparison, totals: totals(comparison))
        guard asked != listed else { return }
        listed = asked
        listing = Task { [weak self] in
            guard let self, case .success(let overview) = await self.source.openOverview(asked.comparison),
                  self.comparison == asked.comparison, self.overview != overview
            else { return }
            self.overview = overview
        }
    }

    private func waitForRows(_ delay: Duration) {
        if delay == .zero {
            loadingHint = true
            return
        }
        Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard let self else { return }
            self.loadingHint = self.ids.isEmpty
        }
    }

    // MARK: - Scrolling

    /// Where the reader is: at the bottom of the list or above it.
    public func reading(atNewest: Bool) {
        setFollowing(atNewest)
    }

    /// Takes the reader to the newest row.
    public func jumpToNewest() {
        setFollowing(true)
        toNewest += 1
    }

    /// The oldest rows came near the top of the reader's view: ask for the
    /// page before them. One arrival at the top asks for one page at most.
    public func reachedTop() {
        askOlder()
    }

    private func askOlder() {
        guard frame?.hasOlder == true, paging != .fetching else { return }
        paging = .fetching
        let rows = pageSize
        Task {
            let outcome = await source.pageOlder(rows)
            switch outcome {
            case .arrived?: paging = .idle
            case .originUnreachable?: paging = .unreachable
            case .failed(let reason)?: paging = .failed(reason)
            case nil: paging = .failed("the chat is closed")
            }
        }
    }

    /// A page, or at a collapsed run that continues below the window, the
    /// run's own length up to a cap, so a long run arrives in a few pages.
    public var pageSize: UInt32 {
        let top = ids.lazy.compactMap { self.cell(for: $0).row }.first { self.shows($0) }
        guard let top, let run = top.run, run.openBelow, top.id == run.last,
            !run.open
        else { return Self.pageRows }
        return min(max(run.steps, Self.pageRows), Self.largestPage)
    }

    // MARK: - Writing

    /// Keeps the question card's progress on the head ask.
    public func keep(_ draft: QuestionDraft, onAsk key: String) {
        guard ask?.key == key else { return }
        questionKept = (key, draft)
    }

    /// The progress kept on this ask's question card, if any.
    public func questionDraft(onAsk key: String) -> QuestionDraft? {
        questionKept?.ask == key ? questionKept?.draft : nil
    }

    /// Keeps the tool server form's values, one per field, on the head ask.
    public func keep(form values: [FormValue], onAsk key: String) {
        guard ask?.key == key else { return }
        formKept = (key, values)
    }

    /// The values kept on this ask's form, if any.
    public func formDraft(onAsk key: String) -> [FormValue]? {
        formKept?.ask == key ? formKept?.values : nil
    }

    /// Sending waits for the rows to be current and the agent live; the draft
    /// never does.
    /// A chat that is not current still sends: the library holds the
    /// prompt, drawn pending with Cancel, and sends it once it is.
    public var canSend: Bool {
        guard let frame, !sending, uploading == 0, hasDraft else { return false }
        switch frame.composer.mode {
        case .send: return frame.caughtUp
        case .holds: return true
        default: return false
        }
    }

    public var canResume: Bool {
        guard let frame, !sending, uploading == 0 else { return false }
        return frame.composer.mode == .resume
    }

    public var hasDraft: Bool {
        !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty
    }

    /// The draft as it is sent. The chips sit apart from the field, and a
    /// sent message draws attachments after its words, so that is where
    /// they go.
    private var composed: Draft {
        Draft(words: draft, attachments: attachments)
    }

    /// Something a command the client runs asked the screen to open; the
    /// screen opens it and clears this.
    public var asks: ChatAsk?

    public func send() {
        guard canSend else { return }
        dictation.stop()
        if draft.trimmingCharacters(in: .whitespaces).hasPrefix("/") {
            runCommand(composed)
            return
        }
        sendAsTyped()
    }

    /// A draft that starts with "/" goes to the client library first: its
    /// own commands run here and never reach the agent, and one the agent
    /// does not run is refused with the draft kept.
    private func runCommand(_ typed: Draft) {
        sending = true
        notice = nil
        Task {
            let outcome = await source.command(typed)
            sending = false
            switch outcome {
            case .send?:
                if composed == typed { sendAsTyped() }
                return
            case .pick?:
                if composed == typed { clearDraft() }
                asks = .settings
            case .askName?:
                if composed == typed { clearDraft() }
                asks = .rename
            case .acted(let acted)?:
                switch acted {
                case .done, .held: if composed == typed { clearDraft() }
                case .rejected(let reason): notice = Self.notSent(reason)
                case .failed(let reason): notice = reason
                case .notConfirmed: notice = Self.notConfirmed
                }
            case .sent?:
                if composed == typed { clearDraft() }
                setFollowing(true)
                toNewest += 1
            case .refused(let command, let offered)?:
                notice = Self.refused(command: command, offered: offered, kind: frame?.kind)
            case .unknown(let command, let value, let offered)?:
                notice = Self.unknown(command, value: value, offered: offered)
            case nil:
                notice = Self.closed
            }
        }
    }

    /// "Codex does not run /status here. It runs /compact, /clear and its skills."
    static func refused(command: String, offered: [String], kind: Kind?) -> String {
        let name = command.hasPrefix("/") ? command : "/" + command
        let agent = kind == .codex ? "Codex" : String(localized: "This agent")
        let runs = offered.map { $0.hasPrefix("/") || $0.hasPrefix("$") ? $0 : "/" + $0 }
        guard !runs.isEmpty else { return String(localized: "\(agent) does not run \(name) here.") }
        return String(localized: "\(agent) does not run \(name) here. It runs \(runs.joined(separator: ", ")).")
    }

    /// "No model named fast. It offers sonnet, opus."
    static func unknown(_ command: ClientCommand, value: String, offered: [String]) -> String {
        let list = offered.joined(separator: ", ")
        switch command {
        case .effort:
            return offered.isEmpty
                ? String(localized: "No effort level named \(value).")
                : String(localized: "No effort level named \(value). It offers \(list).")
        default:
            return offered.isEmpty
                ? String(localized: "No model named \(value).")
                : String(localized: "No model named \(value). It offers \(list).")
        }
    }

    private func sendAsTyped() {
        let sent = composed
        clearDraft()
        sending = true
        notice = nil
        setFollowing(true)
        toNewest += 1
        Task {
            let outcome = await source.send(sent)
            sending = false
            if case .failure(let failure) = outcome {
                restore(sent)
                notice = failure.description
            }
        }
    }

    /// The exited composer's one tap: the draft starts the agent again as its
    /// first prompt.
    public func resume() {
        guard canResume else { return }
        dictation.stop()
        let sent = composed
        sending = true
        notice = nil
        Task {
            let outcome = await source.resume(with: sent)
            sending = false
            switch outcome {
            case .done?:
                if composed == sent { clearDraft() }
                setFollowing(true)
                toNewest += 1
            // The library holds it until the host is back, drawn pending
            // with Cancel; the draft has gone with it.
            case .held?:
                if composed == sent { clearDraft() }
            case .rejected(let reason)?: notice = Self.notSent(reason)
            case .failed(let reason)?: notice = reason
            case .notConfirmed?: notice = Self.notConfirmed
            case nil: notice = Self.closed
            }
        }
    }

    // MARK: - Slash commands and pastes

    /// How many commands the slash rows offer at once.
    public static let slashRows = 5
    /// A paste this long becomes one attachment instead of draft text, as
    /// the terminal client does.
    public static let pasteLines = 8
    public static let pasteCharacters = 1_000

    /// The agent's commands that match a draft that is exactly a leading
    /// "/word": by prefix, or by prefix after a plugin's namespace. The
    /// list is the settings view's, which already leaves out what a
    /// headless agent cannot run; an agent that offers none lists nothing.
    public var slashMatches: [CommandView] {
        guard let word = Self.slashWord(draft), let commands = settings?.commands else { return [] }
        return Array(commands.filter { Self.command($0.name, matches: word) }.prefix(Self.slashRows))
    }

    static func slashWord(_ draft: String) -> String? {
        guard draft.hasPrefix("/") else { return nil }
        let word = draft.dropFirst()
        guard !word.contains(where: { $0.isWhitespace || $0 == "/" }) else { return nil }
        return word.lowercased()
    }

    static func command(_ name: String, matches word: String) -> Bool {
        let name = name.lowercased()
        if name.hasPrefix(word) { return true }
        guard let colon = name.firstIndex(of: ":") else { return false }
        return name[name.index(after: colon)...].hasPrefix(word)
    }

    /// Puts a picked command in the draft as the agent reads one: Claude's
    /// "/name", and a Codex skill by its "$name" mention.
    public func pick(_ command: CommandView) {
        let sigil = frame?.kind == .codex ? "$" : "/"
        draft = sigil + command.name + " "
    }

    /// The field's edits. A long run arriving in one edit is a paste: it
    /// leaves the draft and joins it as one inline text attachment. Only
    /// the person's typing comes through here, so words put back into the
    /// draft stay words.
    public func type(_ text: String) {
        var new = Array(
            text.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n"))
        // The field goes on showing a paste after the draft takes it out,
        // and sends it back with its echo of the paste and with the next
        // keystroke; it is taken out again until the field has redrawn.
        if let pasted, new.count >= pasted.at + pasted.text.count,
           Array(new[pasted.at..<(pasted.at + pasted.text.count)]) == pasted.text {
            new.removeSubrange(pasted.at..<(pasted.at + pasted.text.count))
            redrawField()
        } else {
            pasted = nil
        }
        let old = Array(draft)
        var head = 0
        while head < old.count, head < new.count, old[head] == new[head] { head += 1 }
        var tail = 0
        while tail < old.count - head, tail < new.count - head,
              old[old.count - 1 - tail] == new[new.count - 1 - tail] { tail += 1 }
        var inserted = Array(new[head..<(new.count - tail)])
        guard inserted.count >= Self.pasteCharacters || Self.lines(String(inserted)) >= Self.pasteLines
        else {
            draft = String(new)
            return
        }
        var before = Array(new[..<head])
        var after = Array(new[(new.count - tail)...])
        // Pasting beside a word, the text view adds a space to keep the
        // words apart; the space belongs to the sentence, not to the paste.
        if inserted.first == " ", before.last.map({ !$0.isWhitespace }) ?? false {
            inserted.removeFirst()
            before.append(" ")
        }
        if inserted.last == " ", after.first.map({ !$0.isWhitespace }) ?? false {
            inserted.removeLast()
            after.insert(" ", at: 0)
        }
        draft = String(before + after)
        attachments.append(.text(name: String(localized: "Pasted text"), text: String(inserted)))
        pasted = (at: before.count, text: inserted)
        redrawField()
    }

    /// Has the field read the draft again on the next turn, once the edit
    /// it is in the middle of is over.
    private func redrawField() {
        Task { @MainActor [weak self] in
            guard let self else { return }
            let draft = self.draft
            self.draft = draft
        }
    }

    /// Lines as the chat counts them: a last line break ends a line rather
    /// than starting one.
    nonisolated public static func lines(_ text: String) -> Int {
        var lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        if lines.last?.isEmpty == true { lines.removeLast() }
        return lines.count
    }

    /// The whole of what dictation has heard so far.
    public func heard(_ text: String) {
        dictation.receive(text, draft: &draft)
    }

    public func clearDraft() {
        draft = ""
        attachments = []
    }

    /// Puts back a draft kept from an earlier session, ahead of anything
    /// typed since.
    public func reopen(_ kept: Draft) {
        restore(kept)
    }

    private func keepDraft() {
        var kept = (words: draft, attachments: attachments)
        for held in heldDrafts.reversed() {
            kept = Self.restoring(held.draft, onto: kept)
        }
        keeping?(Draft(words: kept.words, attachments: kept.attachments))
    }

    /// Keeps the draft again when the prompts held for an unreachable host
    /// changed: one newly held joins it, one released and sent, or taken
    /// back into the composer, leaves it.
    private func keepHeld(_ frame: ChatFrame) {
        let held = frame.underway.filter { $0.underway == .held }.map(\.inputId)
        guard held != heldDrafts.map(\.id) else { return }
        heldDrafts = held.compactMap { id in source.draft(of: id).map { (id: id, draft: $0) } }
        keepDraft()
    }

    private func restore(_ sent: Draft) {
        (draft, attachments) = Self.restoring(sent, onto: (draft, attachments))
    }

    /// A draft's words and attachments put back ahead of what the composer
    /// holds.
    private static func restoring(
        _ sent: Draft, onto composer: (words: String, attachments: [DraftAttachment])
    ) -> (words: String, attachments: [DraftAttachment]) {
        let words = sent.words
        let joined =
            if composer.words.isEmpty { words } else if words.isEmpty { composer.words } else {
                words + "\n" + composer.words
            }
        return (joined, (sent.attachments ?? []) + composer.attachments)
    }

    /// The most an attachment may hold, in bytes: 64 MiB of its content,
    /// worded "64 MB". The hosts refuse more; the phone says so before it
    /// uploads anything. A copy of `wire::ATTACHMENT_SIZE_LIMIT`, because the
    /// bridge carries values and no constants: the two change together.
    public static let attachmentLimit = 64 * 1024 * 1024

    /// Whether an attachment this large may be stored. One over the limit is
    /// refused here, in words naming the file and the limit, so nothing is
    /// read or uploaded for it.
    @discardableResult
    public func admits(name: String, size: Int) -> Bool {
        guard size > Self.attachmentLimit else { return true }
        notice = String(localized: "\(name) is not attached because it is over the 64 MB limit for attachments.")
        return false
    }

    /// Stores a photo or file and adds it to the draft once it is stored. One
    /// that cannot be stored is named in the notice with the reason, so it
    /// never leaves the draft without a word.
    public func attach(_ data: Data, name: String, mime: String, image: Bool) {
        guard admits(name: name, size: data.count) else { return }
        uploading += 1
        notice = nil
        Task {
            let stored = await source.putBlob(data, name: name, mime: mime)
            uploading -= 1
            switch stored {
            case .success(let blob): attachments.append(image ? .image(blob) : .file(blob))
            case .failure(let failure):
                notice = String(localized: "\(name) is not attached: \(failure.description)")
            }
        }
    }

    /// The review page's model for the chosen comparison, as the agent's
    /// host freezes it now: the one already being written when it is on the
    /// same patch, else a new one. Nil when nothing changed.
    public func openReview() async -> Result<ReviewModel?, RuntimeFailure> {
        switch await source.review(comparison) {
        case .failure(let failure): return .failure(failure)
        case .success(let frozen):
            if let reviewing, reviewing.review.diff.patch?.hash == frozen.diff.patch?.hash {
                return .success(reviewing)
            }
            guard !ReviewModel.document(frozen, comments: []).files.isEmpty else { return .success(nil) }
            let review = ReviewModel(review: frozen)
            reviewing = review
            return .success(review)
        }
    }

    /// Puts a written review into the draft as one token, replacing the
    /// token of an earlier review of the same patch.
    public func attach(_ review: ReviewModel) {
        let patch = review.review.diff.patch?.hash
        attachments.removeAll { attachment in
            if case .review(let diff, _) = attachment { return diff.patch?.hash == patch }
            return false
        }
        attachments.append(review.attachment)
    }

    public func removeAttachment(at index: Int) {
        guard attachments.indices.contains(index) else { return }
        attachments.remove(at: index)
    }

    /// Takes a queued prompt back; its words and attachments return to the
    /// draft. What it held is read before it leaves the queue.
    public func withdraw(_ queued: QueuedRow) {
        let taken = source.draft(of: queued.inputId)
        act({ await $0.withdraw(queued.inputId) }) { [weak self] in
            if let taken { self?.restore(taken) }
        }
    }

    /// Steers a queued prompt into the running turn.
    public func sendNow(_ queued: QueuedRow) {
        act({ await $0.sendNow(queued.inputId) })
    }

    /// Sends an input that was not confirmed again, under a new id.
    public func resend(_ input: [UInt8]) {
        notice = nil
        Task {
            _ = await source.resend(input)
        }
    }

    public func discard(_ input: [UInt8]) {
        source.discard(input)
    }

    /// Takes back a prompt held while its host could not be reached: it
    /// never left, so its words and attachments return to the draft.
    public func cancelHeld(_ input: [UInt8]) {
        if let held = source.draft(of: input) { restore(held) }
        discard(input)
    }

    /// Takes back an answer held while its host could not be reached; the
    /// card opens again to be answered.
    public func cancelHeldAnswer() {
        guard let input = frame?.askInput else { return }
        discard(input)
    }

    /// A prompt the agent refused, back into the draft with the reason
    /// said: its words and attachments to change and send again.
    private func takeBack(_ refused: RefusedPrompt) {
        if let sent = source.draft(of: refused.inputId) { restore(sent) }
        notice = Self.notSent(refused.reason)
        source.discard(refused.inputId)
    }

    /// Why an input was not sent, as a notice.
    static func notSent(_ refused: Refusal) -> String {
        String(localized: "Not sent: \(refusal(refused))")
    }

    /// Why the agent or its host refused an input: the refusal's own words
    /// when it has them.
    public static func refusal(_ refused: Refusal) -> String {
        if let detail = refused.detail { return detail }
        return switch refused.reason {
        case .exited: String(localized: "it had exited")
        case .exiting: String(localized: "it was exiting")
        case .draining: String(localized: "its host is shutting down")
        case .hostUnreachable: String(localized: "its host can’t be reached")
        case .unsupported: String(localized: "this agent can’t take it")
        case .closedAsk: String(localized: "the ask had already closed")
        case .notQueued: String(localized: "it was no longer queued")
        case .conversationElsewhere: String(localized: "Claude’s own view switched to another conversation")
        case .tooLong: String(localized: "it is longer than the agent takes")
        case .unspecified: String(localized: "it was refused")
        }
    }

    /// This client's prompts on their way that are drawn at the feed's
    /// end, as they will stand once the agent has them.
    public var landingInFeed: [SentPrompt] {
        (frame?.underway ?? []).filter { $0.lands == .feed }
    }

    /// Those waiting in the queue, behind the agent's own queue.
    public var landingInQueue: [SentPrompt] {
        (frame?.underway ?? []).filter { $0.lands == .queue }
    }

    /// Stop: the interrupt. The turn ends, an open ask is dismissed, and the
    /// agent stays.
    public func interrupt() {
        act({ await $0.interrupt() })
    }

    /// Terminal Claude's own view resumed another conversation: puts this
    /// agent's back, so prompts reach it again.
    public func switchBack() {
        act({ await $0.switchBack() })
    }

    /// Sends a pick from the settings card. Nothing changes here until the
    /// agent reports the new value, which moves the current mark and the
    /// strip together.
    public func change(_ setting: SettingChange) {
        act({ await $0.change(setting) })
    }

    // MARK: - Answering

    public func answer(choice: Int, note: String? = nil) {
        guard let ask else { return }
        act({ await $0.answer(ask.key, choice: choice, note: note) })
    }

    /// Answers the head question card: one response per question.
    public func answer(responses: [QuestionResponse]) {
        guard let ask else { return }
        act({ await $0.answer(ask.key, responses: responses) })
    }

    /// Replies to the head question card in the person's own words.
    public func replyInstead(_ text: String, soFar: [QuestionResponse]) {
        guard let ask else { return }
        act({ await $0.replyInstead(ask.key, text: text, soFar: soFar) })
    }

    /// Submits the head form with one value per field, which the shared
    /// view checks and encodes.
    public func submit(form values: [FormValue]) {
        guard let ask else { return }
        act({ await $0.answerForm(ask.key, values: values) })
    }

    /// Sends the head ask's answer again after the connection dropped.
    public func resendAnswer() {
        guard let input = frame?.askInput else { return }
        resend(input)
    }

    public func discardAnswer() {
        guard let input = frame?.askInput else { return }
        discard(input)
    }

    private func act(
        _ body: @escaping @Sendable (ChatSource) async -> ActOutcome?,
        done: (@MainActor () -> Void)? = nil
    ) {
        notice = nil
        let source = self.source
        Task {
            let outcome = await body(source)
            switch outcome {
            case .done?: done?()
            // Held by the library until the host is back; what holds it says
            // so, with Cancel.
            case .held?: break
            case .rejected(let reason)?: notice = Self.notSent(reason)
            case .failed(let reason)?: notice = reason
            case .notConfirmed?:
                // An answer the card itself now says was not confirmed, with
                // Resend and Discard, is not said a second time above it.
                if case .notConfirmed? = ask?.state {} else { notice = Self.notConfirmed }
            case nil: notice = Self.closed
            }
        }
    }

    static let notConfirmed = String(
        localized: "The connection dropped before the agent answered.")
    static let closed = String(localized: "This chat is closed.")
}

/// A question card part way through: where it stands, what is picked,
/// typed, skipped and noted per question, and a reply being written.
public struct QuestionDraft: Equatable, Sendable {
    public var step: Int
    public var picks: [Set<UInt32>]
    public var others: [String?]
    public var highlighted: [UInt32?]
    public var skipped: [Bool]
    public var notes: [String]
    /// Whose note field is open.
    public var noting: [Bool]
    public var reviewing: Bool
    public var replying: Bool
    public var reply: String

    public init(
        step: Int = 0, picks: [Set<UInt32>], others: [String?], highlighted: [UInt32?],
        skipped: [Bool]? = nil, notes: [String]? = nil, noting: [Bool]? = nil,
        reviewing: Bool = false, replying: Bool = false, reply: String = ""
    ) {
        self.step = step
        self.picks = picks
        self.others = others
        self.highlighted = highlighted
        self.skipped = skipped ?? picks.map { _ in false }
        self.notes = notes ?? picks.map { _ in "" }
        self.noting = noting ?? picks.map { _ in false }
        self.reviewing = reviewing
        self.replying = replying
        self.reply = reply
    }

    /// Whether it was kept for a card of this many questions.
    public func fits(_ count: Int) -> Bool {
        [picks.count, others.count, highlighted.count, skipped.count, notes.count, noting.count]
            .allSatisfy { $0 == count }
    }
}

extension Draft {
    /// Words followed by attachments: the composer's shape, whose chips sit
    /// beside the field rather than among the words.
    public init(words: String, attachments: [DraftAttachment]) {
        self.init(parts: (words.isEmpty ? [] : [.text(words)]) + attachments.map(DraftPart.attachment))
    }

    /// The prompt's text as its host receives it: each attachment's place
    /// marked with U+FFFC.
    public var text: String {
        parts.map { part in
            if case .text(let words) = part { words } else { "\u{FFFC}" }
        }.joined()
    }

    /// The words alone, as they go back into the field.
    public var words: String {
        parts.compactMap { part in
            if case .text(let words) = part { words } else { nil }
        }.joined()
    }

    /// The attachments in order, nil when there are none.
    public var attachments: [DraftAttachment]? {
        let attached = parts.compactMap { part in
            if case .attachment(let attachment) = part { attachment } else { nil }
        }
        return attached.isEmpty ? nil : attached
    }
}

extension RowKind {
    /// Inside a run of tool steps, this neither counts nor ends the run:
    /// thinking, a retry notice, and what no client draws. Every other row
    /// a run holds is one of its steps.
    public var passesThroughRuns: Bool {
        switch self {
        case .thinking, .error, .hidden: true
        default: false
        }
    }

    /// A step the agent is still at: what a live run draws bright.
    public var inFlight: Bool {
        let phase: CallPhase? = switch self {
        case .command(_, let state, _, _, _, _, _, _): state
        case .explore(_, _, let state, _): state
        case .fileChange(_, let state): state
        case .toolCall(_, _, _, let state, _): state
        case .child(_, _, _, _, let state, _): state
        case .subagent(_, let running, _, _, _, _): running ? .running : nil
        default: nil
        }
        return phase == .pending || phase == .running
    }
}
