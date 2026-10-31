import Foundation
import XCTest

@testable import AmuxCore

/// A chat source that records what the model asks it for.
/// What changed in a chat since the last delta, as the session gathers it:
/// the keys whose rows arrived or changed, whether every key may be new,
/// and whether anything beside the rows moved.
struct ChatChanges {
    var keys: [String]
    var reloaded: Bool
    var session: Bool
}

/// A chat as the runtime hands it over: opened whole, then each delta
/// computed, as the runtime computes it, against the keys and views it
/// last handed over.
private final class FakeChat: ChatSource, @unchecked Sendable {
    var ordered: [Row]
    var current: ChatFrame
    var card: AskCard?
    var pending = ChatChanges(keys: [], reloaded: false, session: false)
    /// Every key the model asked a row for, per call.
    var fetched: [[String]] = []
    var paged: [UInt32] = []
    /// The history before `ordered` that a page brings in, oldest first.
    var earlier: [Row] = []
    var sent: [Draft] = []
    var resumed: [Draft] = []
    var withdrawn: [[UInt8]] = []
    var discarded: [[UInt8]] = []
    /// The open sets asked to be re-held, in order.
    /// The runs the model opened or closed, by the member it named.
    var toggled: [String] = []
    /// The question responses sent, answered or with a reply, in order.
    var responded: [[QuestionResponse]] = []
    var replied: [String] = []
    /// What each queued or sent prompt held, by input id.
    var drafts: [[UInt8]: Draft] = [:]
    var interrupts = 0
    var facts: Overview?
    var offered: SettingsView?
    var changed: [SettingChange] = []
    /// The working-tree diff the machine answers with, and how often it was
    /// asked for.
    var working: FrozenReview?
    var reviews = 0
    /// The comparisons the overview asked for its changed files, in order,
    /// and the files it was told.
    var listings: [Comparison] = []
    var files: Changes?
    /// The window's cap while the reader follows, as the session keeps it.
    var cap = Int.max
    /// Where the model last said its reader is, in order.
    var follows: [Bool] = []
    var following = true
    /// Rows the session holds apart while the reader is in history.
    var held: [Row] = []

    /// What the model was last handed.
    private var sentKeys: [String] = []
    private var sentFrame: ChatFrame?
    private var sentCard: AskCard??
    private var sentFacts: Overview??
    private var sentOffered: SettingsView??

    init(rows: [Row], frame: ChatFrame) {
        ordered = rows
        current = frame
    }

    func opening() -> ChatDelta {
        pending = ChatChanges(keys: [], reloaded: false, session: false)
        sentKeys = ordered.map(\.id)
        (sentFrame, sentCard, sentFacts, sentOffered) = (Self.drawn(current), card, facts, offered)
        return ChatDelta(
            chat: 0, rows: [], ask: AskMoved(card: card), frame: current,
            keys: .reload(sentKeys), overview: facts, settings: offered)
    }

    /// What moved since the last delta, or nil when nothing did.
    func delta() -> ChatDelta? {
        defer { pending = ChatChanges(keys: [], reloaded: false, session: false) }
        let now = ordered.map(\.id)
        var keys: KeysMoved?
        var wanted = Set<String>()
        if !pending.reloaded, let (above, below, dropped) = Self.edges(from: sentKeys, to: now) {
            if !above.isEmpty || !below.isEmpty || dropped > 0 {
                keys = .edges(above: above, below: below, dropped: UInt32(dropped))
            }
            wanted = Set(pending.keys).intersection(now).subtracting(below).subtracting(above)
        } else if pending.reloaded || !sentKeys.isEmpty || !now.isEmpty {
            keys = .reload(now)
        }
        sentKeys = now
        let rows = ordered.filter { wanted.contains($0.id) }
        let frame = Self.drawn(current) == sentFrame ? nil : current
        sentFrame = Self.drawn(current)
        let ask = sentCard == .some(card) ? nil : AskMoved(card: card)
        sentCard = card
        let overview = sentFacts == .some(facts) ? nil : facts
        sentFacts = facts
        let settings = sentOffered == .some(offered) ? nil : offered
        sentOffered = offered
        guard keys != nil || !rows.isEmpty || frame != nil || ask != nil || overview != nil
            || settings != nil
        else { return nil }
        return ChatDelta(
            chat: 0, rows: rows, ask: ask, frame: frame, keys: keys, overview: overview,
            settings: settings)
    }

    /// Hands the model what moved since the last delta, if anything did.
    @MainActor
    func deliver(to model: ChatModel) {
        if let delta = delta() { model.apply(delta) }
    }

    /// The keys new above the newest handed over and below the oldest, and
    /// how many of the oldest went; nil when the window moved otherwise.
    private static func edges(
        from sent: [String], to now: [String]
    ) -> (above: [String], below: [String], dropped: Int)? {
        guard let newest = sent.last, let at = now.firstIndex(of: newest), let first = now.first
        else { return nil }
        let above = Array(now[(at + 1)...])
        var below: [String] = []
        var dropped = 0
        if let oldest = now.firstIndex(of: sent[0]) {
            below = Array(now[..<oldest])
        } else {
            guard let kept = sent.firstIndex(of: first) else { return nil }
            dropped = kept
        }
        guard sent.count - dropped + above.count + below.count == now.count else { return nil }
        return (above, below, dropped)
    }

    /// A frame moved only by the clock is not handed over, as the runtime
    /// hands it over.
    private static func drawn(_ frame: ChatFrame) -> ChatFrame {
        var frame = frame
        if let activity = frame.composer.activity, activity.sinceMs > 0 {
            frame.composer.activity?.elapsedMs = 0
        }
        return frame
    }

    /// Rows arriving at the head as the session takes them: while the
    /// reader follows they join the window, which drops its oldest rows past
    /// the cap; in history they are held apart and the frame says so.
    func arrive(_ rows: [Row]) {
        if following {
            ordered.append(contentsOf: rows)
            pending.keys.append(contentsOf: rows.map(\.id))
            trim()
        } else {
            held.append(contentsOf: rows)
            current.arrivalsHeld = true
            pending.session = true
        }
    }

    private func trim() {
        while ordered.count > cap {
            pending.keys.append(ordered.removeFirst().id)
        }
    }

    func follow(_ following: Bool) {
        follows.append(following)
        self.following = following
        guard following, !held.isEmpty else { return }
        current.arrivalsHeld = false
        pending.session = true
        if held.count > cap {
            // More arrived than the window holds: the head is reloaded.
            ordered = Array((ordered + held).suffix(cap))
            pending.reloaded = true
        } else {
            ordered.append(contentsOf: held)
            pending.keys.append(contentsOf: held.map(\.id))
            trim()
        }
        held = []
    }

    func rows(for keys: [String]) -> [Row] {
        fetched.append(keys)
        return ordered.filter { keys.contains($0.id) }
    }

    /// Sends are held, as for a host that cannot be reached: each is listed
    /// held with its draft, under ids from 1.
    var holdsSends = false

    func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> {
        sent.append(draft)
        let id = [UInt8(sent.count)]
        guard holdsSends else { return .success(SendOutcome(inputId: id, state: .sent)) }
        drafts[id] = draft
        current.underway.append(
            SentPrompt(inputId: id, text: [.text(draft.words)], lands: .feed, underway: .held))
        pending.session = true
        return .success(SendOutcome(inputId: id, state: .held))
    }

    func answer(_ ask: String, choice: Int, note: String?) async -> ActOutcome? { .done }
    func answer(_ ask: String, responses: [QuestionResponse]) async -> ActOutcome? {
        responded.append(responses)
        return .done
    }

    func replyInstead(_ ask: String, text: String, soFar: [QuestionResponse]) async -> ActOutcome? {
        replied.append(text)
        responded.append(soFar)
        return .done
    }
    /// Opens or closes a run as the runtime does: every row of it says so
    /// from then on, handed over by the next delta.
    func toggleRun(_ member: String) {
        toggled.append(member)
        guard let last = ordered.first(where: { $0.id == member })?.run?.last else { return }
        for index in ordered.indices where ordered[index].run?.last == last {
            ordered[index].run?.open.toggle()
            pending.keys.append(ordered[index].id)
        }
    }
    func answerForm(_ ask: String, values: [FormValue]) async -> ActOutcome? { .done }

    func withdraw(_ input: [UInt8]) async -> ActOutcome? {
        withdrawn.append(input)
        return .done
    }

    func sendNow(_ input: [UInt8]) async -> ActOutcome? { .done }
    func resend(_ input: [UInt8]) async -> SendOutcome? { nil }
    func discard(_ input: [UInt8]) {
        discarded.append(input)
        current.underway.removeAll { $0.inputId == input }
        current.refused.removeAll { $0.inputId == input }
    }
    func draft(of input: [UInt8]) -> Draft? { drafts[input] }

    func interrupt() async -> ActOutcome? {
        interrupts += 1
        return .done
    }
    func stopJob(_ key: String) async -> ActOutcome? { .done }

    func change(_ setting: SettingChange) async -> ActOutcome? {
        changed.append(setting)
        return .done
    }

    func resume(with draft: Draft) async -> ActOutcome? {
        resumed.append(draft)
        return .done
    }

    func pageOlder(_ rows: UInt32) async -> PageOutcome? {
        paged.append(rows)
        let page = Array(earlier.suffix(Int(rows)))
        earlier.removeLast(page.count)
        ordered.insert(contentsOf: page, at: 0)
        pending.keys.append(contentsOf: page.map(\.id))
        return .arrived(UInt32(page.count))
    }

    /// What storing an attachment fails with, when it does.
    var blobFailure: RuntimeFailure?

    /// The name of every attachment handed over to be stored, in order.
    var stored: [String] = []

    func putBlob(_ data: Data, name: String, mime: String) async -> Result<BlobRef, RuntimeFailure> {
        stored.append(name)
        if let blobFailure { return .failure(blobFailure) }
        return .success(BlobRef(hash: [9], name: name, mime: mime, size: UInt64(data.count)))
    }

    func blob(_ hash: [UInt8]) -> Data? { nil }

    func review(_ comparison: Comparison) async -> Result<FrozenReview, RuntimeFailure> {
        reviews += 1
        guard let working else { return .failure(RuntimeFailure("no machine")) }
        return .success(working)
    }

    func openOverview(_ comparison: Comparison) async -> Result<Overview, RuntimeFailure> {
        listings.append(comparison)
        var overview = facts ?? Overview(jobs: [], failedServers: [], changes: nil, tasks: nil, usageNearLimit: nil)
        overview.changes = files
        facts = overview
        return .success(overview)
    }
}

private func row(_ id: String, _ order: UInt64, _ text: String = "", run: Run? = nil) -> Row {
    Row(
        id: id, order: order, atMs: 0,
        kind: .prose(text: [.text(text.isEmpty ? id : text)], streaming: false, workingNote: false),
        collapsed: false, attention: false, decision: nil, parent: nil, run: run)
}

private func frame(
    mode: Composer = .send, caughtUp: Bool = true, hasOlder: Bool = false,
    phase: PhaseView = .idle, kind: Kind = .claudeSdk
) -> ChatFrame {
    ChatFrame(
        agent: AgentKey(host: [1], agent: [2]), name: "a", kind: kind, phase: phase,
        composer: ComposerView(mode: mode, switchedAway: false, activity: nil), controls: ControlsSummary(effort: nil, mode: nil, model: nil, permission: nil), connection: .live, caughtUp: caughtUp,
        hasOlder: hasOlder, arrivalsHeld: false, queue: [], underway: [], refused: [], askInput: nil, context: nil, ended: nil, git: nil,
        signIn: nil)
}

/// Prose rows "m<first>" through "m<last>", in order.
private func numbered(_ numbers: ClosedRange<Int>) -> [Row] {
    numbers.map { row("m\($0)", UInt64($0)) }
}

@MainActor
final class ChatModelTests: XCTestCase {
    func testOpeningReadsTheKeysAndReadsARowOnlyWhenItsCellIsDrawn() {
        let source = FakeChat(rows: [row("a", 1), row("b", 2), row("c", 3)], frame: frame())
        let model = ChatModel(source: source)
        XCTAssertEqual(model.ids, ["a", "b", "c"])
        XCTAssertTrue(source.fetched.isEmpty, "no row is read before its cell is drawn")
        XCTAssertEqual(model.cell(for: "b").row?.id, "b")
        XCTAssertEqual(model.cell(for: "b").row?.id, "b")
        XCTAssertEqual(source.fetched, [["b"]], "a drawn cell is not read again unchanged")
    }

    func testNewRowsExtendTheSequenceAtItsEdgesAndChangedRowsComeWithTheDelta() {
        let source = FakeChat(rows: [row("b", 2), row("c", 3)], frame: frame())
        let model = ChatModel(source: source)
        let b = model.cell(for: "b")
        _ = model.cell(for: "c")
        source.fetched = []

        // A streamed revision of c and a new row d above the newest.
        source.ordered = [row("b", 2), row("c", 3, "c, longer"), row("d", 4)]
        source.pending = ChatChanges(keys: ["c", "d"], reloaded: false, session: false)
        source.deliver(to: model)
        XCTAssertEqual(model.ids, ["b", "c", "d"])
        XCTAssertEqual(source.fetched, [], "the changed row came with the delta; nothing is read")
        XCTAssertEqual(model.cell(for: "c").row?.kind, row("c", 3, "c, longer").kind)
        XCTAssertTrue(model.cell(for: "b") === b, "an unchanged cell keeps its identity")

        // A page of older rows below the oldest.
        source.ordered.insert(row("a", 1), at: 0)
        source.pending = ChatChanges(keys: ["a"], reloaded: false, session: false)
        source.deliver(to: model)
        XCTAssertEqual(model.ids, ["a", "b", "c", "d"])
    }

    func testEveryCellReadSinceTheListLastLookedCarriesALaterRevision() {
        let source = FakeChat(rows: [row("a", 1), row("b", 2), row("c", 3)], frame: frame())
        let model = ChatModel(source: source)
        for id in ["a", "b", "c"] { _ = model.cell(for: id) }
        let laidOut = model.revision

        // Two wakes land before the list looks again, each reading a
        // different drawn cell.
        source.ordered = [row("a", 1, "a, longer"), row("b", 2), row("c", 3)]
        source.pending = ChatChanges(keys: ["a"], reloaded: false, session: false)
        source.deliver(to: model)
        source.ordered = [row("a", 1, "a, longer"), row("b", 2), row("c", 3, "c, longer")]
        source.pending = ChatChanges(keys: ["c"], reloaded: false, session: false)
        source.deliver(to: model)

        XCTAssertGreaterThan(model.revision, laidOut)
        XCTAssertGreaterThan(model.revision(of: "a"), laidOut, "the first wake's row is still to measure")
        XCTAssertGreaterThan(model.revision(of: "c"), laidOut)
        XCTAssertEqual(model.revision(of: "b"), laidOut, "an unread cell keeps its revision")
        XCTAssertEqual(model.revision(of: "zz"), 0, "a row never drawn has nothing to measure")
    }

    /// An item arrives as it streams and again whole, which usually draws
    /// the same row: the list has nothing to measure again.
    func testARowReadAgainUnchangedKeepsItsRevision() {
        let source = FakeChat(rows: [row("a", 1), row("b", 2)], frame: frame())
        let model = ChatModel(source: source)
        for id in ["a", "b"] { _ = model.cell(for: id) }
        let laidOut = model.revision

        source.pending = ChatChanges(keys: ["b"], reloaded: false, session: false)
        source.deliver(to: model)
        XCTAssertEqual(model.revision, laidOut)
        XCTAssertEqual(model.revision(of: "b"), laidOut)

        source.ordered = [row("a", 1), row("b", 2, "b, longer")]
        source.pending = ChatChanges(keys: ["b"], reloaded: false, session: false)
        source.deliver(to: model)
        XCTAssertGreaterThan(model.revision(of: "b"), laidOut, "a row that changed is measured again")
    }


    func testAResetSwapsTheWholeSequenceInAtOnceAndFollowsTheNewestRow() {
        let source = FakeChat(rows: [row("a", 1), row("b", 2)], frame: frame())
        let model = ChatModel(source: source)
        let b = model.cell(for: "b")
        let before = model.toNewest

        source.ordered = [row("b", 20, "b again"), row("x", 21), row("y", 22)]
        source.pending = ChatChanges(keys: [], reloaded: true, session: true)
        source.deliver(to: model)
        XCTAssertEqual(model.ids, ["b", "x", "y"])
        XCTAssertTrue(model.cell(for: "b") === b, "a kept key keeps its cell")
        XCTAssertEqual(model.cell(for: "b").row?.order, 20, "and the cell holds the new row")
        XCTAssertEqual(model.toNewest, before + 1)
        XCTAssertFalse(model.newActivity)
    }

    func testAResetInHistoryFollowsTheNewestRowAgain() {
        let source = FakeChat(rows: numbered(1...600), frame: frame())
        let model = ChatModel(source: source)
        model.reading(atNewest: false)
        let sequence = model.sequence
        source.ordered = numbered(1...700)
        source.pending = ChatChanges(keys: [], reloaded: true, session: true)
        source.deliver(to: model)
        XCTAssertTrue(model.following)
        XCTAssertGreaterThan(model.sequence, sequence)
        XCTAssertEqual(model.ids.last, "m700")
    }

    /// The list watches the sequence's edition, not the keys: an arrival
    /// bumps it once, a wake that moved nothing leaves it alone, and the
    /// frame, card and strip are assigned only when they differ.
    func testTheSequenceEditionMovesOncePerChange() {
        let source = FakeChat(rows: numbered(1...3), frame: frame())
        let model = ChatModel(source: source)
        let sequence = model.sequence
        source.arrive([row("m4", 4)])
        source.deliver(to: model)
        XCTAssertEqual(model.sequence, sequence + 1)
        XCTAssertEqual(model.ids.count, 4)
        XCTAssertFalse(model.empty)
        source.deliver(to: model)
        XCTAssertEqual(model.sequence, sequence + 1, "a wake with nothing new moves nothing")
    }

    func testRowsArrivingBelowAReaderWhoScrolledUpAreOfferedRatherThanFollowed() {
        let source = FakeChat(rows: [row("a", 1)], frame: frame())
        let model = ChatModel(source: source)
        model.reading(atNewest: false)
        let before = model.toNewest
        source.arrive([row("b", 2)])
        source.deliver(to: model)
        XCTAssertTrue(model.newActivity, "shown from the session's report")
        XCTAssertEqual(model.ids, ["a"], "the session holds the arrival apart")
        XCTAssertEqual(model.toNewest, before, "the list is not moved under the reader")
        model.jumpToNewest()
        XCTAssertFalse(model.newActivity)
        XCTAssertEqual(model.toNewest, before + 1)
        source.deliver(to: model)
        XCTAssertEqual(model.ids, ["a", "b"], "the return releases it")
    }

    /// The model tells its session once when the reader leaves the newest
    /// row and once when the reader returns, whichever way that happens.
    func testTheSessionIsToldWhenTheReaderLeavesAndReturns() async {
        let source = FakeChat(rows: numbered(1...50), frame: frame())
        let model = ChatModel(source: source)
        XCTAssertEqual(source.follows, [], "a chat opens following")
        model.reading(atNewest: false)
        model.reading(atNewest: false)
        XCTAssertEqual(source.follows, [false])
        model.reading(atNewest: true)
        XCTAssertEqual(source.follows, [false, true], "reaching the bottom returns")
        model.reading(atNewest: false)
        model.jumpToNewest()
        XCTAssertEqual(source.follows, [false, true, false, true], "and so does New activity")
        model.reading(atNewest: false)
        model.draft = "go on"
        let sending = model.send()
        XCTAssertEqual(source.follows.last, true, "and so does sending, before the send")
        XCTAssertTrue(source.sent.isEmpty)
        await sending?.value
        XCTAssertEqual(source.sent.count, 1)
    }

    /// A following chat under a flood holds the session's capped window: the
    /// keys the window drops leave the sequence, and nothing is paged.
    func testAFollowingChatUnderLiveRowsDropsTrimmedKeysAndAsksForNoPage() async {
        let source = FakeChat(rows: numbered(801...1_000), frame: frame(hasOlder: true))
        source.cap = 200
        let model = ChatModel(source: source)
        for n in 1_001...1_300 {
            source.arrive([row("m\(n)", UInt64(n))])
            source.deliver(to: model)
            model.reading(atNewest: true)
        }
        XCTAssertTrue(source.paged.isEmpty, "a following chat pages nothing")
        XCTAssertEqual(model.ids, (1_101...1_300).map { "m\($0)" })
        XCTAssertEqual(source.follows, [])
    }

    // MARK: - History and paging

    /// In history, arrivals are held and New activity shows; returning to
    /// the bottom resumes following and the session releases what it held,
    /// trimmed back to its cap, all without a fetch.
    func testArrivalsWhileReadingAreHeldAndReturningDownLandsOnTheHead() {
        let source = FakeChat(rows: numbered(1...600), frame: frame(hasOlder: true))
        source.cap = 200
        let model = ChatModel(source: source)
        model.reading(atNewest: false)
        let sequence = model.sequence
        let toNewest = model.toNewest

        for n in 601...650 {
            source.arrive([row("m\(n)", UInt64(n))])
            source.deliver(to: model)
        }
        XCTAssertEqual(model.sequence, sequence, "arrivals leave the sequence alone")
        XCTAssertTrue(model.newActivity)
        XCTAssertEqual(model.toNewest, toNewest, "the list is not moved under the reader")
        XCTAssertEqual(model.ids.count, 600, "the session holds them apart")

        model.reading(atNewest: true)
        XCTAssertTrue(model.following)
        XCTAssertFalse(model.newActivity)
        source.deliver(to: model)
        XCTAssertEqual(model.ids, (451...650).map { "m\($0)" })
        XCTAssertEqual(model.toNewest, toNewest, "the list keeps the bottom itself while following")
        XCTAssertTrue(source.paged.isEmpty, "coming back down asks for nothing")
    }

    /// New activity takes the reader to the live head, however far up they
    /// were.
    func testNewActivityTakesTheReaderToTheNewestRun() {
        let source = FakeChat(rows: numbered(1...600), frame: frame())
        let model = ChatModel(source: source)
        source.cap = 200
        model.reading(atNewest: false)
        source.arrive(numbered(601...1_000))
        source.deliver(to: model)
        XCTAssertTrue(model.newActivity)
        let toNewest = model.toNewest
        model.jumpToNewest()
        XCTAssertTrue(model.following)
        XCTAssertFalse(model.newActivity)
        XCTAssertEqual(model.toNewest, toNewest + 1)
        // More arrived than the window holds: the head is reloaded.
        source.deliver(to: model)
        XCTAssertEqual(model.ids, (801...1_000).map { "m\($0)" })
    }

    /// A reader resting at the top pages ahead once per arrival there, and a
    /// page landing while the reader waits at the oldest row never cascades
    /// into the next page.
    func testOnePageIsAskedPerArrivalAtTheTop() async {
        let source = FakeChat(rows: numbered(1_001...1_020), frame: frame(hasOlder: true))
        source.earlier = numbered(1...1_000)
        let model = ChatModel(source: source)
        model.reading(atNewest: false)

        let page = model.reachedTop()
        XCTAssertEqual(model.paging, .fetching)
        XCTAssertNil(model.reachedTop(), "a page on its way is not asked for twice")
        await page?.value
        XCTAssertEqual(source.paged.count, 1)
        XCTAssertEqual(model.paging, .idle)
        source.deliver(to: model)
        XCTAssertEqual(model.ids.first, "m961", "the page the reader waited for lands above")
        // An ask is marked fetching as it is made, so idle here is none made.
        XCTAssertEqual(model.paging, .idle, "a page landing asks for no other")

        await model.reachedTop()?.value
        XCTAssertEqual(source.paged.count, 2)
        source.deliver(to: model)
        XCTAssertEqual(model.ids.first, "m921")
        XCTAssertEqual(model.ids.count, 100)
    }

    /// The newest row's rail reads the row below it as soon as one is held.
    func testTheRowBelowTheNewestIsReadWhenItArrives() {
        let source = FakeChat(rows: numbered(1...300), frame: frame())
        let model = ChatModel(source: source)
        XCTAssertNil(model.row(below: "m300"))
        source.ordered.append(row("m301", 301))
        source.pending = ChatChanges(keys: ["m301"], reloaded: false, session: false)
        source.deliver(to: model)
        XCTAssertEqual(model.row(below: "m300")?.id, "m301")
    }

    func testAPageIsTheUsualSizeExceptAtACollapsedRunThatContinuesBelow() async {
        let run = Run(
            first: "q", last: "r", steps: 300, live: false, openBelow: true,
            unresolvedFailure: false, open: false, counts: RunCounts(commands: 0, edits: 0, reads: 300, searches: 0, subagents: 0, other: 0),
            recent: nil)
        let source = FakeChat(rows: [row("r", 5, run: run), row("s", 6)], frame: frame(hasOlder: true))
        let model = ChatModel(source: source)
        _ = model.cell(for: "r")
        await model.reachedTop()?.value
        XCTAssertEqual(source.paged, [300])

        let huge = Run(
            first: "q", last: "r", steps: 5_000, live: false, openBelow: true,
            unresolvedFailure: false, open: false, counts: RunCounts(commands: 0, edits: 0, reads: 5_000, searches: 0, subagents: 0, other: 0),
            recent: nil)
        source.ordered[0] = row("r", 5, run: huge)
        source.pending = ChatChanges(keys: ["r"], reloaded: false, session: false)
        source.deliver(to: model)
        await model.reachedTop()?.value
        XCTAssertEqual(source.paged.last, ChatModel.largestPage)

        let plain = FakeChat(rows: [row("a", 1)], frame: frame(hasOlder: true))
        let other = ChatModel(source: plain)
        _ = other.cell(for: "a")
        await other.reachedTop()?.value
        XCTAssertEqual(plain.paged, [ChatModel.pageRows])
    }

    func testTheRowDrawnBelowPassesOverCollapsedAndEmptyRows() {
        var folded = row("b", 2)
        folded.collapsed = true
        var empty = row("c", 3)
        empty.kind = .hidden
        let source = FakeChat(
            rows: [row("a", 1), folded, empty, row("d", 4)], frame: frame())
        let model = ChatModel(source: source)
        XCTAssertEqual(model.row(below: "a")?.id, "d")
        XCTAssertNil(model.row(below: "d"))
    }

    func testNothingIsAskedForWhenNoOlderHistoryExists() async {
        let source = FakeChat(rows: [row("a", 1)], frame: frame(hasOlder: false))
        let model = ChatModel(source: source)
        await model.reachedTop()?.value
        XCTAssertTrue(source.paged.isEmpty)
    }

    func testADraftSentWhileCatchingUpIsHandedOnForTheLibraryToHold() async {
        // Decided: a chat that is not current still takes a send. The client
        // library holds it, shown pending with Cancel, and sends it under its
        // id once the chat is current; the phone does not keep Send disabled.
        let source = FakeChat(rows: [], frame: frame(mode: .holds(.catchingUp), caughtUp: false))
        let model = ChatModel(source: source)
        model.draft = "Run the tests."
        XCTAssertTrue(model.canSend)
        await model.send()?.value
        XCTAssertEqual(source.sent.map(\.text), ["Run the tests."])
        XCTAssertEqual(model.draft, "")
    }

    func testAnExitedAgentResumesWithTheDraft() async {
        let source = FakeChat(rows: [], frame: frame(mode: .resume, phase: .exited(cause: .ended)))
        let model = ChatModel(source: source)
        model.draft = "Carry on."
        XCTAssertFalse(model.canSend)
        XCTAssertTrue(model.canResume)
        await model.resume()?.value
        XCTAssertEqual(source.resumed.map(\.text), ["Carry on."])
        XCTAssertEqual(model.draft, "")
    }

    func testAWithdrawnPromptComesBackToTheDraft() async {
        let source = FakeChat(rows: [], frame: frame(phase: .working))
        source.drafts[[7]] = Draft(words: "Then run the Windows check.", attachments: [])
        let model = ChatModel(source: source)
        let queued = QueuedRow(
            inputId: [7], text: [.text("Then run the Windows check.")], steered: false,
            canWithdraw: true, canSendNow: true, fromAgent: nil)
        await model.withdraw(queued)?.value
        XCTAssertEqual(source.withdrawn, [[7]])
        XCTAssertEqual(model.draft, "Then run the Windows check.")
    }

    /// Withdrawn, a prompt gives back every attachment whole: a pasted text
    /// with all its words, a review with its comments and patch, not the
    /// line count and label its queued row shows.
    func testAWithdrawnPromptGivesBackItsPastedTextAndReview() async {
        let source = FakeChat(rows: [], frame: frame(phase: .working))
        let pasted = DraftAttachment.text(name: "Pasted text", text: "line one\nline two\nline three")
        let patch = BlobRef(hash: [4, 2], name: "patch", mime: "text/x-diff", size: 120)
        let review = DraftAttachment.review(
            diff: Diff(head: "abc123", files: [], base: nil, mergeBase: nil, patch: patch),
            comments: [ReviewComment(path: "src/lib.rs", line: 12, oldLine: 0, text: "Name this.")])
        source.drafts[[7]] = Draft(words: "Look at these.", attachments: [pasted, review])
        let model = ChatModel(source: source)
        model.draft = "half written"
        let queued = QueuedRow(
            inputId: [7],
            text: [
                .text("Look at these."),
                .attachment(.text(name: "Pasted text", lines: 3, text: "line one\nline two\nline three")),
            ],
            steered: false, canWithdraw: true, canSendNow: true, fromAgent: nil)
        await model.withdraw(queued)?.value
        XCTAssertEqual(source.withdrawn, [[7]])
        XCTAssertEqual(model.draft, "Look at these.\nhalf written")
        XCTAssertEqual(model.attachments, [pasted, review])
    }

    /// A refused prompt comes back into the composer on its own, words and
    /// photo, with why it was not sent, and is forgotten.
    func testARefusedPromptReturnsToTheComposerWithTheReason() async {
        var refusing = frame()
        refusing.refused = [RefusedPrompt(inputId: [8], reason: Refusal(reason: .hostUnreachable, detail: nil))]
        let source = FakeChat(rows: [], frame: refusing)
        let photo = BlobRef(hash: [5], name: "screen.png", mime: "image/png", size: 2048)
        source.drafts[[8]] = Draft(words: "What is this?", attachments: [.image(photo)])
        let model = ChatModel(source: source)
        XCTAssertEqual(model.draft, "What is this?")
        XCTAssertEqual(model.attachments, [.image(photo)])
        XCTAssertEqual(model.notice, "Not sent: its host can’t be reached")
        XCTAssertEqual(source.discarded, [[8]])
        XCTAssertEqual(model.frame?.refused, [], "taken back once")
    }

    /// Prompts on their way split by where they land: the feed's end or
    /// the queue.
    func testPromptsOnTheirWayAreDrawnWhereTheyLand() async {
        var sending = frame()
        sending.underway = [
            SentPrompt(inputId: [1], text: [.text("now")], lands: .feed, underway: .sending(waiting: false)),
            SentPrompt(inputId: [2], text: [.text("later")], lands: .queue, underway: .mayNotHaveArrived),
        ]
        let model = ChatModel(source: FakeChat(rows: [], frame: sending))
        XCTAssertEqual(model.landingInFeed.map(\.inputId), [[1]])
        XCTAssertEqual(model.landingInQueue.map(\.inputId), [[2]])
    }

    func testAPickIsSentAndTheCurrentValueMovesOnlyWhenTheAgentReportsIt() async {
        func offer(current: String) -> SettingsView {
            let models = ["opus", "sonnet"].map { value in
                ModelChoice(
                    value: value, displayName: value, description: "", efforts: [],
                    current: value == current, unlisted: false, defaultEffort: nil)
            }
            return SettingsView(
                models: models, efforts: [], permissions: [], modes: [],
                changeable: Changeability(model: .pick, effort: .notOffered, permission: .notOffered, mode: .notOffered),
                commands: [])
        }
        let source = FakeChat(rows: [], frame: frame())
        source.offered = offer(current: "opus")
        source.current.controls.model = "opus"
        let model = ChatModel(source: source)
        await model.change(.model("sonnet"))?.value
        XCTAssertEqual(source.changed, [.model("sonnet")])
        XCTAssertEqual(model.settings?.models.first { $0.current }?.value, "opus", "not before the agent says so")
        XCTAssertEqual(model.frame?.controls.model, "opus")
        source.offered = offer(current: "sonnet")
        source.current.controls.model = "sonnet"
        source.pending.session = true
        source.deliver(to: model)
        XCTAssertEqual(model.settings?.models.first { $0.current }?.value, "sonnet")
        XCTAssertEqual(model.frame?.controls.model, "sonnet")
    }

    private func offering(_ names: [String], skills: [String] = []) -> SettingsView {
        SettingsView(
            models: [], efforts: [], permissions: [], modes: [], changeable: Changeability(
                model: .notOffered, effort: .notOffered, permission: .notOffered, mode: .notOffered),
            commands: names.map {
                CommandView(name: $0, description: "", argumentHint: "", source: "", sigil: "/", anywhere: false)
            } + skills.map {
                CommandView(name: $0, description: "", argumentHint: "", source: "", sigil: "$", anywhere: true)
            })
    }

    func testALeadingSlashWordListsUpToFiveMatchingCommandsAndAPickFillsTheDraft() {
        let source = FakeChat(rows: [], frame: frame())
        source.offered = offering([
            "clear", "compact", "context", "cost", "config-check", "code-review:review",
            "documents:documents", "init",
        ])
        let model = ChatModel(source: source)
        model.draft = "/c"
        XCTAssertEqual(
            model.slashMatches.map(\.command.name), ["clear", "compact", "context", "cost", "config-check"],
            "five at most, in the agent's order")
        model.draft = "/Doc"
        XCTAssertEqual(model.slashMatches.map(\.command.name), ["documents:documents"])
        model.draft = "/rev"
        XCTAssertEqual(model.slashMatches.map(\.command.name), ["code-review:review"], "after a plugin's namespace")
        for draft in ["/compact now", "please /c", "c", "/a/b", ""] {
            model.draft = draft
            XCTAssertTrue(model.slashMatches.isEmpty, "\(draft) is not a leading /word")
        }
        model.draft = "/comp"
        model.pick(model.slashMatches[0])
        XCTAssertEqual(model.draft, "/compact ")
        XCTAssertTrue(model.slashMatches.isEmpty)
    }

    func testACodexSkillIsOfferedAfterADollarAnywhereAndPickedAsItsMention() {
        let source = FakeChat(rows: [], frame: frame(kind: .codex))
        source.offered = offering(["compact"], skills: ["autopilot", "documents:documents"])
        let model = ChatModel(source: source)
        model.draft = "/auto"
        XCTAssertTrue(model.slashMatches.isEmpty, "a skill is not a slash command")
        model.draft = "/comp"
        XCTAssertEqual(model.slashMatches.map(\.command.name), ["compact"])
        model.draft = "$doc"
        XCTAssertEqual(model.slashMatches.map(\.command.name), ["documents:documents"])
        for draft in ["$comp", "$auto now", "costs$auto"] {
            model.draft = draft
            XCTAssertTrue(model.slashMatches.isEmpty, "\(draft) ends in no skill mention")
        }
        model.draft = "please use $auto"
        XCTAssertEqual(model.slashMatches.map(\.command.name), ["autopilot"])
        model.pick(model.slashMatches[0])
        XCTAssertEqual(model.draft, "please use $autopilot ")
        XCTAssertTrue(model.slashMatches.isEmpty)
    }

    func testAnAgentOfferingNoCommandsListsNothing() {
        let source = FakeChat(rows: [], frame: frame(kind: .claudePty))
        source.offered = offering([])
        let model = ChatModel(source: source)
        model.draft = "/model"
        XCTAssertTrue(model.slashMatches.isEmpty)
    }

    func testASpaceAddedWherePastedTextMeetsAWordStaysInTheSentence() {
        let model = ChatModel(source: FakeChat(rows: [], frame: frame()))
        model.type("Please check")
        let log = (1...12).map { "line \($0)" }.joined(separator: "\n")
        model.type("Please check " + log)
        XCTAssertEqual(model.draft, "Please check ")
        XCTAssertEqual(model.attachments, [.text(name: "Pasted text", text: log)])
        model.type("Please check " + log + "against")
        XCTAssertEqual(model.draft, "Please check against")
        XCTAssertEqual(model.attachments.count, 1)
    }

    func testALongPasteBecomesOnePastedTextAttachmentSentInline() async {
        let source = FakeChat(rows: [], frame: frame())
        let model = ChatModel(source: source)
        model.type("Look at this: ")
        let log = (1...12).map { "line \($0)" }.joined(separator: "\r\n") + "\r\n"
        model.type("Look at this: " + log)
        XCTAssertEqual(model.draft, "Look at this: ", "the paste leaves the draft")
        let text = (1...12).map { "line \($0)" }.joined(separator: "\n") + "\n"
        XCTAssertEqual(model.attachments, [.text(name: "Pasted text", text: text)])
        XCTAssertEqual(ChatModel.lines(text), 12)
        model.type("Look at this: " + log)
        XCTAssertEqual(model.attachments.count, 1, "the text view's echo of the paste is not a second paste")
        XCTAssertEqual(model.draft, "Look at this: ")
        model.type("Look at this: " + log + "now")
        XCTAssertEqual(model.attachments.count, 1, "typing while the field still shows the paste")
        XCTAssertEqual(model.draft, "Look at this: now")
        model.type("Look at this: two\nlines")
        XCTAssertEqual(model.draft, "Look at this: two\nlines", "a short paste stays words")
        XCTAssertEqual(model.attachments.count, 1)
        model.type(model.draft + String(repeating: "x", count: 1_000))
        XCTAssertEqual(model.attachments.count, 2, "one very long line is a paste too")
        await model.send()?.value
        XCTAssertEqual(
            source.sent.first?.text, "Look at this: two\nlines\u{FFFC}\u{FFFC}",
            "each paste's place follows the words, where the sent message draws it")
        XCTAssertEqual(source.sent.first?.attachments?.first, .text(name: "Pasted text", text: text))
    }

    /// A prompt's text marks each attachment's place with U+FFFC, one per
    /// attachment in order (`Draft`'s contract, and docs/ATTACHMENTS.md):
    /// headless Claude reads an attachment only where its placeholder is,
    /// so a file, photo or long paste sent without one never reaches the
    /// model. The chips sit above the field, so the words carry none of
    /// their own; the sent draft must still place them.
    func testEveryAttachmentSentHasItsPlaceholderInTheText() async {
        let source = FakeChat(rows: [], frame: frame())
        let model = ChatModel(source: source)
        let first = model.attach(Data("2048 bytes".utf8), name: "noext", mime: "application/octet-stream", image: false)
        await model.attach(Data("png".utf8), name: "red.png", mime: "image/png", image: true)?.value
        await first?.value
        XCTAssertEqual(model.attachments.count, 2)
        model.type("What is in these?")
        await model.send()?.value
        let sent = source.sent.first
        let placeholders = sent?.text.unicodeScalars.filter { $0 == "\u{FFFC}" }.count
        XCTAssertEqual(sent?.attachments?.count, 2)
        XCTAssertEqual(
            placeholders, 2,
            "the sent text \(String(reflecting: sent?.text)) has no place for its attachments"
        )
    }

    func testAnAttachmentThatCouldNotBeStoredIsNamedAndNotSilentlyDropped() async {
        // On a cut or lossy link the phone's attachment vanished: no chip,
        // no retry, only "desk stopped answering; the call may or may not
        // have arrived", which names no file and is gone at the next tap.
        let source = FakeChat(rows: [], frame: frame())
        source.blobFailure = RuntimeFailure("desk stopped answering; the call may or may not have arrived")
        let model = ChatModel(source: source)
        await model.attach(Data(count: 1024), name: "mid.bin", mime: "application/octet-stream", image: false)?.value
        model.type("what is in it?")
        let said = (model.notice ?? "") + model.attachments.map { "\($0)" }.joined()
        XCTAssertTrue(
            said.contains("mid.bin"),
            "the failed attachment left no trace naming it: notice \(String(reflecting: model.notice)), chips \(model.attachments)")
    }

    func testAnAttachmentOverTheSizeLimitIsRefusedBeforeUploadNamingTheLimit() async {
        // An attachment has a stated limit of 64 MB, checked by the phone
        // before it uploads anything and refused in plain words naming it.
        let source = FakeChat(rows: [], frame: frame())
        let model = ChatModel(source: source)
        await model.attach(Data(count: 64 * 1024 * 1024 + 1), name: "huge.bin", mime: "application/octet-stream", image: false)?.value
        XCTAssertEqual(source.stored, [], "a file over the limit was handed over for upload")
        XCTAssertTrue(model.attachments.isEmpty, "a file over the limit became a chip")
        XCTAssertTrue(
            (model.notice ?? "").contains("64 MB") && (model.notice ?? "").contains("huge.bin"),
            "the refusal names the file and the 64 MB limit: \(String(reflecting: model.notice))")
    }

    func testAnEmptyChatSaysItIsLoadingOnlyAfterAMoment() async throws {
        let source = FakeChat(rows: [], frame: frame(caughtUp: false))
        let model = ChatModel(source: source)
        XCTAssertFalse(model.loadingHint)
        try await Task.sleep(for: ChatModel.loadingHintDelay + .milliseconds(200))
        XCTAssertTrue(model.loadingHint)
        source.ordered = [row("a", 1)]
        source.pending = ChatChanges(keys: ["a"], reloaded: false, session: false)
        source.deliver(to: model)
        XCTAssertFalse(model.loadingHint)
    }

    func testOpeningARunAsksTheRuntimeAndItsRowsComeWithTheNextDelta() {
        let run = Run(
            first: "a", last: "b", steps: 2, live: false, openBelow: false,
            unresolvedFailure: false, open: false, counts: RunCounts(commands: 0, edits: 0, reads: 2, searches: 0, subagents: 0, other: 0),
            recent: nil)
        let source = FakeChat(rows: [row("a", 1, run: run), row("b", 2, run: run), row("c", 3)], frame: frame())
        let model = ChatModel(source: source)
        let a = model.cell(for: "a")
        _ = model.cell(for: "c")
        model.toggleRun(run)
        XCTAssertEqual(source.toggled, ["b"], "the run is named by its newest step")
        source.deliver(to: model)
        XCTAssertEqual(a.row?.run?.open, true, "a drawn row of the run reads open")
        XCTAssertEqual(model.cell(for: "b").row?.run?.open, true)
        XCTAssertTrue(model.expanded.isEmpty, "a run is not a row's own detail")
        model.toggleRun(run)
        source.deliver(to: model)
        XCTAssertEqual(a.row?.run?.open, false)
    }

    func testARowInsideARunOpensItsOwnDetailNotTheRun() {
        let run = Run(
            first: "a", last: "b", steps: 2, live: false, openBelow: false,
            unresolvedFailure: false, open: false, counts: RunCounts(commands: 0, edits: 0, reads: 1, searches: 0, subagents: 1, other: 0),
            recent: nil)
        let source = FakeChat(rows: [row("a", 1, run: run), row("b", 2, run: run)], frame: frame())
        let model = ChatModel(source: source)
        _ = model.cell(for: "a")
        model.toggle("a")
        XCTAssertTrue(model.isExpanded("a"))
        XCTAssertTrue(source.toggled.isEmpty, "the run stays folded")
    }

    func testAQuestionCardIsAnsweredOrRepliedToInsteadPerQuestion() async {
        let source = FakeChat(rows: [row("a", 1)], frame: frame())
        source.card = AskCard(
            kind: .codex, key: "ask:1", itemKey: "", position: 1, count: 1, body: .question([]),
            choices: [], questionNote: true, questionSkip: true, questionReply: true,
            stopsTurn: true, defaultToNo: false, state: .open)
        let model = ChatModel(source: source)
        let answers = [
            QuestionResponse(note: "only on staging", other: nil, picked: [1]),
            QuestionResponse(note: nil, other: nil, picked: []),
        ]
        await model.answer(responses: answers)?.value
        XCTAssertEqual(source.responded, [answers], "a skip is an empty pick, and each note stays on its question")
        await model.replyInstead("Hold off for now", soFar: answers)?.value
        XCTAssertEqual(source.replied, ["Hold off for now"])
        XCTAssertEqual(source.responded.last, answers)
    }

    func testAQuestionCardsPicksStayUntilItsAskCloses() async {
        func card(_ key: String) -> AskCard {
            AskCard(
                kind: .claudeSdk, key: key, itemKey: "", position: 1, count: 1, body: .question([]),
                choices: [], questionNote: true, questionSkip: true, questionReply: true,
                stopsTurn: true, defaultToNo: false, state: .open)
        }
        let source = FakeChat(rows: [row("a", 1)], frame: frame())
        source.card = card("ask:1")
        let model = ChatModel(source: source)
        let picked = QuestionDraft(
            step: 1, picks: [[2], []], others: [nil, "Only on staging"], highlighted: [2, nil],
            skipped: [false, false], notes: ["", "and say when"], noting: [false, true])
        model.keep(picked, onAsk: "ask:1")
        model.keep(picked, onAsk: "ask:2")
        XCTAssertNil(model.questionDraft(onAsk: "ask:2"), "only the head ask keeps progress")

        source.pending = ChatChanges(keys: [], reloaded: false, session: true)
        source.deliver(to: model)
        XCTAssertEqual(model.questionDraft(onAsk: "ask:1"), picked, "the ask is still open")

        source.card = card("ask:2")
        source.pending = ChatChanges(keys: [], reloaded: false, session: true)
        source.deliver(to: model)
        XCTAssertNil(model.questionDraft(onAsk: "ask:1"), "the ask closed")

        source.card = card("ask:1")
        source.pending = ChatChanges(keys: [], reloaded: false, session: true)
        source.deliver(to: model)
        XCTAssertNil(model.questionDraft(onAsk: "ask:1"), "gone for good once closed")
    }

    func testAFormsValuesStayUntilItsAskCloses() async {
        func card(_ key: String) -> AskCard {
            AskCard(
                kind: .claudeSdk, key: key, itemKey: "", position: 1, count: 1,
                body: .form(server: "linear", message: "File an issue", fields: []),
                choices: [], questionNote: false, questionSkip: false, questionReply: false,
                stopsTurn: true, defaultToNo: false, state: .open)
        }
        let source = FakeChat(rows: [row("a", 1)], frame: frame())
        source.card = card("ask:1")
        let model = ChatModel(source: source)
        let typed: [FormValue] = [.text("Fleet rows lose their branch"), .choice(0)]
        model.keep(form: typed, onAsk: "ask:1")
        model.keep(form: typed, onAsk: "ask:2")
        XCTAssertEqual(model.formDraft(onAsk: "ask:1"), typed)
        XCTAssertNil(model.formDraft(onAsk: "ask:2"), "only the head ask keeps values")

        source.pending = ChatChanges(keys: [], reloaded: false, session: true)
        source.deliver(to: model)
        XCTAssertEqual(model.formDraft(onAsk: "ask:1"), typed, "the ask is still open")

        source.card = card("ask:2")
        source.pending = ChatChanges(keys: [], reloaded: false, session: true)
        source.deliver(to: model)
        XCTAssertNil(model.formDraft(onAsk: "ask:1"), "the ask closed")
        XCTAssertNil(model.formDraft(onAsk: "ask:2"), "a new form starts empty")
    }

    private func git(uncommitted: ChangeTotals?, onBranch: ChangeTotals?, base: String? = "main") -> Git {
        Git(baseBranch: base, branch: "fix", onBranch: onBranch, uncommitted: uncommitted)
    }

    func testTheHeaderCountsTheChosenComparisonFromTheFrame() async {
        var current = frame()
        current.git = git(
            uncommitted: ChangeTotals(files: 3, added: 42, removed: 7),
            onBranch: ChangeTotals(files: 6, added: 120, removed: 30))
        let source = FakeChat(rows: [row("a", 1)], frame: current)
        let model = ChatModel(source: source)
        XCTAssertEqual(model.comparison, .uncommitted)
        XCTAssertEqual(model.changes, ChangeTotals(files: 3, added: 42, removed: 7))
        XCTAssertEqual(model.comparisons, [.uncommitted, .onBranch])
        model.compare(.onBranch)
        XCTAssertEqual(model.changes, ChangeTotals(files: 6, added: 120, removed: 30))
        XCTAssertEqual(model.base, "main")
        XCTAssertEqual(source.reviews, 0, "the totals come with the frame; nothing is asked")
        XCTAssertEqual(source.listings, [], "nor are files listed while the overview is not shown")
    }

    func testNothingToCountShowsNoTotalsAndNoBaseOffersOneComparison() {
        var current = frame()
        current.git = git(uncommitted: ChangeTotals(files: 0, added: 0, removed: 0), onBranch: nil, base: nil)
        let model = ChatModel(source: FakeChat(rows: [row("a", 1)], frame: current))
        XCTAssertNil(model.changes)
        XCTAssertEqual(model.comparisons, [.uncommitted])
    }

    func testTheOverviewListsTheChangedFilesWhileShownAndAgainWhenTheyMove() async {
        var current = frame()
        current.git = git(
            uncommitted: ChangeTotals(files: 1, added: 2, removed: 0),
            onBranch: ChangeTotals(files: 2, added: 9, removed: 1))
        let source = FakeChat(rows: [row("a", 1)], frame: current)
        source.files = Changes(
            totals: ChangeTotals(files: 1, added: 2, removed: 0),
            folders: [Folder(path: "", files: [ChangedFile(
                path: "README.md", name: "README.md", added: 2, removed: 0, status: .modified, binary: false)])])
        let model = ChatModel(source: source)
        model.showOverview(true)
        await model.listing?.value
        XCTAssertEqual(source.listings, [.uncommitted])
        XCTAssertEqual(model.overview?.changes, source.files)

        // A wake that moves nothing asks nothing again.
        source.pending = ChatChanges(keys: [], reloaded: false, session: true)
        source.deliver(to: model)
        await model.listing?.value
        XCTAssertEqual(source.listings, [.uncommitted])

        // The turn ended with more changed: the totals moved.
        source.current.git = git(
            uncommitted: ChangeTotals(files: 2, added: 5, removed: 0),
            onBranch: ChangeTotals(files: 2, added: 9, removed: 1))
        source.pending = ChatChanges(keys: [], reloaded: false, session: true)
        source.deliver(to: model)
        await model.listing?.value
        XCTAssertEqual(source.listings, [.uncommitted, .uncommitted])

        model.compare(.onBranch)
        await model.listing?.value
        XCTAssertEqual(source.listings, [.uncommitted, .uncommitted, .onBranch])

        // Off screen, nothing is listed.
        model.showOverview(false)
        model.compare(.uncommitted)
        await model.listing?.value
        XCTAssertEqual(source.listings.count, 3)
    }

    func testAnAgentWithNoChangesHasNoReview() async {
        let source = FakeChat(rows: [row("a", 1)], frame: frame())
        source.working = FrozenReview(diff: ReviewFixtures.frozen.diff, patch: "")
        let model = ChatModel(source: source)
        guard case .success(nil) = await model.openReview() else {
            return XCTFail("an empty diff has no review")
        }
        source.working = nil
        guard case .failure = await model.openReview() else {
            return XCTFail("a host that was not asked says so")
        }
    }

    func testAnAttachedReviewIsOneDraftTokenThatSendsWithItsPatch() async {
        let source = FakeChat(rows: [row("a", 1)], frame: frame())
        source.working = ReviewFixtures.frozen
        let model = ChatModel(source: source)
        guard case .success(let opened?) = await model.openReview(),
              case .success(let again?) = await model.openReview()
        else { return XCTFail("the changes have a review") }
        let review = opened
        XCTAssertTrue(review === again, "the review being written is kept")
        review.begin(at: ReviewLine(file: 0, hunk: 0, line: 1))
        review.comment("Keep the old name.")
        model.attach(review)
        review.begin(at: ReviewLine(file: 1, hunk: 0, line: 0))
        review.comment("Say why.")
        model.attach(review)
        XCTAssertEqual(model.attachments.count, 1, "a second attach replaces the first token")
        guard case .review(let diff, let comments)? = model.attachments.first else {
            return XCTFail("the draft holds no review")
        }
        XCTAssertEqual(diff, ReviewFixtures.frozen.diff)
        XCTAssertEqual(comments.map(\.text), ["Keep the old name.", "Say why."])

        await model.send()?.value
        XCTAssertEqual(source.sent.first?.attachments, [.review(diff: ReviewFixtures.frozen.diff, comments: comments)])
    }

    func testDictationStreamsIntoTheDraftAndStopsOnSend() async {
        let source = FakeChat(rows: [row("a", 1)], frame: frame())
        let model = ChatModel(source: source)
        model.draft = "Please"
        model.dictation.prepare(speech: .allowed, microphone: .allowed, available: true)
        model.dictation.began(draft: model.draft)
        model.heard("run the")
        model.heard("rerun the tests")
        XCTAssertEqual(model.draft, "Please rerun the tests")
        let sending = model.send()
        XCTAssertFalse(model.dictation.active, "sending ends dictation")
        await sending?.value
        XCTAssertEqual(source.sent.map(\.text), ["Please rerun the tests"])
        model.heard("late words")
        XCTAssertEqual(model.draft, "", "nothing heard after the send lands in the next draft")
    }

    func testAPushPayloadNamesTheAgentAndItsHost() {
        let host = UUID(uuidString: "11111111-2222-3333-4444-555555555555")!
        let agent = UUID(uuidString: "66666666-7777-8888-9999-000000000000")!
        let payload: [AnyHashable: Any] = [
            "aps": ["content-available": 1],
            "amux": ["host": host.uuidString, "agent": agent.uuidString],
        ]
        XCTAssertEqual(PushPayload.agent(payload), AgentKey(host: HostId(host), agent: agent))
        XCTAssertNil(PushPayload.agent(["aps": ["content-available": 1]]))
    }

    func testOnlyNotificationsForAgentsThatStoppedNeedingYouAreStale() {
        let host = UUID(uuidString: "11111111-2222-3333-4444-555555555555")!
        let answered = UUID(uuidString: "66666666-7777-8888-9999-000000000000")!
        let waiting = UUID(uuidString: "66666666-7777-8888-9999-000000000001")!
        func payload(_ agent: UUID) -> [AnyHashable: Any] {
            ["amux": ["host": host.uuidString, "agent": agent.uuidString]]
        }
        let delivered: [(id: String, payload: [AnyHashable: Any])] = [
            ("answered", payload(answered)), ("waiting", payload(waiting)), ("other", ["aps": [:]]),
        ]
        let settled: Set = [AgentKey(host: HostId(host), agent: answered)]
        XCTAssertEqual(PushPayload.stale(delivered, settled: settled), ["answered"])
    }
}
