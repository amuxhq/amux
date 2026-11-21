import AmuxValues
import Foundation

/// An open chat as the Mac's screens read and drive it.
///
/// The app hands in the shared chat model behind this; the storybook hands
/// in a still one. Reads go straight through to the model, so a view that
/// reads one row redraws when that row moves and no other.
@MainActor
public protocol ChatSurface: AnyObject {
    // MARK: The rows

    /// Every row key the chat holds, oldest first.
    var ids: [String] { get }
    func row(_ id: String) -> Row?
    /// Whether a row draws at all: inside a folded run, or a subagent's
    /// step under a closed subagent, it does not.
    func shows(_ row: Row) -> Bool
    /// The nearest row below this one that draws.
    func row(below id: String) -> Row?
    func isExpanded(_ id: String) -> Bool
    func toggle(_ id: String)
    func toggleRun(_ run: Run)
    /// Opens or closes the group of calls that opened agents ("4 subagents")
    /// a row heads; opening one of those agents is the call's own row.
    func toggleGroup(_ group: SubagentGroup)
    /// The agent a call row opens, as the fleet has it; nil when the row
    /// opens none or the fleet does not hold it yet.
    func opened(by row: Row) -> OpenedAgent?
    /// What a group's agents come to, by the fleet.
    func state(of group: SubagentGroup) -> GroupState?
    /// How the agents the rows open that are still at work or waiting on
    /// the person stand, by the fleet: what the activity line says while
    /// one of them waits on the person.
    var liveSubagents: GroupState { get }
    /// Opens or closes a finished turn's work.
    func toggleTurn(_ turn: TurnPlace)
    func counts(of run: Run) -> RunCounts?
    /// An attachment's bytes, once they have landed.
    func bytes(of blob: BlobRef) -> Data?

    // MARK: Around the rows

    var frame: ChatFrame? { get }
    /// The ask at the head of the queue, while one is open.
    var ask: AskCard? { get }
    /// Whether the chat holds no rows yet.
    var empty: Bool { get }
    /// The chat is still empty a moment after opening.
    var loadingHint: Bool { get }
    /// Said above the oldest row while older history is coming or cannot.
    var olderNotice: String? { get }
    /// What went wrong with the last thing the person did.
    var notice: String? { get }
    /// The reader is at the newest row, so new rows are followed.
    var following: Bool { get }
    /// Bumped whenever the list should go to its newest row.
    var toNewest: Int { get }
    func reading(atNewest: Bool)
    func jumpToNewest()
    /// The reader reached the oldest row held.
    func reachedTop()
    /// This client's prompts on their way at the feed's end.
    var landing: [SentPrompt] { get }

    // MARK: The composer

    var draft: String { get set }
    /// What goes with the draft: pastes, files and a review's comments.
    var attachments: [DraftAttachment] { get }
    func removeAttachment(at index: Int)
    /// Adds files to the draft, each stored on the agent's host first.
    func attach(files: [URL])
    /// What the agent offers to change, the current values marked.
    var settings: SettingsView? { get }
    func change(_ setting: SettingChange)
    var canSend: Bool { get }
    /// Something to send: words, an attachment, or the review's drafts
    /// under the chip, which go with or without words.
    var hasMessage: Bool { get }
    var canResume: Bool { get }
    var sending: Bool { get }
    var slashMatches: [CommandMatch] { get }
    func pick(_ match: CommandMatch)
    func send()
    func resume()
    /// Stops the turn under way.
    func interrupt()
    /// Stops one job by the key its `JobStop` gives: a provider subagent's
    /// own, from its composer.
    func stopJob(_ key: String)
    func withdraw(_ queued: QueuedRow)
    func sendNow(_ queued: QueuedRow)

    // MARK: Asks

    func answer(choice: Int, note: String?)
    func answer(responses: [QuestionResponse])
    func replyInstead(_ text: String, soFar: [QuestionResponse])

    // MARK: Changes

    /// The header's change totals, against what the review compares.
    var changes: ChangeTotals? { get }
    /// The branch the changes are measured against.
    var base: String? { get }
    /// What the changes are counted against, and what else they could be.
    var comparison: Comparison { get }
    var comparisons: [Comparison] { get }
    func compare(_ comparison: Comparison)

    // MARK: The agent's review

    /// The agent's review page, read and written at the agent's host, while
    /// the tool window shows it; nil before it opens. On a host that offers
    /// no reviews the page says so.
    var reviewPage: (any ReviewPageSurface)? { get }
    /// Opens the review page for a look; it follows the agent's row until
    /// it is closed.
    func openReviewPage()
    /// The page left the screen. The review and its drafts stay.
    func closeReviewPage()
    /// The composer's chip while the agent's review holds drafts, which
    /// the next send carries.
    var reviewChip: ReviewChip? { get }
    /// The chip's ✕, once the person confirmed it: the drafts are deleted.
    func discardReviewDrafts()
    /// The round a review sent into the chat closed, read from the agent's
    /// host apart from the page open on the chat; nil once the host holds
    /// none of it, when the text the agent read stands in.
    func reviewRound(_ sent: SentReviewRef) async -> ReviewPage?
}

/// The agent's review page as the runtime composed it, and what the person
/// does on it. Rows, banners and the header are drawn as handed; a
/// selection is held by the rows' indices in `page`, and the runtime says
/// which lines it is.
@MainActor
public protocol ReviewPageSurface: AnyObject {
    var page: ReviewPage { get }
    /// The rows selected, first to last.
    var selected: ClosedRange<Int>? { get }
    /// Where a comment on the selection goes: nil with nothing selected,
    /// and for a selection with no line or across files.
    var anchor: ReviewAnchor? { get }
    /// Acts on their way to the agent's host.
    var waiting: Int { get }
    /// Why the last act came to nothing, when the page does not say it.
    var notice: String? { get }
    /// The comment being written on the selection, while its field is open.
    var writing: Bool { get set }
    var words: String { get set }

    /// Takes a new snapshot, moving any drafts onto it.
    func refresh()
    func moreContext(_ ask: ReviewContextAsk)
    func show(_ view: ReviewView)
    func toggleFile(_ path: String)
    func toggleComment(_ id: [UInt8])
    func select(_ row: Int)
    func extend(to row: Int)
    func clearSelection()
    /// Writes a draft on the selection; the selection ends once the host
    /// has it.
    func comment(_ text: String)
    func reword(_ id: [UInt8], as text: String)
    func delete(_ id: [UInt8])
    func discardDrafts()
    func startOver()
}
