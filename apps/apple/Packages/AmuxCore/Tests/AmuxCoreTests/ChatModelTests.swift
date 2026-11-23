import AmuxFixtures
import AmuxWords
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
    /// The jobs asked to stop, by key.
    var stopped: [String] = []
    /// The groups the model opened or closed, by the member it named.
    var groupsToggled: [String] = []
    /// How the agents the rows open stand, as the fleet says.
    var agents = ChatAgents(agents: [], groups: [], live: .empty)
    var facts: Overview?
    var offered: SettingsView?
    var changed: [SettingChange] = []
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
    /// The review page opening it shows, and the page open on the chat.
    var reviewToShow: ReviewPage?
    var reviewOpen: ReviewPage?
    /// The open page moved since the last delta.
    var reviewMoved = false
    var reviewOpens = 0
    var reviewCloses = 0
    var reviewEdits: [[ReviewChange]] = []
    /// What every edit of the review answers.
    var editAnswer: ReviewOutcome = .done
    /// Drafts sent with the review's, and what such a send comes to.
    var sentWithReview: [Draft] = []
    var withReview: Result<SendOutcome, RuntimeFailure> = .success(SendOutcome(inputId: [7], state: .sent))

    /// What the model was last handed.
    private var sentKeys: [String] = []
    private var sentFrame: ChatFrame?
    private var sentCard: AskCard??
    private var sentFacts: Overview??
    private var sentOffered: SettingsView??
    private var sentAgents: ChatAgents?

    init(rows: [Row], frame: ChatFrame) {
        ordered = rows
        current = frame
    }

    func opening() -> ChatDelta {
        pending = ChatChanges(keys: [], reloaded: false, session: false)
        sentKeys = ordered.map(\.id)
        (sentFrame, sentCard, sentFacts, sentOffered) = (Self.drawn(current), card, facts, offered)
        sentAgents = agents
        return ChatDelta(
            chat: 0, rows: [], agents: agents, ask: AskMoved(card: card), frame: current,
            keys: .reload(sentKeys), overview: facts, review: nil, settings: offered)
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
        let agents = sentAgents == self.agents ? nil : self.agents
        sentAgents = self.agents
        let review = reviewMoved ? reviewOpen : nil
        reviewMoved = false
        guard keys != nil || !rows.isEmpty || frame != nil || ask != nil || overview != nil
            || settings != nil || agents != nil || review != nil
        else { return nil }
        return ChatDelta(
            chat: 0, rows: rows, agents: agents, ask: ask, frame: frame, keys: keys, overview: overview,
            review: review, settings: settings)
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

    /// What the next send comes back as.
    var sendState: InputState = .sent

    func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> {
        sent.append(draft)
        return .success(SendOutcome(inputId: [UInt8(sent.count)], state: sendState))
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
    func foldTurns(_ on: Bool) {}
    func toggleTurn(_ end: String) {}
    /// Opens or closes a group as the runtime does: every call of it says
    /// so from then on, and the first draws alone while it is folded.
    func toggleGroup(_ member: String) {
        groupsToggled.append(member)
        guard let first = ordered.first(where: { $0.id == member })?.group?.first else { return }
        for index in ordered.indices where ordered[index].group?.first == first {
            ordered[index].group?.open.toggle()
            let open = ordered[index].group?.open ?? false
            ordered[index].collapsed = !open && ordered[index].id != first
            pending.keys.append(ordered[index].id)
        }
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
    func stopJob(_ key: String) async -> ActOutcome? {
        stopped.append(key)
        return .done
    }

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
    /// What the shared layer would say of an attachment, by its name.
    var refusals: [String: String] = [:]

    func attachmentRefusal(name: String, mime: String, size: Int) -> String? { refusals[name] }

    func putBlob(_ data: Data, name: String, mime: String) async -> Result<BlobRef, RuntimeFailure> {
        stored.append(name)
        if let blobFailure { return .failure(blobFailure) }
        return .success(BlobRef(hash: [9], name: name, mime: mime, size: UInt64(data.count)))
    }

    /// Attachments' bytes, by hash, once they have landed.
    var landed: [[UInt8]: Data] = [:]
    var failedFetches: Set<[UInt8]> = []
    var fetchedAgain: [[UInt8]] = []

    func blob(_ hash: [UInt8]) -> Data? { landed[hash] }
    func blobFailed(_ hash: [UInt8]) -> Bool { failedFetches.contains(hash) }
    func fetchBlob(_ hash: [UInt8]) { fetchedAgain.append(hash) }


    func sendWithReview(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> {
        sentWithReview.append(draft)
        return withReview
    }

    func openReview(baseBranch: String?, refresh: Bool) async -> ReviewOutcome? {
        reviewOpens += 1
        reviewOpen = reviewToShow
        reviewMoved = reviewToShow != nil
        return .done
    }

    func editReview(_ changes: [ReviewChange]) async -> ReviewOutcome? {
        reviewEdits.append(changes)
        return editAnswer
    }

    func reviewPage() -> ReviewPage? { reviewOpen }

    func closeReview() {
        reviewCloses += 1
        reviewOpen = nil
    }

    func openOverview(_ comparison: Comparison) async -> Result<Overview, RuntimeFailure> {
        listings.append(comparison)
        var overview = facts ?? Overview(jobs: [], failedServers: [], changes: nil, tasks: nil, usageNearLimit: nil)
        overview.changes = files
        facts = overview
        return .success(overview)
    }
}

/// A call that started the agent `agent`, as the shared view draws it:
/// folded away under its group's first call until the group opens.
private func call(_ id: String, _ order: UInt64, opens agent: UInt8, group: SubagentGroup? = nil) -> Row {
    Row(
        id: id, order: order, atMs: 0,
        kind: .subagent(description: "Survey \(id)", continued: false, state: .succeeded, answer: "", durationMs: nil),
        collapsed: group.map { !$0.open && $0.first != id } ?? false, attention: false, decision: nil,
        group: group, opens: [agent], parent: nil, run: nil, turn: nil)
}

/// The fleet's word on each agent, by its id on host 1.
private func agents(_ standing: [(UInt8, PhaseView)]) -> ChatAgents {
    ChatAgents(
        agents: standing.map {
            OpenedAgent(agent: AgentKey(host: [1], agent: [$0.0]), name: "sub-\($0.0)", kind: .claudeSdk, phase: $0.1)
        },
        groups: [], live: .empty)
}

private func row(_ id: String, _ order: UInt64, _ text: String = "", run: Run? = nil) -> Row {
    Row(
        id: id, order: order, atMs: 0,
        kind: .prose(text: [.text(Markdown(words: text.isEmpty ? id : text))], streaming: false, workingNote: false),
        collapsed: false, attention: false, decision: nil, group: nil, opens: nil, parent: nil, run: run, turn: nil)
}

private func frame(
    mode: Composer = .send, caughtUp: Bool = true, hasOlder: Bool = false,
    phase: PhaseView = .idle, reviewChip: ReviewChip? = nil
) -> ChatFrame {
    ChatFrame(
        agent: AgentKey(host: [1], agent: [2]), name: "a",
        provider: ProviderLabel(provider: "claude", name: "Claude"),
        capabilities: Capabilities(
            stopsJob: true, switchesBack: false, attachable: false, resumable: true, sendsNow: true, images: true,
            files: true, passesUnknownCommands: true), phase: phase,
        composer: ComposerView(mode: mode, switchedAway: false, activity: nil), controls: ControlsSummary(effort: nil, mode: nil, model: nil, permission: nil), connection: .live, caughtUp: caughtUp,
        hasOlder: hasOlder, arrivalsHeld: false, queue: [], underway: [], refused: [], askInput: nil, context: nil, ended: nil, git: nil,
        reviewChip: reviewChip, signIn: nil)
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


    func testAResetSwapsTheWholeSequenceInAtOnceAndFollowsTheNewestRow() async {
        let source = FakeChat(rows: [row("a", 1), row("b", 2)], frame: frame())
        let model = ChatModel(source: source)
        let b = model.cell(for: "b")
        let before = model.toNewest

        source.ordered = [row("b", 20, "b again"), row("x", 21), row("y", 22)]
        source.pending = ChatChanges(keys: [], reloaded: true, session: true)
        source.deliver(to: model)
        XCTAssertEqual(model.ids, ["b", "x", "y"])
        XCTAssertTrue(model.cell(for: "b") === b, "a kept key keeps its cell")
        // The kept cells are read again away from the main actor.
        await model.reading?.value
        XCTAssertEqual(model.cell(for: "b").row?.order, 20, "and the cell holds the new row")
        XCTAssertEqual(model.toNewest, before + 1)
        XCTAssertFalse(model.newActivity)
    }

    /// The app's chats read their opening off the main actor, since the
    /// rows it hands over are built and parsed there; the chat fills when
    /// it comes, and what arrived since lands after it.
    func testAChatOpenedInTheBackgroundFillsWhenItsOpeningComes() async {
        let source = FakeChat(rows: [row("a", 1), row("b", 2)], frame: frame())
        let model = ChatModel(source: source, loadingHintAfter: .zero, openingInBackground: true)
        await model.reading?.value
        XCTAssertEqual(model.ids, ["a", "b"])
        source.arrive([row("c", 3)])
        source.deliver(to: model)
        XCTAssertEqual(model.ids, ["a", "b", "c"])
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
            first: "q", last: "r", steps: 300, live: false, openBelow: true, liveShown: false,
            unresolvedFailure: false, open: false, counts: RunCounts(commands: 0, edits: 0, reads: 300, searches: 0, lists: 0, fetches: 0, subagents: 0, other: 0, denied: 0), hidden: nil,
            recent: nil)
        let source = FakeChat(rows: [row("r", 5, run: run), row("s", 6)], frame: frame(hasOlder: true))
        let model = ChatModel(source: source)
        _ = model.cell(for: "r")
        await model.reachedTop()?.value
        XCTAssertEqual(source.paged, [300])

        let huge = Run(
            first: "q", last: "r", steps: 5_000, live: false, openBelow: true, liveShown: false,
            unresolvedFailure: false, open: false, counts: RunCounts(commands: 0, edits: 0, reads: 5_000, searches: 0, lists: 0, fetches: 0, subagents: 0, other: 0, denied: 0), hidden: nil,
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

    func testADraftIsNotSentWhileCatchingUpAndStaysInTheComposer() async {
        // Decided: a chat that is not current sends nothing. Send stays off,
        // the draft stays in the composer, and nothing holds it to send
        // later; the person sends it once the chat is current.
        let source = FakeChat(rows: [], frame: frame(mode: .waiting(.catchingUp), caughtUp: false))
        let model = ChatModel(source: source)
        model.draft = "Run the tests."
        XCTAssertFalse(model.canSend)
        XCTAssertNil(model.send())
        XCTAssertEqual(source.sent.map(\.text), [])
        XCTAssertEqual(model.draft, "Run the tests.")
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
    /// with all its words, a page review with its comments, not the line
    /// count and label its queued row shows.
    func testAWithdrawnPromptGivesBackItsPastedTextAndPageReview() async {
        let source = FakeChat(rows: [], frame: frame(phase: .working))
        let pasted = DraftAttachment.text(name: "Pasted text", text: "line one\nline two\nline three")
        let review = DraftAttachment.page(PageReview(
            url: "http://localhost:5173/", title: "Pricing",
            comments: [PageComment(text: "Say annual pricing.", quote: "", element: nil, image: nil)]))
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
        XCTAssertEqual(model.notice, "Not sent: its device is away")
        XCTAssertEqual(source.discarded, [[8]])
        XCTAssertEqual(model.frame?.refused, [], "taken back once")
    }

    /// A send refused at once, as one carrying an attachment the agent does
    /// not take is, puts the draft back with why, and once only though a
    /// later frame reports the same refusal.
    func testASendRefusedAtOnceKeepsTheDraftWithTheReason() async {
        let source = FakeChat(rows: [], frame: frame())
        source.sendState = .rejected(Refusal(reason: .unsupported, detail: "OpenCode can’t take images"))
        let model = ChatModel(source: source)
        model.draft = "What is this?"
        await model.send()?.value
        XCTAssertEqual(model.draft, "What is this?")
        XCTAssertEqual(source.discarded, [[1]])
        let notice = model.notice ?? ""
        XCTAssertTrue(notice.hasPrefix("Not sent"), notice)
        var later = frame()
        later.refused = [RefusedPrompt(inputId: [1], reason: Refusal(reason: .unsupported, detail: nil))]
        source.drafts[[1]] = Draft(words: "What is this?", attachments: [])
        model.apply(ChatDelta(
            chat: 0, rows: [], agents: nil, ask: nil, frame: later, keys: nil, overview: nil, review: nil,
            settings: nil))
        XCTAssertEqual(model.draft, "What is this?", "the draft came back twice")
    }

    private static let shot = BlobRef(hash: [5], name: "../shots/screen.png", mime: "image/png", size: 4)

    /// A row's change, as the session reports an attachment landing.
    private func landing(on model: ChatModel) {
        model.apply(ChatDelta(
            chat: 0, rows: [row("r1", 1, "words")], agents: nil, ask: nil, frame: nil, keys: nil, overview: nil,
            review: nil, settings: nil))
    }

    func testAnAttachmentHereOpensAsAFileUnderItsOwnName() throws {
        let source = FakeChat(rows: [], frame: frame())
        source.landed[Self.shot.hash] = Data("png!".utf8)
        let model = ChatModel(source: source)
        model.open(attachment: Self.shot)
        let opened = try XCTUnwrap(model.opened)
        XCTAssertEqual(opened.lastPathComponent, "screen.png")
        XCTAssertEqual(try Data(contentsOf: opened), Data("png!".utf8))
    }

    func testAnAttachmentAskedForBeforeItLandsOpensWhenItDoes() {
        let source = FakeChat(rows: [], frame: frame())
        let model = ChatModel(source: source)
        model.open(attachment: Self.shot)
        XCTAssertNil(model.opened)
        landing(on: model)
        XCTAssertNil(model.opened, "still on its way")
        source.landed[Self.shot.hash] = Data("png!".utf8)
        landing(on: model)
        XCTAssertNotNil(model.opened)
    }

    /// An attachment landing changes nothing in its row, and being handed the
    /// same row again is how its picture gets drawn.
    func testARowWithAnAttachmentIsReadAgainWhenHandedOverUnchanged() {
        var held = row("r1", 1)
        held.kind = .prose(text: [.attachment(.image(Self.shot))], streaming: false, workingNote: false)
        let source = FakeChat(rows: [held], frame: frame())
        let model = ChatModel(source: source)
        let cell = model.cell(for: "r1")
        let before = cell.attachmentRevision
        model.apply(ChatDelta(
            chat: 0, rows: [held], agents: nil, ask: nil, frame: nil, keys: nil, overview: nil, review: nil, settings: nil))
        XCTAssertGreaterThan(cell.attachmentRevision, before)
    }

    func testOpeningAnAttachmentThatFailedFetchesItAgain() {
        let source = FakeChat(rows: [], frame: frame())
        source.failedFetches = [Self.shot.hash]
        let model = ChatModel(source: source)
        model.open(attachment: Self.shot)
        XCTAssertEqual(source.fetchedAgain, [Self.shot.hash])
        XCTAssertNil(model.opened)
    }

    /// Prompts on their way split by where they land: the feed's end or
    /// the queue.
    func testPromptsOnTheirWayAreDrawnWhereTheyLand() async {
        var sending = frame()
        sending.underway = [
            SentPrompt(inputId: [1], text: [.text(Markdown(words: "now"))], lands: .feed, underway: .sending(waiting: false)),
            SentPrompt(inputId: [2], text: [.text(Markdown(words: "later"))], lands: .queue, underway: .mayNotHaveArrived),
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
        let source = FakeChat(rows: [], frame: frame())
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
        let source = FakeChat(rows: [], frame: frame())
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
            source.sent.first?.text, "Look at this: \u{FFFC}two\nlines\u{FFFC}",
            "each paste is sent where it was pasted among the words")
        XCTAssertEqual(source.sent.first?.attachments?.first, .text(name: "Pasted text", text: text))
    }

    /// A paste goes where the cursor was, with the draft's words on both
    /// sides of it, and stays there as the words around it change: the
    /// terminal and Android place it the same way.
    func testAPasteIsPlacedAtTheCursorAndMovesWithTheWordsAroundIt() async {
        let source = FakeChat(rows: [], frame: frame())
        let model = ChatModel(source: source)
        model.type("Before after")
        let log = (1...12).map { "line \($0)" }.joined(separator: "\n")
        model.type("Before " + log + "after")
        XCTAssertEqual(model.draft, "Before after", "the words on both sides stay in the draft")
        XCTAssertEqual(model.attachments, [.text(name: "Pasted text", text: log)])
        model.draft = "Look: before after"
        model.draft = "Look: before after, then send"
        await model.send()?.value
        XCTAssertEqual(source.sent.first?.text, "Look: before \u{FFFC}after, then send")
        XCTAssertEqual(
            source.sent.first?.parts,
            [.text("Look: before "), .attachment(.text(name: "Pasted text", text: log)), .text("after, then send")])
    }

    /// A draft put back keeps each paste where it was among its words.
    func testAPromptTakenBackKeepsItsPasteAmongItsWords() {
        let pasted = DraftAttachment.text(name: "Pasted text", text: "line one\nline two\nline three")
        let draft = Draft(parts: [.text("Read "), .attachment(pasted), .text(" closely.")])
        XCTAssertEqual(draft.places, [5])
        let model = ChatModel(source: FakeChat(rows: [], frame: frame()))
        var kept: Draft?
        model.keeping = { kept = $0 }
        model.reopen(draft)
        XCTAssertEqual(model.draft, "Read  closely.")
        XCTAssertEqual(kept, draft)
        model.draft = "Please: Read  closely."
        XCTAssertEqual(kept?.text, "Please: Read \u{FFFC} closely.")
    }

    /// A prompt put back into a composer that already holds a paste goes
    /// ahead of it, and the held paste moves past the words put back.
    func testADraftPutBackAheadOfAHeldPasteMovesThatPastePastItsWords() async {
        let source = FakeChat(rows: [], frame: frame())
        let model = ChatModel(source: source)
        let held = (1...12).map { "held \($0)" }.joined(separator: "\n")
        model.type(held)
        XCTAssertEqual(model.draft, "")
        XCTAssertEqual(model.attachments.count, 1)
        let returned = DraftAttachment.text(name: "Pasted text", text: "line one\nline two\nline three")
        model.reopen(Draft(parts: [.text("Read "), .attachment(returned), .text(" closely.")]))
        XCTAssertEqual(model.draft, "Read  closely.")
        await model.send()?.value
        XCTAssertEqual(source.sent.first?.text, "Read \u{FFFC} closely.\u{FFFC}")
    }

    /// Typing or deleting one of a run of repeated characters beside a
    /// paste moves it by the edit the field made, which the caret says:
    /// the texts alone read "abbc" the same whichever b was typed.
    func testAPasteMovesByTheCaretAmongRepeatedCharacters() {
        let model = ChatModel(source: FakeChat(rows: [], frame: frame()))
        var kept: Draft?
        model.keeping = { kept = $0 }
        let log = (1...12).map { "line \($0)" }.joined(separator: "\n")
        model.type("abc")
        model.type("ab" + log + "c")
        XCTAssertEqual(kept?.text, "ab\u{FFFC}c")
        model.type("ab" + log + "c")
        model.type("abbc", caret: 2)
        XCTAssertEqual(kept?.text, "abb\u{FFFC}c", "the b typed between a and b")
        model.type("abc", caret: 1)
        XCTAssertEqual(kept?.text, "ab\u{FFFC}c", "the b before the paste deleted")
        model.type("abbc", caret: 1)
        XCTAssertEqual(kept?.text, "ab\u{FFFC}bc", "a caret that has yet to move: the texts decide")
        for gap in [" ", "\n"] {
            let model = ChatModel(source: FakeChat(rows: [], frame: frame()))
            model.keeping = { kept = $0 }
            model.type("a\(gap)\(gap)b")
            model.type("a\(gap)" + log + "\(gap)b")
            model.type("a\(gap)" + log + "\(gap)b")
            XCTAssertEqual(kept?.text, "a\(gap)\u{FFFC}\(gap)b")
            model.type("a\(gap)b", caret: 1)
            XCTAssertEqual(kept?.text, "a\u{FFFC}\(gap)b", "the first of two \(String(reflecting: gap)) deleted")
            model.type("a\(gap)\(gap)b", caret: 3)
            XCTAssertEqual(kept?.text, "a\u{FFFC}\(gap)\(gap)b", "one typed after the paste")
        }
    }

    /// A paste after every word keeps its place when the draft is put back,
    /// so words typed after it go after it, as they did before.
    func testAPasteAfterEveryWordKeepsItsPlaceWhenPutBack() {
        let pasted = DraftAttachment.text(name: "Pasted text", text: "line one\nline two\nline three")
        let draft = Draft(parts: [.text("Look: "), .attachment(pasted)])
        XCTAssertEqual(draft.places, [6])
        let photo = DraftAttachment.image(BlobRef(hash: [1], name: "red.png", mime: "image/png", size: 3))
        XCTAssertEqual(Draft(parts: [.text("Look"), .attachment(photo)]).places, [nil], "a photo still follows every word")
        let model = ChatModel(source: FakeChat(rows: [], frame: frame()))
        var kept: Draft?
        model.keeping = { kept = $0 }
        model.reopen(draft)
        model.draft = "Look:  now"
        XCTAssertEqual(kept?.text, "Look: \u{FFFC} now")
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

    func testAnAttachmentTheSharedLayerRefusesIsNamedAndNeverStored() async {
        // The shared layer says what is refused before upload (over the
        // hosts' limit, or a kind the agent takes none of); the chat shows
        // its words and stores nothing.
        let source = FakeChat(rows: [], frame: frame())
        source.refusals["shot.png"] = "shot.png is not attached: OpenCode can’t take images."
        let model = ChatModel(source: source)
        await model.attach(Data(count: 16), name: "shot.png", mime: "image/png", image: true)?.value
        XCTAssertEqual(model.notice, "shot.png is not attached: OpenCode can’t take images.")
        XCTAssertEqual(source.stored, [], "a refused attachment was handed over for upload")
        XCTAssertTrue(model.attachments.isEmpty)
        await model.attach(Data(count: 16), name: "notes.txt", mime: "text/plain", image: false)?.value
        XCTAssertEqual(source.stored, ["notes.txt"], "one it takes is stored")
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

    // MARK: - Calls that open agents

    /// A call row reads how its subagent stands from the fleet, which moves
    /// without the row: the delta's agents carry it, and the cell is
    /// measured again though no row was handed over.
    func testACallReadsItsAgentsStandingFromTheFleetAsItMoves() throws {
        let source = FakeChat(rows: [call("c1", 1, opens: 9)], frame: frame())
        source.agents = agents([(9, .working)])
        let model = ChatModel(source: source)
        let cell = model.cell(for: "c1")
        let row = try XCTUnwrap(cell.row)
        XCTAssertEqual(AgentStanding.of(model.opened(by: row), call: .succeeded), .running,
                       "a call that returned while its subagent runs on reads running")
        let before = cell.revision
        source.agents = agents([(9, .needsYou)])
        source.deliver(to: model)
        XCTAssertEqual(source.fetched.count, 1, "no row was read again")
        XCTAssertEqual(AgentStanding.of(model.opened(by: row), call: .succeeded), .needsYou)
        XCTAssertGreaterThan(cell.revision, before, "the list measures the call again")
        XCTAssertEqual(model.opened(by: row)?.agent, AgentKey(host: [1], agent: [9]),
                       "the row opens the agent the fleet names")
    }

    /// Until the fleet lists a subagent, its call's own state stands in.
    func testACallWhoseAgentTheFleetDoesNotHoldReadsItsOwnState() throws {
        let source = FakeChat(rows: [call("c1", 1, opens: 9)], frame: frame())
        let model = ChatModel(source: source)
        let row = try XCTUnwrap(model.cell(for: "c1").row)
        XCTAssertNil(model.opened(by: row))
        XCTAssertEqual(AgentStanding.of(model.opened(by: row), call: .succeeded), .done)
        XCTAssertEqual(AgentStanding.of(nil, call: .running), .running)
        XCTAssertEqual(AgentStanding.of(nil, call: .cancelled), .stopped)
    }

    /// Calls of one turn that opened several agents draw as one row at the
    /// first until the reader opens the group, which is the runtime's fold;
    /// its state is what the fleet says the group comes to.
    func testAGroupDrawsAtItsFirstCallUntilOpened() throws {
        let group = SubagentGroup(first: "c1", agents: [[9], [10], [11]], open: false)
        let rows = [
            call("c1", 1, opens: 9, group: group), call("c2", 2, opens: 10, group: group),
            call("c3", 3, opens: 11, group: group),
        ]
        let source = FakeChat(rows: rows, frame: frame())
        let standing = GroupState(
            agents: 3, running: 2, needsYou: 1, idle: 0, finished: 0, ended: 0, failed: 0, unknown: 0)
        source.agents = ChatAgents(
            agents: [], groups: [GroupView(first: "c1", state: standing)], live: standing)
        let model = ChatModel(source: source)
        XCTAssertEqual(model.liveSubagents, standing, "the activity line's subagents, from the fleet")
        let shown = ["c1", "c2", "c3"].filter { model.shows(model.cell(for: $0).row!) }
        XCTAssertEqual(shown, ["c1"], "a folded group draws at its first call alone")
        let first = try XCTUnwrap(model.cell(for: "c1").row?.group)
        XCTAssertEqual(model.state(of: first).map(ChatWords.subagentGroupState), "2 running · 1 waiting on you")
        XCTAssertEqual(model.state(of: first).map(ChatWords.subagentGroupStanding), .needsYou)
        model.toggleGroup(first)
        XCTAssertEqual(source.groupsToggled, ["c1"])
        source.deliver(to: model)
        XCTAssertEqual(["c1", "c2", "c3"].filter { model.shows(model.cell(for: $0).row!) }, ["c1", "c2", "c3"],
                       "opened, each call draws its own row")
        XCTAssertTrue(model.expanded.isEmpty, "a group is not a row's own detail")
    }

    /// A provider subagent's chat has no text box: nothing typed is sent,
    /// and its Stop stops its own job by the key the composer gives.
    func testANativeChatSendsNothingAndStopsItsOwnJob() async {
        let source = FakeChat(
            rows: [], frame: frame(mode: .native(stop: .offered(key: "task-7")), phase: .working))
        let model = ChatModel(source: source)
        model.draft = "hello"
        XCTAssertFalse(model.canSend)
        XCTAssertFalse(model.canResume)
        XCTAssertEqual(model.frame?.composer.mode.nativeStop, .offered(key: "task-7"))
        XCTAssertNil(Composer.send.nativeStop)
        await model.stopJob("task-7")?.value
        XCTAssertEqual(source.stopped, ["task-7"])
        XCTAssertNil(model.notice)
    }

    func testOpeningARunAsksTheRuntimeAndItsRowsComeWithTheNextDelta() {
        let run = Run(
            first: "a", last: "b", steps: 2, live: false, openBelow: false, liveShown: false,
            unresolvedFailure: false, open: false, counts: RunCounts(commands: 0, edits: 0, reads: 2, searches: 0, lists: 0, fetches: 0, subagents: 0, other: 0, denied: 0), hidden: nil,
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
            first: "a", last: "b", steps: 2, live: false, openBelow: false, liveShown: false,
            unresolvedFailure: false, open: false, counts: RunCounts(commands: 0, edits: 0, reads: 1, searches: 0, lists: 0, fetches: 0, subagents: 1, other: 0, denied: 0), hidden: nil,
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
            choices: [], questionNote: true, questionSkip: true, questionReply: true, replyInstead: false,
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
                choices: [], questionNote: true, questionSkip: true, questionReply: true, replyInstead: false,
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
                choices: [], questionNote: false, questionSkip: false, questionReply: false, replyInstead: false,
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
        Git(baseBranch: base, branch: "fix", checkout: nil, onBranch: onBranch, uncommitted: uncommitted)
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
        XCTAssertEqual(source.listings, [], "the totals come with the frame; no files are listed while the overview is not shown")
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
                path: "README.md", name: "README.md", added: 2, removed: 0, status: .modified, binary: false,
                language: "markdown", oldPath: nil)])],
            base: nil)
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

        // On the branch the overview names the base the host reported, which
        // need not be the row's.
        XCTAssertEqual(model.listedBase, "main", "the row's base until the host has listed the branch")
        source.files?.base = "origin/main"
        model.compare(.onBranch)
        await model.listing?.value
        XCTAssertEqual(source.listings, [.uncommitted, .uncommitted, .onBranch])
        XCTAssertEqual(model.listedBase, "origin/main")
        XCTAssertEqual(model.base, "main", "the review still compares with the row's base")

        // Off screen, nothing is listed.
        model.showOverview(false)
        model.compare(.uncommitted)
        await model.listing?.value
        XCTAssertEqual(source.listings.count, 3)
    }

    // MARK: - The agent's review

    /// The chip is the agent's row's: while it shows, a send carries the
    /// review's drafts, with or without words of its own, and it is held
    /// back from then until the row says something new of them.
    func testTheChipFromTheRowSendsTheReviewsDrafts() async {
        let chip = ReviewChip(comments: 2, moved: false)
        let source = FakeChat(rows: [row("a", 1)], frame: frame(reviewChip: chip))
        let model = ChatModel(source: source)
        XCTAssertEqual(model.reviewChip, chip)
        XCTAssertFalse(model.hasDraft)
        XCTAssertTrue(model.hasMessage, "the chip's drafts are a message")
        XCTAssertTrue(model.canSend)

        model.draft = "Look at these first."
        await model.send()?.value
        XCTAssertEqual(source.sentWithReview, [Draft(words: "Look at these first.", attachments: [])])
        XCTAssertTrue(source.sent.isEmpty)
        XCTAssertNil(model.reviewChip, "the drafts just sent are not offered again")
        XCTAssertFalse(model.canSend)

        // Before the row says they were sent, a send carries words alone.
        model.draft = "And the docs."
        await model.send()?.value
        XCTAssertEqual(source.sent, [Draft(words: "And the docs.", attachments: [])])

        // The host marked them sent: the row has no chip. A draft written
        // after brings it back.
        source.current.reviewChip = nil
        source.deliver(to: model)
        XCTAssertNil(model.reviewChip)
        source.current.reviewChip = chip
        source.deliver(to: model)
        XCTAssertEqual(model.reviewChip, chip, "a chip like the one sent is new drafts")

        // Drafts written on another client meanwhile show at once.
        await model.send()?.value
        XCTAssertEqual(source.sentWithReview.count, 2, "the chip alone sends")
        source.current.reviewChip = ReviewChip(comments: 3, moved: true)
        source.deliver(to: model)
        XCTAssertEqual(model.reviewChip, ReviewChip(comments: 3, moved: true))
    }

    /// A queued prompt that carried the review, withdrawn, closed no round:
    /// its words come back without the review, and the chip is back, so the
    /// next send reads the drafts as they are then and carries one review.
    func testAWithdrawnReviewPromptBringsTheChipBack() async {
        let chip = ReviewChip(comments: 2, moved: false)
        let source = FakeChat(rows: [row("a", 1)], frame: frame(phase: .working, reviewChip: chip))
        source.withReview = .success(SendOutcome(inputId: [7], state: .queued))
        let model = ChatModel(source: source)
        model.draft = "Look at these."
        await model.send()?.value
        XCTAssertNil(model.reviewChip, "held back while the prompt waits")

        // The bridge's draft of a queued prompt never carries the review.
        source.drafts[[7]] = Draft(words: "Look at these.", attachments: [])
        let sent = SentReviewRef(reviewId: [1], round: 1, inputId: [7])
        let queued = QueuedRow(
            inputId: [7],
            text: [.text("Look at these. "), .attachment(.sentReview(comments: 2, review: sent, text: ""))],
            steered: false, canWithdraw: true, canSendNow: true, fromAgent: nil)
        await model.withdraw(queued)?.value
        XCTAssertEqual(source.withdrawn, [[7]])
        XCTAssertEqual(model.draft, "Look at these.")
        XCTAssertTrue(model.attachments.isEmpty, "the review stays behind")
        XCTAssertEqual(model.reviewChip, chip, "the drafts are the chip's again")

        await model.send()?.value
        XCTAssertEqual(source.sentWithReview.count, 2, "the next send reads the drafts afresh")
        XCTAssertEqual(source.sentWithReview.last, Draft(words: "Look at these.", attachments: []))
    }

    func testASendWithTheReviewThatFailsKeepsTheWordsAndTheChip() async {
        let chip = ReviewChip(comments: 1, moved: false)
        let source = FakeChat(rows: [row("a", 1)], frame: frame(reviewChip: chip))
        source.withReview = .failure(RuntimeFailure("the review holds no drafts to send"))
        let model = ChatModel(source: source)
        model.draft = "Summary."
        await model.send()?.value
        XCTAssertEqual(model.draft, "Summary.")
        XCTAssertEqual(model.notice, "the review holds no drafts to send")
        XCTAssertEqual(model.reviewChip, chip)

        // Refused by the agent: nothing was marked sent, so the chip stays.
        source.withReview = .success(SendOutcome(inputId: [7], state: .rejected(Refusal(reason: .exited, detail: nil))))
        await model.send()?.value
        XCTAssertEqual(model.reviewChip, chip)

        // Not known to have landed: the drafts stay drafts until the agent
        // takes a message carrying them, so the chip shows them again.
        source.withReview = .success(SendOutcome(inputId: [8], state: .uncertain))
        await model.send()?.value
        XCTAssertEqual(model.reviewChip, chip)
    }

    func testWithoutAChipASendCarriesNoReview() async {
        let source = FakeChat(rows: [row("a", 1)], frame: frame())
        let model = ChatModel(source: source)
        XCTAssertNil(model.reviewChip)
        XCTAssertFalse(model.canSend, "nothing to send")
        model.draft = "Hello."
        await model.send()?.value
        XCTAssertEqual(source.sent.count, 1)
        XCTAssertTrue(source.sentWithReview.isEmpty)
    }

    /// The page opens for a look, moves with the chat's deltas while it is
    /// open, and stops when it closes; each look opens the review again.
    func testTheReviewPageFollowsTheChatsDeltasWhileOpen() async {
        let source = FakeChat(rows: [row("a", 1)], frame: frame())
        let page = ReviewPage(
            banners: [.changedSince], rows: [], toFetch: [], opening: false, empty: .nothing, header: nil)
        source.reviewToShow = page
        let model = ChatModel(source: source)
        XCTAssertNil(model.reviewPage)
        let session = model.openReviewPage()
        XCTAssertTrue(model.reviewPage === session)
        _ = await session.latest?.value
        XCTAssertEqual(source.reviewOpens, 1)
        source.deliver(to: model)
        XCTAssertEqual(session.page, page)

        var moved = page
        moved.banners = []
        source.reviewOpen = moved
        source.reviewMoved = true
        source.deliver(to: model)
        XCTAssertEqual(session.page, moved)

        let again = model.openReviewPage()
        XCTAssertTrue(again === session, "the page open on the chat is the one looked at again")
        _ = await again.latest?.value
        XCTAssertEqual(source.reviewOpens, 2)

        model.closeReviewPage()
        XCTAssertNil(model.reviewPage)
        XCTAssertTrue(session.closed)
        XCTAssertEqual(source.reviewCloses, 1)
        source.reviewOpen = page
        source.reviewMoved = true
        source.deliver(to: model)
        XCTAssertEqual(session.page, moved, "a closed page is handed nothing")

        source.reviewOpen = nil
        let next = model.openReviewPage()
        XCTAssertFalse(next === session)
        XCTAssertEqual(next.page, ReviewSession.opening)
    }

    func testTheChipsCrossDiscardsTheDraftsAtTheHost() async {
        let source = FakeChat(rows: [row("a", 1)], frame: frame(reviewChip: ReviewChip(comments: 2, moved: false)))
        let model = ChatModel(source: source)
        await model.discardReviewDrafts().value
        XCTAssertEqual(source.reviewEdits, [[.discardDrafts]])
        XCTAssertNil(model.notice)
        XCTAssertNil(model.reviewPage, "no page opens for it")

        source.editAnswer = .unreachable
        await model.discardReviewDrafts().value
        XCTAssertEqual(model.notice, "This agent’s device can’t be reached, so nothing changed.")
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
