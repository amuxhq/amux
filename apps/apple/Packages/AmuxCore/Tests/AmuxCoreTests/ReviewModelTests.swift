import Foundation
import XCTest

@testable import AmuxCore

/// A three-file working tree: a modified file, another file of the same
/// name in another folder, and a new file. Four lines added, two removed.
enum ReviewFixtures {
    static let patch = """
        diff --git a/src/pairing/errors.rs b/src/pairing/errors.rs
        index 1111111..2222222 100644
        --- a/src/pairing/errors.rs
        +++ b/src/pairing/errors.rs
        @@ -10,4 +10,3 @@ pub enum PairError {
             Expired,
        -    Refused,
        -    Unknown,
        +    Refused(String),
         }
        diff --git a/tests/errors.rs b/tests/errors.rs
        index 3333333..4444444 100644
        --- a/tests/errors.rs
        +++ b/tests/errors.rs
        @@ -1,2 +1,3 @@
         use pairing::PairError;
        +use pairing::describe;
         fn main() {}
        diff --git a/docs/PAIRING.md b/docs/PAIRING.md
        new file mode 100644
        index 0000000..5555555
        --- /dev/null
        +++ b/docs/PAIRING.md
        @@ -0,0 +1,2 @@
        +# Pairing
        +Errors now carry the reason.

        """

    static let frozen = FrozenReview(
        diff: Diff(
            head: "4f2a9c1", files: [], base: DiffBase(base: .head(Empty())), mergeBase: nil,
            patch: BlobRef(hash: [7, 7, 7], name: "patch", mime: "text/x-diff", size: UInt64(patch.utf8.count))),
        patch: patch)
}

@MainActor
final class ReviewModelTests: XCTestCase {
    func testTheDocumentIsTheRuntimesParseInPatchOrder() {
        let model = ReviewModel(review: ReviewFixtures.frozen)
        XCTAssertEqual(model.doc.files.map(\.path), ["src/pairing/errors.rs", "tests/errors.rs", "docs/PAIRING.md"])
        XCTAssertEqual(model.doc.files.map(\.added), [1, 1, 2])
        XCTAssertEqual(model.doc.files.map(\.removed), [2, 0, 0])
        XCTAssertEqual(model.doc.files[2].status, .added)
        XCTAssertEqual(model.doc.files[0].hunks[0].lines.map(\.kind), [.context, .removed, .removed, .added, .context])
    }

    func testAFileSharingItsNameWithAnotherIsLabelledWithItsFolder() {
        let model = ReviewModel(review: ReviewFixtures.frozen)
        XCTAssertEqual((0..<3).map(model.label(ofFile:)), ["pairing/errors.rs", "tests/errors.rs", "PAIRING.md"])
    }

    func testASelectionStaysInItsFileAndACommentGoesOnTheLineItEndsOn() {
        let model = ReviewModel(review: ReviewFixtures.frozen)
        model.begin(at: ReviewLine(file: 0, hunk: 0, line: 0))
        model.extend(to: ReviewLine(file: 1, hunk: 0, line: 1))
        XCTAssertEqual(model.selection?.upperBound, ReviewLine(file: 0, hunk: 0, line: 0), "another file is not reached")
        model.extend(to: ReviewLine(file: 0, hunk: 0, line: 3))
        XCTAssertTrue(model.isSelected(ReviewLine(file: 0, hunk: 0, line: 2)))
        XCTAssertFalse(model.canAttach, "nothing written yet")

        XCTAssertFalse(model.comment("   "), "a blank comment adds nothing")
        XCTAssertTrue(model.comment("Name the reason."))
        XCTAssertNil(model.selection, "adding a comment ends the selection")
        XCTAssertEqual(model.comments, [ReviewComment(path: "src/pairing/errors.rs", line: 11, oldLine: 0, text: "Name the reason.")])
        XCTAssertEqual(model.doc.files[0].hunks[0].lines[3].comments, ["Name the reason."], "placed under its line by the runtime")
        XCTAssertTrue(model.canAttach)
    }

    func testACommentOnARemovedLineNamesItsOldLine() {
        let model = ReviewModel(review: ReviewFixtures.frozen)
        // Dragging upward: the selection ends on the lower line all the same.
        model.begin(at: ReviewLine(file: 0, hunk: 0, line: 2))
        model.extend(to: ReviewLine(file: 0, hunk: 0, line: 1))
        model.comment("Unknown was never raised.")
        XCTAssertEqual(model.comments.first?.oldLine, 12)
        XCTAssertEqual(model.comments.first?.line, 0)
        XCTAssertEqual(model.doc.files[0].hunks[0].lines[2].comments, ["Unknown was never raised."])
        XCTAssertEqual(model.count(inFile: 0), 1)
        XCTAssertEqual(model.count(inFile: 1), 0)
    }

    func testAFileFoldsAndOpensAgain() {
        let model = ReviewModel(review: ReviewFixtures.frozen)
        model.toggle(file: 1)
        XCTAssertEqual(model.folded, [1])
        model.toggle(file: 1)
        XCTAssertEqual(model.folded, [])
    }

    func testACommentIsFoundWhereItIsDrawnAndCanBeRewrittenOrRemoved() {
        let model = ReviewModel(review: ReviewFixtures.frozen)
        model.begin(at: ReviewLine(file: 0, hunk: 0, line: 3))
        model.comment("Name the reason.")
        model.begin(at: ReviewLine(file: 0, hunk: 0, line: 1))
        model.comment("Refused takes a reason now.")
        model.begin(at: ReviewLine(file: 2, hunk: 0, line: 0))
        model.comment("Link the table.")
        XCTAssertEqual(model.placed[CommentPlace(file: 0, line: ReviewLine(file: 0, hunk: 0, line: 3))], [0])
        XCTAssertEqual(model.placed[CommentPlace(file: 0, line: ReviewLine(file: 0, hunk: 0, line: 1))], [1])
        XCTAssertEqual(model.placed[CommentPlace(file: 2, line: ReviewLine(file: 2, hunk: 0, line: 0))], [2])

        XCTAssertFalse(model.rewrite(1, as: "  "), "a blank rewrite changes nothing")
        XCTAssertTrue(model.rewrite(1, as: "Refused carries why."))
        XCTAssertEqual(model.doc.files[0].hunks[0].lines[1].comments, ["Refused carries why."])

        model.remove(0)
        XCTAssertEqual(model.comments.map(\.text), ["Refused carries why.", "Link the table."])
        XCTAssertEqual(model.doc.files[0].hunks[0].lines[3].comments, [])
        XCTAssertEqual(
            model.placed[CommentPlace(file: 2, line: ReviewLine(file: 2, hunk: 0, line: 0))], [1],
            "the indexes follow the comments that are left")
    }
}
