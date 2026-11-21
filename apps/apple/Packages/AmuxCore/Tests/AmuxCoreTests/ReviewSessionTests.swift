import AmuxWords
import Foundation
import XCTest

@testable import AmuxCore

/// A chat whose agent has a review, as the runtime's review page answers:
/// what each act on the page was asked, the page open on the chat, and
/// where a selection of its rows would be commented on.
private final class ReviewChat: ChatSource, @unchecked Sendable {
    /// The page the runtime holds open on the chat, nil with none open.
    var open: ReviewPage?
    /// What every act on the review answers.
    var answering: ReviewOutcome = .done
    /// What `reviewFiles` answers, when it differs from the rest.
    var filesAnswer: ReviewOutcome?
    /// Where a selection of rows would be commented on, by the indices
    /// asked; nil for any other selection.
    var anchors: [[Int]: ReviewAnchor] = [:]
    var opens: [(base: String?, refresh: Bool)] = []
    var fetches: [[String]] = []
    var contexts: [ReviewContextAsk] = []
    var edits: [[ReviewChange]] = []
    var choices: [ReviewChoice] = []
    var anchorsAsked: [[Int]] = []
    var closes = 0

    func openReview(baseBranch: String?, refresh: Bool) async -> ReviewOutcome? {
        opens.append((baseBranch, refresh))
        return answering
    }

    func reviewFiles(_ paths: [String]) async -> ReviewOutcome? {
        fetches.append(paths)
        return filesAnswer ?? answering
    }

    func reviewContext(_ ask: ReviewContextAsk) async -> ReviewOutcome? {
        contexts.append(ask)
        return answering
    }

    func editReview(_ changes: [ReviewChange]) async -> ReviewOutcome? {
        edits.append(changes)
        return answering
    }

    func chooseInReview(_ choice: ReviewChoice) { choices.append(choice) }
    func reviewPage() -> ReviewPage? { open }

    func reviewAnchor(from: Int, to: Int) -> ReviewAnchor? {
        anchorsAsked.append([from, to])
        return anchors[[from, to]]
    }

    func closeReview() {
        closes += 1
        open = nil
    }

    // The chat around the review, which these tests do not read.

    func opening() -> ChatDelta {
        ChatDelta(
            chat: 0, rows: [], agents: nil, ask: nil, frame: nil, keys: .reload([]), overview: nil,
            review: open, settings: nil)
    }
    func follow(_ following: Bool) {}
    func rows(for keys: [String]) -> [Row] { [] }
    func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> {
        .success(SendOutcome(inputId: [1], state: .sent))
    }
    func answer(_ ask: String, choice: Int, note: String?) async -> ActOutcome? { .done }
    func answer(_ ask: String, responses: [QuestionResponse]) async -> ActOutcome? { .done }
    func replyInstead(_ ask: String, text: String, soFar: [QuestionResponse]) async -> ActOutcome? { .done }
    func toggleRun(_ member: String) {}
    func toggleGroup(_ member: String) {}
    func foldTurns(_ on: Bool) {}
    func toggleTurn(_ end: String) {}
    func answerForm(_ ask: String, values: [FormValue]) async -> ActOutcome? { .done }
    func withdraw(_ input: [UInt8]) async -> ActOutcome? { .done }
    func draft(of input: [UInt8]) -> Draft? { nil }
    func sendNow(_ input: [UInt8]) async -> ActOutcome? { .done }
    func resend(_ input: [UInt8]) async -> SendOutcome? { nil }
    func discard(_ input: [UInt8]) {}
    func interrupt() async -> ActOutcome? { .done }
    func stopJob(_ key: String) async -> ActOutcome? { .done }
    func change(_ setting: SettingChange) async -> ActOutcome? { .done }
    func resume(with draft: Draft) async -> ActOutcome? { .done }
    func pageOlder(_ rows: UInt32) async -> PageOutcome? { .arrived(0) }
    func putBlob(_ data: Data, name: String, mime: String) async -> Result<BlobRef, RuntimeFailure> {
        .failure(RuntimeFailure("no blobs"))
    }
    func blob(_ hash: [UInt8]) -> Data? { nil }
    func openOverview(_ comparison: Comparison) async -> Result<Overview, RuntimeFailure> {
        .failure(RuntimeFailure("no overview"))
    }
}

/// Review pages as the shared view composes them.
enum ReviewPages {
    static let snapshot = "5555555555555555555555555555555555555555"
    static let base = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

    static func header(
        snapshot: String = snapshot, view: ReviewView = .all, drafts: UInt32 = 0
    ) -> ReviewHeader {
        ReviewHeader(
            baseBranch: "main", baseCommit: base, snapshot: snapshot, view: view, comparedWith: base,
            comparedBase: "", views: [ReviewViewTab(view: .all, files: 2)], rounds: 0, files: 2, added: 3, removed: 1,
            drafts: drafts, sent: 0, discardDrafts: drafts > 0, startOver: true)
    }

    static func file(_ path: String) -> ReviewRow {
        ReviewRow(
            key: "file:\(path)",
            of: .file(ReviewFileHead(
                path: path, status: .modified, added: 2, removed: 1, binary: false, language: "rust",
                flags: [], folded: false, changedSinceLast: false, inChange: true, comments: 0,
                outline: [], oldPath: nil)))
    }

    static func line(_ path: String, _ number: UInt32) -> ReviewRow {
        ReviewRow(
            key: "line:\(path):\(number)",
            of: .line(ReviewCodeLine(
                path: path, kind: .added, text: "let x = \(number);", runs: [], commented: false,
                extra: false, newLine: number, oldLine: nil)))
    }

    static func loading(_ path: String) -> ReviewRow {
        ReviewRow(key: "loading:\(path)", of: .loading(path: path))
    }

    static func page(
        _ rows: [ReviewRow], toFetch: [String] = [], banners: [ReviewBanner] = [],
        header: ReviewHeader? = header()
    ) -> ReviewPage {
        ReviewPage(banners: banners, rows: rows, toFetch: toFetch, opening: false, empty: nil, header: header)
    }

    static let anchor = ReviewAnchor(commit: snapshot, path: "src/lib.rs", start: 11, end: 12)
}

@MainActor
final class ReviewSessionTests: XCTestCase {
    private typealias P = ReviewPages

    func testAPageStartsAsTheOneOpenOnTheChatOrOpening() {
        let none = ReviewSession(source: ReviewChat())
        XCTAssertEqual(none.page, ReviewSession.opening)
        XCTAssertTrue(none.page.opening)
        XCTAssertNil(none.page.header)

        let chat = ReviewChat()
        let open = P.page([P.file("src/lib.rs")])
        chat.open = open
        XCTAssertEqual(ReviewSession(source: chat).page, open, "a page already open is drawn at once")
    }

    func testOpeningRefreshingAndPickingABaseAskTheHost() async {
        let chat = ReviewChat()
        let session = ReviewSession(source: chat)
        let opened = await session.open()?.value
        XCTAssertEqual(opened, .done)
        _ = await session.refresh()?.value
        _ = await session.open(base: "release")?.value
        XCTAssertEqual(chat.opens.map(\.base), [nil, nil, "release"])
        XCTAssertEqual(chat.opens.map(\.refresh), [false, true, false])
        XCTAssertNil(session.notice)
        XCTAssertEqual(session.waiting, 0)
    }

    /// The page is drawn as it was handed: a later page replaces it whole.
    func testThePageIsTheOneHandedOverLast() {
        let session = ReviewSession(source: ReviewChat())
        let first = P.page([P.file("a.rs")])
        let second = P.page([P.file("a.rs"), P.line("a.rs", 1)], banners: [.changedSince])
        session.apply(first)
        XCTAssertEqual(session.page, first)
        session.apply(second)
        XCTAssertEqual(session.page, second)
        XCTAssertEqual(session.page.header, second.header)
    }

    /// What the page still needs is asked for once, in page order, however
    /// often the page moves before it lands; and asked again if the page
    /// still lists it after the answer.
    func testTheFilesThePageNeedsAreAskedForOnce() async {
        let chat = ReviewChat()
        let session = ReviewSession(source: chat)
        let waiting = P.page(
            [P.file("a.rs"), P.loading("a.rs"), P.file("b.rs"), P.loading("b.rs")],
            toFetch: ["a.rs", "b.rs"])
        session.apply(waiting)
        let asked = session.latest
        XCTAssertNotNil(asked)
        session.apply(waiting)
        XCTAssertEqual(session.latest, asked, "a page that moves before they land asks nothing twice")
        // The page that says they landed comes before the answer.
        session.apply(P.page([P.file("a.rs"), P.line("a.rs", 1), P.file("b.rs"), P.line("b.rs", 1)]))
        _ = await asked?.value
        XCTAssertEqual(chat.fetches, [["a.rs", "b.rs"]])

        // They landed under another view than the page showed, so a page
        // handed over now still needs one: it is asked for again.
        session.apply(P.page([P.file("b.rs"), P.loading("b.rs")], toFetch: ["b.rs"]))
        XCTAssertNotEqual(session.latest, asked)
        _ = await session.latest?.value
        XCTAssertEqual(chat.fetches, [["a.rs", "b.rs"], ["b.rs"]])
    }

    func testANewSnapshotOrViewAsksForItsOwnBodies() async {
        let chat = ReviewChat()
        let session = ReviewSession(source: chat)
        // Failing, so nothing asked is let go.
        chat.filesAnswer = .unreachable
        session.apply(P.page([P.file("a.rs")], toFetch: ["a.rs"]))
        _ = await session.latest?.value
        session.apply(P.page([P.file("a.rs")], toFetch: ["a.rs"], header: P.header(view: .sinceLast)))
        _ = await session.latest?.value
        session.apply(P.page(
            [P.file("a.rs")], toFetch: ["a.rs"],
            header: P.header(snapshot: "6666666666666666666666666666666666666666")))
        _ = await session.latest?.value
        XCTAssertEqual(chat.fetches, [["a.rs"], ["a.rs"], ["a.rs"]])
    }

    /// A fetch that fails is not asked again by every page that follows,
    /// which would ask a host out of reach in a loop: it is asked again when
    /// the host answers again, or the page is opened again.
    func testAFailedFetchIsAskedAgainOnlyWhenItCanAnswer() async {
        let chat = ReviewChat()
        let session = ReviewSession(source: chat)
        chat.filesAnswer = .unreachable
        let needing = P.page([P.file("a.rs"), P.loading("a.rs")], toFetch: ["a.rs"])
        session.apply(needing)
        _ = await session.latest?.value
        let first = session.latest
        session.apply(P.page(needing.rows, toFetch: ["a.rs"], banners: [.unreachable]))
        session.apply(P.page(needing.rows, toFetch: ["a.rs"], banners: [.unreachable]))
        XCTAssertEqual(session.latest, first, "a failing host is not asked in a loop")
        XCTAssertEqual(chat.fetches.count, 1)
        XCTAssertNil(session.notice, "the page's banner says the host is out of reach")

        chat.filesAnswer = nil
        session.apply(needing)
        _ = await session.latest?.value
        XCTAssertEqual(chat.fetches.count, 2, "the host answers again, so the file is asked for again")

        chat.filesAnswer = .unreachable
        let elsewhere = P.page(needing.rows, toFetch: ["a.rs"], header: P.header(snapshot: "77"))
        session.apply(elsewhere)
        _ = await session.latest?.value
        session.apply(elsewhere)
        _ = await session.latest?.value
        XCTAssertEqual(chat.fetches.count, 3)
        _ = await session.open()?.value
        session.apply(elsewhere)
        _ = await session.latest?.value
        XCTAssertEqual(chat.fetches.count, 4, "opening the page again asks again")
    }

    func testAFetchThatFailsInWordsSaysWhy() async {
        let chat = ReviewChat()
        chat.filesAnswer = .failed("the agent's host answered none of the files asked for")
        let session = ReviewSession(source: chat)
        session.apply(P.page([P.file("a.rs")], toFetch: ["a.rs"]))
        _ = await session.latest?.value
        XCTAssertEqual(session.notice, "the agent's host answered none of the files asked for")
    }

    func testMoreContextAsksForTheGapsLines() async {
        let chat = ReviewChat()
        let session = ReviewSession(source: chat)
        let ask = ReviewContextAsk(path: "a.rs", commit: P.snapshot, start: 4, end: 24, definition: "fn open")
        _ = await session.moreContext(ask)?.value
        XCTAssertEqual(chat.contexts, [ask])
    }

    func testChoicesGoToTheRuntimeWhichDrawsThem() {
        let chat = ReviewChat()
        let session = ReviewSession(source: chat)
        session.show(.sinceLast)
        session.toggleFile("Cargo.lock")
        session.toggleComment([7])
        XCTAssertEqual(chat.choices, [.view(.sinceLast), .toggleFile("Cargo.lock"), .toggleComment([7])])
        XCTAssertEqual(session.page, ReviewSession.opening, "nothing is drawn here until the page is handed over")
    }

    /// A selection is rows of the page, turned into lines by the runtime
    /// against the page it handed over; as the page moves, the selection
    /// stays on its rows.
    func testASelectionStaysOnItsRowsAsThePageMoves() {
        let chat = ReviewChat()
        let session = ReviewSession(source: chat)
        session.apply(P.page([P.file("src/lib.rs"), P.line("src/lib.rs", 11), P.line("src/lib.rs", 12)]))
        chat.anchors = [[2, 1]: P.anchor, [3, 2]: P.anchor]
        session.select(2)
        XCTAssertEqual(session.selected, 2...2)
        XCTAssertNil(session.anchor)
        session.extend(to: 1)
        XCTAssertEqual(session.selected, 1...2)
        XCTAssertEqual(session.anchor, P.anchor)
        XCTAssertTrue(session.canComment)
        XCTAssertTrue(session.isSelected(1))
        XCTAssertFalse(session.isSelected(0))

        // A banner's row above: the same rows, one further down.
        session.apply(P.page([
            P.file("other.rs"), P.file("src/lib.rs"), P.line("src/lib.rs", 11), P.line("src/lib.rs", 12),
        ]))
        XCTAssertEqual(session.selected, 2...3)
        XCTAssertEqual(chat.anchorsAsked.last, [3, 2], "the runtime is asked again for the page it handed")
        XCTAssertEqual(session.anchor, P.anchor)

        // A selected row leaves the page: the selection goes with it.
        session.apply(P.page([P.file("other.rs"), P.file("src/lib.rs"), P.line("src/lib.rs", 11)]))
        XCTAssertNil(session.selected)
        XCTAssertNil(session.anchor)
        XCTAssertFalse(session.canComment)

        session.select(99)
        XCTAssertNil(session.selected, "a row the page does not hold selects nothing")
        session.extend(to: 1)
        XCTAssertNil(session.selected, "nothing is extended without a start")
    }

    func testACommentGoesOnTheSelectionsLinesAndEndsIt() async {
        let chat = ReviewChat()
        let session = ReviewSession(source: chat)
        session.apply(P.page([P.file("src/lib.rs"), P.line("src/lib.rs", 11)]))
        XCTAssertNil(session.comment("No lines selected."), "with no selection nothing is written")
        chat.anchors = [[1, 1]: P.anchor]
        session.select(1)
        XCTAssertNil(session.comment("  \n "), "blank words write nothing")
        let written = session.comment("  Say why.\n")
        XCTAssertEqual(session.waiting, 1)
        let outcome = await written?.value
        XCTAssertEqual(outcome, .done)
        XCTAssertEqual(chat.edits, [[.comment(anchor: P.anchor, text: "Say why.")]])
        XCTAssertNil(session.selected, "the host has it, so the selection ends")
        XCTAssertEqual(session.waiting, 0)
    }

    func testACommentTheHostDidNotTakeKeepsTheSelectionAndSaysWhy() async {
        let chat = ReviewChat()
        let session = ReviewSession(source: chat)
        session.apply(P.page([P.file("src/lib.rs"), P.line("src/lib.rs", 11)]))
        chat.anchors = [[1, 1]: P.anchor]
        session.select(1)
        chat.answering = .unreachable
        _ = await session.comment("Say why.")?.value
        XCTAssertEqual(session.selected, 1...1)
        XCTAssertEqual(session.notice, "This agent’s computer can’t be reached, so nothing changed.")

        chat.answering = .failed("the review holds 500 comments already")
        _ = await session.comment("Say why.")?.value
        XCTAssertEqual(session.notice, "the review holds 500 comments already")

        chat.answering = .done
        _ = await session.comment("Say why.")?.value
        XCTAssertNil(session.notice, "the next act clears what the last said")
    }

    /// Opening, refreshing and more context change nothing at the host: a
    /// host out of reach is the page's banner to say.
    func testReadingFromAHostOutOfReachLeavesItToTheBanner() async {
        let chat = ReviewChat()
        chat.answering = .unreachable
        let session = ReviewSession(source: chat)
        _ = await session.open()?.value
        XCTAssertNil(session.notice)
        chat.answering = .unavailable
        _ = await session.refresh()?.value
        XCTAssertNil(session.notice, "the page says why there is no review")
        chat.answering = .notConfirmed
        _ = await session.open()?.value
        XCTAssertEqual(
            session.notice,
            "The connection dropped before the agent’s computer answered. The page shows what it says next.")
    }

    func testRewordingDeletingDiscardingAndStartingOverChangeTheReview() async {
        let chat = ReviewChat()
        let session = ReviewSession(source: chat)
        XCTAssertNil(session.reword([1], as: "  "), "blank words change nothing")
        _ = await session.reword([1], as: " Sharper words. ")?.value
        _ = await session.delete([2])?.value
        _ = await session.discardDrafts()?.value
        _ = await session.startOver()?.value
        XCTAssertEqual(chat.edits, [
            [.reword(id: [1], text: "Sharper words.")], [.delete([2])], [.discardDrafts], [.startOver],
        ])
    }

    func testAClosedPageAsksTheHostNothingMore() async {
        let chat = ReviewChat()
        let session = ReviewSession(source: chat)
        session.apply(P.page([P.file("src/lib.rs"), P.line("src/lib.rs", 11)]))
        chat.anchors = [[1, 1]: P.anchor]
        session.select(1)
        session.close()
        session.close()
        XCTAssertEqual(chat.closes, 1)
        XCTAssertTrue(session.closed)
        XCTAssertNil(session.selected)
        XCTAssertFalse(session.canComment)
        XCTAssertNil(session.open())
        XCTAssertNil(session.discardDrafts())
        session.show(.all)
        session.apply(P.page([P.file("a.rs")], toFetch: ["a.rs"]))
        XCTAssertTrue(chat.choices.isEmpty)
        XCTAssertTrue(chat.fetches.isEmpty)
        XCTAssertTrue(chat.opens.isEmpty)
    }

    func testAnActOnAClosedChatSaysSo() async {
        let session = ReviewSession(source: ReviewChatClosing())
        _ = await session.startOver()?.value
        XCTAssertEqual(session.notice, "This chat is closed.")
    }

    func testTheReviewIsWordedTheSameOnEveryApp() {
        XCTAssertEqual(ChatWords.reviewChip(ReviewChip(comments: 3, moved: false)), "Review · 3 comments")
        XCTAssertEqual(ChatWords.reviewChip(ReviewChip(comments: 1, moved: true)), "Review · 1 comment")
        XCTAssertEqual(ChatWords.discardDrafts(1), "Discard 1 draft?")
        XCTAssertEqual(ChatWords.discardDrafts(4), "Discard 4 drafts?")
        XCTAssertEqual(ChatWords.reviewAgainst(P.header()), "Since main")
        var uncommitted = P.header()
        uncommitted.baseBranch = ""
        XCTAssertEqual(ChatWords.reviewAgainst(uncommitted), "Uncommitted")
        XCTAssertEqual(ChatWords.reviewAgainst(P.header(view: .sinceLast)), "Since your last review")
        XCTAssertEqual(ChatWords.reviewCounts(P.header()), "2 files · +3 \u{2212}1")
        var commented = P.header(drafts: 2)
        commented.sent = 1
        XCTAssertEqual(ChatWords.reviewCounts(commented), "2 files · +3 \u{2212}1 · 2 drafts · 1 sent")
        XCTAssertEqual(ChatWords.reviewCommentState(.sent(round: 2)), "Sent in round 2")
        XCTAssertEqual(ChatWords.reviewLines(start: 41, end: 41), "Line 41")
        XCTAssertEqual(ChatWords.reviewLines(start: 41, end: 42), "Lines 41–42")
        XCTAssertEqual(
            ChatWords.reviewGap(ReviewGap(path: "a.rs", loading: false, down: nil, hidden: 12, up: nil)),
            "12 hidden lines")
        XCTAssertEqual(
            ChatWords.reviewGap(ReviewGap(path: "a.rs", loading: false, down: nil, hidden: nil, up: nil)),
            "The rest of the file")
        let step = ReviewContextAsk(path: "a.rs", commit: P.snapshot, start: 5, end: 24, definition: nil)
        XCTAssertEqual(ChatWords.reviewMore(step, up: true), "20 lines up")
        var definition = step
        definition.definition = "fn open"
        XCTAssertEqual(ChatWords.reviewMore(definition, up: false), "Down to the end of fn open")
        XCTAssertEqual(ChatWords.reviewBannerAction(.changedSince), "Refresh")
        XCTAssertNil(ChatWords.reviewBannerAction(.mergeUnderway))
        XCTAssertNil(ChatWords.reviewNotice(.done))
    }
}

/// A chat closed under the page: every act on its review answers nothing.
private final class ReviewChatClosing: ChatSource, @unchecked Sendable {
    func openReview(baseBranch: String?, refresh: Bool) async -> ReviewOutcome? { nil }
    func editReview(_ changes: [ReviewChange]) async -> ReviewOutcome? { nil }
    func opening() -> ChatDelta {
        ChatDelta(
            chat: 0, rows: [], agents: nil, ask: nil, frame: nil, keys: .reload([]), overview: nil,
            review: nil, settings: nil)
    }
    func follow(_ following: Bool) {}
    func rows(for keys: [String]) -> [Row] { [] }
    func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> { .failure(RuntimeFailure("closed")) }
    func answer(_ ask: String, choice: Int, note: String?) async -> ActOutcome? { nil }
    func answer(_ ask: String, responses: [QuestionResponse]) async -> ActOutcome? { nil }
    func replyInstead(_ ask: String, text: String, soFar: [QuestionResponse]) async -> ActOutcome? { nil }
    func toggleRun(_ member: String) {}
    func toggleGroup(_ member: String) {}
    func foldTurns(_ on: Bool) {}
    func toggleTurn(_ end: String) {}
    func answerForm(_ ask: String, values: [FormValue]) async -> ActOutcome? { nil }
    func withdraw(_ input: [UInt8]) async -> ActOutcome? { nil }
    func draft(of input: [UInt8]) -> Draft? { nil }
    func sendNow(_ input: [UInt8]) async -> ActOutcome? { nil }
    func resend(_ input: [UInt8]) async -> SendOutcome? { nil }
    func discard(_ input: [UInt8]) {}
    func interrupt() async -> ActOutcome? { nil }
    func stopJob(_ key: String) async -> ActOutcome? { nil }
    func change(_ setting: SettingChange) async -> ActOutcome? { nil }
    func resume(with draft: Draft) async -> ActOutcome? { nil }
    func pageOlder(_ rows: UInt32) async -> PageOutcome? { nil }
    func putBlob(_ data: Data, name: String, mime: String) async -> Result<BlobRef, RuntimeFailure> {
        .failure(RuntimeFailure("closed"))
    }
    func blob(_ hash: [UInt8]) -> Data? { nil }
    func openOverview(_ comparison: Comparison) async -> Result<Overview, RuntimeFailure> {
        .failure(RuntimeFailure("closed"))
    }
}
