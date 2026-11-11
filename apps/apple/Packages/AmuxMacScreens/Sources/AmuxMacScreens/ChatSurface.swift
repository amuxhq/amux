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
    var canResume: Bool { get }
    var sending: Bool { get }
    var slashMatches: [CommandMatch] { get }
    func pick(_ match: CommandMatch)
    func send()
    func resume()
    /// Stops the turn under way.
    func interrupt()
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
    /// The review of the changes as the host froze them, being written.
    var review: ReviewState { get }
    /// Asks the host for the changes as they stand now. Keeping comments,
    /// a review of the same comparison with comments written or under way
    /// stays as it is, since reading again would replace it.
    func loadReview(keepingComments: Bool)
    /// Puts the review's comments into the draft, for the next message.
    func attachReview()
}

/// Where the review of an agent's changes stands.
public enum ReviewState {
    case idle
    case loading
    /// Nothing changed against the comparison.
    case nothing
    case failed(String)
    case ready(any ReviewSurface)
}

/// A line of the review: its file, hunk and line within the hunk.
public struct ReviewSpot: Hashable, Comparable, Sendable {
    public var file: Int
    public var hunk: Int
    public var line: Int

    public init(file: Int, hunk: Int, line: Int) {
        self.file = file
        self.hunk = hunk
        self.line = line
    }

    public static func < (a: ReviewSpot, b: ReviewSpot) -> Bool {
        (a.file, a.hunk, a.line) < (b.file, b.hunk, b.line)
    }
}

/// A review being written: the diff with the comments placed on it, and
/// the lines picked for the next comment.
@MainActor
public protocol ReviewSurface: AnyObject {
    var doc: ReviewDoc { get }
    /// How many comments are written.
    var count: Int { get }
    func folded(_ file: Int) -> Bool
    func toggle(file: Int)
    func isSelected(_ spot: ReviewSpot) -> Bool
    var selecting: Bool { get }
    func begin(at spot: ReviewSpot)
    func extend(to spot: ReviewSpot)
    func clearSelection()
    /// Comments on the selected lines; false when there was nothing to say.
    @discardableResult func comment(_ text: String) -> Bool
    var canAttach: Bool { get }
}
