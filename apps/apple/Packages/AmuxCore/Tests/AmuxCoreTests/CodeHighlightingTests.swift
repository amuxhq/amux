import AmuxValues
import Foundation
import Observation
import Synchronization
import XCTest
@testable import AmuxCore
@testable import AmuxWords

/// Code is coloured by the shared view through the bridge, drawn as it is
/// while unsettled and asked for again shortly, and kept once settled so a
/// block scrolled back into view does not cross the bridge again.
private func piece(_ text: String, _ kind: TokenKind) -> CodeRun {
    CodeRun(text: text, kind: kind)
}

@MainActor
final class CodeHighlightingTests: XCTestCase {
    /// A stand-in that answers unsettled until it has been asked `settleAt`
    /// times, and counts what it was asked.
    private final class Scripted: Sendable {
        let asked = Mutex(0)
        let settleAt: Int

        init(settleAt: Int) { self.settleAt = settleAt }

        var highlighting: CodeHighlighting {
            CodeHighlighting { request in
                let count = self.asked.withLock { asked in
                    asked += 1
                    return asked
                }
                guard case .block(let code, _, _) = request else { return nil }
                let settled = count >= self.settleAt
                return HighlightedCode(
                    lines: [[piece(code, settled ? .keyword : .plain)]], settled: settled)
            }
        }

        var count: Int { asked.withLock { $0 } }
    }

    func testAnUnsettledAnswerIsDrawnAndAskedForAgainUntilItSettles() async {
        let scripted = Scripted(settleAt: 3)
        let highlighting = scripted.highlighting
        let request = CodeHighlighting.Request.block(code: "fn", info: "rust", streaming: false)
        var drawn: [HighlightedCode?] = []
        await highlighting.follow(request) { drawn.append($0) }
        XCTAssertEqual(drawn.map { $0?.settled }, [false, false, true])
        XCTAssertEqual(drawn.last??.lines, [[piece("fn", .keyword)]])
        XCTAssertEqual(highlighting.kept(request)?.settled, true)
    }

    /// A kept answer is handed over at once without crossing again, so a
    /// view holds it too and still draws it once the keeping is let go.
    func testAKeptAnswerIsHandedWithoutAskingAgain() async {
        let scripted = Scripted(settleAt: 1)
        let highlighting = scripted.highlighting
        let request = CodeHighlighting.Request.block(code: "fn", info: "rust", streaming: false)
        XCTAssertNil(highlighting.kept(request))
        await highlighting.follow(request) { _ in }
        var drawn: [HighlightedCode?] = []
        await highlighting.follow(request) { drawn.append($0) }
        XCTAssertEqual(scripted.count, 1, "a kept answer crossed again")
        XCTAssertEqual(drawn.map { $0?.lines }, [[[piece("fn", .keyword)]]])
    }

    /// Two views show the same code. The first draws it plain, before any
    /// answer; the second's answer settles. The first is drawn again,
    /// because what it read is observed, whether or not its own task has
    /// run yet: nothing it holds has to catch the update.
    func testAViewThatDrewPlainIsDrawnAgainWhenAnotherSettlesItsCode() async {
        let highlighting = Scripted(settleAt: 1).highlighting
        let request = CodeHighlighting.Request.block(code: "fn", info: "rust", streaming: false)
        let redrawn = Mutex(false)
        let first = withObservationTracking {
            highlighting.kept(request)
        } onChange: {
            redrawn.withLock { $0 = true }
        }
        XCTAssertNil(first, "the first view drew before anything was kept")
        await highlighting.follow(request) { _ in }
        XCTAssertTrue(redrawn.withLock { $0 }, "the first view was not told its code settled")
        XCTAssertEqual(highlighting.kept(request)?.lines, [[piece("fn", .keyword)]])
    }

    func testAnUnsettledAnswerIsNotKept() async {
        let scripted = Scripted(settleAt: 2)
        let highlighting = scripted.highlighting
        let request = CodeHighlighting.Request.block(code: "fn", info: "rust", streaming: true)
        // Stopped after its first answer, as a view's task is when the
        // streaming block grows.
        let task = Task { @MainActor in
            await highlighting.follow(request) { _ in withUnsafeCurrentTask { $0?.cancel() } }
        }
        await task.value
        XCTAssertEqual(scripted.count, 1)
        XCTAssertNil(highlighting.kept(request))
    }

    /// Nothing able to colour the code hands nil, so a view lets go of an
    /// answer for an earlier text rather than draw it on.
    func testNoAnswerIsHandedAsNil() async {
        let nothing = CodeHighlighting { _ in nil }
        let request = CodeHighlighting.Request.block(code: "fn", info: "rust", streaming: false)
        var drawn: [HighlightedCode?] = []
        await nothing.follow(request) { drawn.append($0) }
        XCTAssertEqual(drawn.count, 1)
        XCTAssertNil(drawn.first ?? nil)
    }

    func testPlainAnswersNothingAndEachHighlightingIsItsOwn() async {
        let request = CodeHighlighting.Request.block(code: "fn", info: "rust", streaming: false)
        XCTAssertNil(CodeHighlighting.plain.kept(request))
        var drawn = 0
        await CodeHighlighting.plain.follow(request) { _ in drawn += 1 }
        XCTAssertEqual(drawn, 0)
        XCTAssertEqual(CodeHighlighting.bridge, CodeHighlighting.bridge)
        XCTAssertNotEqual(CodeHighlighting.bridge, CodeHighlighting.plain)
        XCTAssertNotEqual(Scripted(settleAt: 1).highlighting, Scripted(settleAt: 1).highlighting)
    }

    /// While the answer for a streaming block's newest text is on its way,
    /// the last answer goes on colouring the lines it still knows, so the
    /// block does not flash plain on every arrival.
    func testAnEarlierAnswerColoursTheLinesItStillKnows() {
        let earlier = HighlightedCode(
            lines: [[piece("let", .keyword), piece(" a = 1", .plain)], [piece("le", .plain)]],
            settled: true)
        XCTAssertEqual(earlier.runs(["let a = 1", "let b = 2", ""]), [
            [piece("let", .keyword), piece(" a = 1", .plain)],
            [piece("let b = 2", .plain)],
            [],
        ])
        // A line that no longer reads as it did is drawn plain, not in
        // another text's colours.
        XCTAssertEqual(earlier.runs(["var a = 1"]), [[piece("var a = 1", .plain)]])
        XCTAssertEqual(HighlightedCode.plain(["x", ""]), [[piece("x", .plain)], []])
    }

    func testCodeIsTextInItsKindsColours() {
        let text = CodeText.attributed(
            lines: [[piece("fn", .keyword), piece(" main", .plain)], []]
        ) { kind in kind == .keyword ? .red : nil }
        XCTAssertEqual(String(text.characters), "fn main\n")
        let keyword = text.runs.first { String(text[$0.range].characters) == "fn" }
        XCTAssertEqual(keyword?.foregroundColor, .red)
        let plain = text.runs.first { String(text[$0.range].characters) == " main" }
        XCTAssertNil(plain?.foregroundColor, "a kind given no colour takes the text's own")
    }

    /// A block from an agent writing Windows line endings is coloured line
    /// for line: the lines drawn are split by the same rule as the lines
    /// the shared view answered, so every one reads as its answer.
    func testABlockWithWindowsLineEndingsIsColouredThroughTheBridge() async throws {
        let source = "```rust\r\nfn a() {}\r\nfn b() {}\r\n```\r\n"
        guard case .code(let language, let text, _) = try XCTUnwrap(MarkdownDocument.parse(source).blocks.first)
        else { return XCTFail("expected code") }
        let request = CodeHighlighting.Request.block(code: text, info: language ?? "", streaming: false)
        var drawn: HighlightedCode?
        await CodeHighlighting.bridge.follow(request) { drawn = $0 }
        let runs = try XCTUnwrap(drawn).runs(CodeText.lines(text))
        XCTAssertEqual(runs.count, 2)
        for line in runs {
            XCTAssertEqual(line.first, piece("fn", .keyword), "a line drew plain: \(runs)")
        }
    }

    /// Through the real bridge: the shared view's answer decodes into the
    /// generated values, plain while the grammar builds and coloured once
    /// it has, for a fenced block and for an edit card's patch.
    func testTheBridgeColoursABlockAndAnEdit() async {
        let block = CodeHighlighting.Request.block(
            code: "fn main() {}\n", info: "rust", streaming: false)
        var drawn: HighlightedCode?
        await CodeHighlighting.bridge.follow(block) { drawn = $0 }
        let code = drawn ?? CodeHighlighting.bridge.kept(block)
        XCTAssertEqual(code?.settled, true)
        XCTAssertEqual(code?.lines.first?.first, piece("fn", .keyword))

        let edit = CodeHighlighting.Request.edit(path: "src/pair.py", lines: [
            .hunk(oldStart: 3, newStart: 3),
            .line(PatchLine(kind: .added, text: "n = 1", number: 3)),
        ])
        var patch: HighlightedCode?
        await CodeHighlighting.bridge.follow(edit) { patch = $0 }
        let lines = (patch ?? CodeHighlighting.bridge.kept(edit))?.lines
        XCTAssertEqual(lines?.count, 2)
        XCTAssertEqual(lines?.first, [], "a hunk's start is drawn in the client's own words")
        XCTAssertTrue(lines?.last?.contains(piece("1", .number)) ?? false, "\(String(describing: lines))")
    }
}
