import AmuxCore
import AmuxValues
import Foundation
import XCTest

/// The tool window draws the agent's review page: the review itself
/// wherever its host offers reviews, and a page saying so where the host
/// offers none, whether or not anything of its review was read.
@MainActor
final class ReviewPageChoice: XCTestCase {
    func testAHostWithoutReviewsHasThePageSaySo() {
        let (chat, model) = opened()
        model.apply(delta(review: Self.notOffered))
        XCTAssertEqual(chat.reviewPage?.page.banners, [.notOffered])
        XCTAssertNil(chat.reviewPage?.page.header, "nothing of the review to draw")
    }

    func testAHostWithReviewsGetsTheAgentsPage() {
        let (chat, model) = opened()
        XCTAssertEqual(chat.reviewPage?.page.opening, true, "the page is drawn opening while the host is asked")
        model.apply(delta(review: Self.summarised(banners: [])))
        XCTAssertEqual(chat.reviewPage?.page.header?.snapshot, "5e1d0c9")
    }

    func testAHostThatStopsOfferingReviewsHasThePageSaySo() {
        let (chat, model) = opened()
        model.apply(delta(review: Self.summarised(banners: [.notOffered])))
        XCTAssertEqual(chat.reviewPage?.page.banners, [.notOffered])
    }

    // MARK: - Fixtures

    private func opened() -> (MacChat, ChatModel) {
        let model = ChatModel(source: Silent(), loadingHintAfter: .zero)
        let chat = MacChat(model: model, host: "this Mac")
        chat.openReviewPage()
        return (chat, model)
    }

    private func delta(review: ReviewPage) -> ChatDelta {
        ChatDelta(
            chat: 1, rows: [], agents: nil, ask: nil, frame: nil, keys: nil, overview: nil, review: review,
            settings: nil)
    }

    private static let notOffered = ReviewPage(
        banners: [.notOffered], rows: [], toFetch: [], opening: false, empty: nil, header: nil)

    /// A review read from the host, saying `banners` above it.
    private static func summarised(banners: [ReviewBanner]) -> ReviewPage {
        ReviewPage(
            banners: banners, rows: [], toFetch: [], opening: false, empty: .nothing,
            header: ReviewHeader(
                baseBranch: "main", baseCommit: "a41f2b7", snapshot: "5e1d0c9", view: .all,
                comparedWith: "a41f2b7", comparedBase: "", views: [ReviewViewTab(view: .all, files: 0)], rounds: 0, files: 0,
                added: 0, removed: 0, drafts: 0, sent: 0, discardDrafts: false, startOver: true))
    }
}

/// A chat that holds nothing and answers nothing: the page reaches the
/// model only through the deltas a test hands it.
private final class Silent: ChatSource {
    func opening() -> ChatDelta {
        ChatDelta(
            chat: 1, rows: [], agents: nil, ask: nil, frame: nil, keys: .reload([]), overview: nil, review: nil,
            settings: nil)
    }
    func follow(_ following: Bool) {}
    func rows(for keys: [String]) -> [Row] { [] }
    func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> { .failure(RuntimeFailure("silent")) }
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
        .failure(RuntimeFailure("silent"))
    }
    func blob(_ hash: [UInt8]) -> Data? { nil }
    func openOverview(_ comparison: Comparison) async -> Result<Overview, RuntimeFailure> {
        .failure(RuntimeFailure("silent"))
    }
}
