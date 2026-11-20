import AmuxCore
import AmuxWords
import XCTest
@testable import AmuxFeatures

/// The phone's review page draws the shared page's rows as handed; what it
/// decides itself is which head each row is pinned under, what its file
/// menu says of a file, and the composer's one button beside the review's
/// chip.
@MainActor
final class ReviewPageTests: XCTestCase {
    private func head(_ path: String, added: UInt32 = 0, removed: UInt32 = 0, comments: UInt32 = 0) -> ReviewRow {
        ReviewRow(key: "f/\(path)", of: .file(ReviewFileHead(
            path: path, status: .modified, added: added, removed: removed, binary: false, language: "",
            flags: [], folded: false, changedSinceLast: false, inChange: true, comments: comments,
            outline: [], oldPath: nil)))
    }

    private func note(_ key: String) -> ReviewRow {
        ReviewRow(key: key, of: .loading(path: key))
    }

    /// Every row is pinned under the head of the file it follows, in page
    /// order, and a head with nothing under it, folded, still stands.
    func testRowsArePinnedUnderTheirFilesHeads() {
        let rows = [note("before"), head("a.rs"), note("a1"), note("a2"), head("b.rs"), head("c.rs"), note("c1")]
        let sections = ReviewSections(rows).all
        XCTAssertEqual(sections.map(\.head), [nil, 1, 4, 5])
        XCTAssertEqual(sections.map(\.rows), [[0], [2, 3], [], [6]])
        XCTAssertEqual(sections.map(\.id), ["above", "f/a.rs", "f/b.rs", "f/c.rs"])
        XCTAssertTrue(ReviewSections([]).all.isEmpty)
    }

    /// The file menu says what a file changed and its comments, leaving out
    /// what is nothing.
    func testTheFileMenuSaysWhatAFileChanged() {
        func summary(_ row: ReviewRow) -> String {
            guard case .file(let head) = row.of else { return "" }
            return ReviewWords.fileSummary(head)
        }
        XCTAssertEqual(summary(head("a.rs", added: 3, removed: 4, comments: 1)), "+3 \u{2212}4 · 1 comment")
        XCTAssertEqual(summary(head("a.rs", added: 2)), "+2")
        XCTAssertEqual(summary(head("a.rs", comments: 2)), "2 comments")
        XCTAssertEqual(summary(head("a.rs")), "")
    }

    /// The review's chip goes with or without words, so while the agent
    /// works a composer holding only the chip queues it rather than offering
    /// to interrupt.
    func testTheChipAloneSendsRatherThanInterrupts() {
        XCTAssertEqual(ComposerBox.primary(resuming: false, hasMessage: true, working: true), .send)
        XCTAssertEqual(ComposerBox.primary(resuming: false, hasMessage: false, working: true), .interrupt)
        XCTAssertEqual(ComposerBox.primary(resuming: false, hasMessage: false, working: false), .send)
        XCTAssertEqual(ComposerBox.primary(resuming: true, hasMessage: true, working: true), .resume)
    }

    /// A sent review names its round for good by its snapshot and exactly
    /// the comments it sent, so two sends on one snapshot are two rounds.
    func testASentReviewNamesItsRoundByItsSnapshotAndComments() {
        let one = SentRound(sent: SentReviewRef(reviewId: [1], round: 1, snapshot: "abc", commentIds: [[1], [2]]), text: "")
        let same = SentRound(sent: SentReviewRef(reviewId: [1], round: 3, snapshot: "abc", commentIds: [[1], [2]]), text: "x")
        let next = SentRound(sent: SentReviewRef(reviewId: [1], round: 2, snapshot: "abc", commentIds: [[3]]), text: "")
        XCTAssertEqual(one.id, same.id)
        XCTAssertNotEqual(one.id, next.id)
    }
}
