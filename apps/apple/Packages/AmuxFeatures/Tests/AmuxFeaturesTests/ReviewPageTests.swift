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

    /// A drawn selection follows the finger through the lines of the file it
    /// began in: over another file's lines it holds at its file's last or
    /// first line, and over a comment between lines it holds where it was.
    func testADrawnSelectionStaysInTheFileItBeganIn() {
        let rows = [head("a.rs"), note("a1"), note("a2"), note("a3"), head("b.rs"), note("b1")]
        let file = SelectionDrag.file(of: 2, in: rows)
        XCTAssertEqual(file, 0..<4)
        XCTAssertEqual(SelectionDrag.file(of: 5, in: rows), 4..<6)
        // Lines 1 and 3 drawn with a comment between them; line 5 is the
        // next file's.
        let frames: [Int: CGRect] = [
            1: CGRect(x: 0, y: 100, width: 300, height: 20),
            3: CGRect(x: 0, y: 160, width: 300, height: 20),
            5: CGRect(x: 0, y: 240, width: 300, height: 20),
        ]
        XCTAssertEqual(SelectionDrag.line(at: 110, in: frames, within: nil), 1)
        XCTAssertEqual(SelectionDrag.line(at: 250, in: frames, within: nil), 5)
        XCTAssertEqual(SelectionDrag.line(at: 130, in: frames, within: nil), nil)
        XCTAssertEqual(SelectionDrag.line(at: 170, in: frames, within: file), 3)
        XCTAssertEqual(SelectionDrag.line(at: 130, in: frames, within: file), nil)
        XCTAssertEqual(SelectionDrag.line(at: 250, in: frames, within: file), 3)
        XCTAssertEqual(SelectionDrag.line(at: 40, in: frames, within: file), 1)
    }

    /// The list runs under a finger only in the bands at its edges, up at
    /// the top and down at the bottom, faster the deeper in, and at its
    /// fastest past the edge.
    func testTheListRunsUnderAFingerAtItsEdges() {
        let top: CGFloat = 100, bottom: CGFloat = 700
        XCTAssertEqual(SelectionDrag.speed(at: 400, top: top, bottom: bottom), 0)
        XCTAssertEqual(SelectionDrag.speed(at: top + SelectionDrag.edge, top: top, bottom: bottom), 0)
        let entering = SelectionDrag.speed(at: bottom - SelectionDrag.edge + 1, top: top, bottom: bottom)
        let deep = SelectionDrag.speed(at: bottom - 5, top: top, bottom: bottom)
        XCTAssertGreaterThan(entering, 0)
        XCTAssertGreaterThan(deep, entering)
        XCTAssertEqual(SelectionDrag.speed(at: bottom + 80, top: top, bottom: bottom), SelectionDrag.fastest)
        XCTAssertLessThan(SelectionDrag.speed(at: top + 10, top: top, bottom: bottom), 0)
        XCTAssertEqual(SelectionDrag.speed(at: top - 80, top: top, bottom: bottom), -SelectionDrag.fastest)
        // A list too short for two bands never runs.
        XCTAssertEqual(SelectionDrag.speed(at: 10, top: 0, bottom: 100), 0)
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
