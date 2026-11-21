import AmuxValues
import SwiftUI
import XCTest

@testable import AmuxMacScreens

/// Dragging across the review's lines picks them within the file the drag
/// began in, and runs the list under a pointer held at its top or bottom
/// edge. The pointer itself is never driven here: these hold the geometry
/// the drag reads.
@MainActor
final class ReviewDrag: XCTestCase {
    /// Two files: a head and three lines each, the second file's lines
    /// below the first's with a comment between its first two.
    private let rows: [ReviewRow] = [
        head("a.rs"), line("a.rs", 1), line("a.rs", 2), line("a.rs", 3),
        head("b.rs"), line("b.rs", 1), comment(), line("b.rs", 2), line("b.rs", 3),
    ]

    private func drawn() -> Drawing {
        let drawing = Drawing()
        drawing.places = Dictionary(uniqueKeysWithValues: rows.enumerated().map { ($1.key, $0) })
        // Each line 20 points tall, a head 30 and the comment 60.
        let tops: [Int: CGFloat] = [1: 30, 2: 50, 3: 70, 5: 120, 7: 200, 8: 220]
        for (index, top) in tops {
            drawing.frames[rows[index].key] = CGRect(x: 0, y: top, width: 400, height: 20)
        }
        return drawing
    }

    func testAPressLandsOnTheLineUnderIt() {
        let drawing = drawn()
        XCTAssertEqual(drawing.line(at: 55, within: nil), 2)
        XCTAssertNil(drawing.line(at: 10, within: nil), "a press on a head picks nothing")
        XCTAssertNil(drawing.line(at: 160, within: nil), "nor one on a comment")
    }

    func testTheRowsOfAFileRunFromItsHeadToTheNext() {
        XCTAssertEqual(FileSection.range(of: 2, in: rows), 0..<4)
        XCTAssertEqual(FileSection.range(of: 7, in: rows), 4..<9)
    }

    func testADragHoldsWithinItsFile() {
        let drawing = drawn()
        let file = FileSection.range(of: 2, in: rows)
        XCTAssertEqual(drawing.line(at: 75, within: file), 3)
        XCTAssertEqual(drawing.line(at: 210, within: file), 3, "past the file's last line it holds at that line")
        XCTAssertEqual(drawing.line(at: 0, within: file), 1, "above the file's first line it holds at that one")
    }

    func testOverACommentADragHoldsWhereItWas() {
        let drawing = drawn()
        XCTAssertNil(drawing.line(at: 160, within: FileSection.range(of: 7, in: rows)))
    }

    func testTheListRunsOnlyWithThePointerAtAnEdge() {
        let drawing = drawn()
        drawing.scroll = ScrollGeometry(
            contentOffset: .zero, contentSize: CGSize(width: 400, height: 2000), contentInsets: EdgeInsets(),
            containerSize: CGSize(width: 400, height: 500))
        drawing.file = 0..<4
        drawing.pointer = 250
        XCTAssertEqual(drawing.edgeSpeed(), 0, "in the middle it holds still")
        drawing.pointer = 490
        XCTAssertGreaterThan(drawing.edgeSpeed(), 0, "at the bottom it runs down")
        drawing.pointer = 5
        XCTAssertLessThan(drawing.edgeSpeed(), 0, "at the top it runs up")
        drawing.pointer = -200
        XCTAssertEqual(drawing.edgeSpeed(), -Drawing.fastest, "past the edge it runs its fastest")
        drawing.file = nil
        XCTAssertEqual(drawing.edgeSpeed(), 0, "with nothing being picked it never runs")
    }

    // MARK: - Rows

    private static func head(_ path: String) -> ReviewRow {
        ReviewRow(
            key: "f/\(path)",
            of: .file(ReviewFileHead(
                path: path, status: .modified, added: 3, removed: 0, binary: false, language: "rust", flags: [],
                folded: false, changedSinceLast: false, inChange: true, comments: 0, outline: [], oldPath: nil)))
    }

    private static func line(_ path: String, _ number: UInt32) -> ReviewRow {
        ReviewRow(
            key: "l/\(path)/\(number)",
            of: .line(ReviewCodeLine(
                path: path, kind: .added, text: "let x = \(number);", runs: [], commented: false, extra: false,
                newLine: number, oldLine: nil)))
    }

    private static func comment() -> ReviewRow {
        ReviewRow(
            key: "c/one",
            of: .comment(ReviewCommentRow(
                id: [1], path: "b.rs", text: "Why?", state: .draft, at: .lines(start: 1, end: 1),
                changedNear: false, scope: [], snippet: [], folded: false)))
    }

    private func head(_ path: String) -> ReviewRow { Self.head(path) }
    private func line(_ path: String, _ number: UInt32) -> ReviewRow { Self.line(path, number) }
    private func comment() -> ReviewRow { Self.comment() }
}
