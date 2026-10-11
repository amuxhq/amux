import Foundation
import XCTest
@testable import AmuxCore

/// Agents write markdown, so structure the agent chose has to survive the trip
/// to the screen. Each case here is a construct that reads as something else
/// entirely when it is flattened.
final class MarkdownTests: XCTestCase {
    private func blocks(_ source: String) -> [MarkdownBlock] {
        MarkdownDocument.parse(source).blocks
    }

    func testHeadingsKeepTheirLevel() {
        guard case .heading(let level, let text) = blocks("## Why it broke")[0] else {
            return XCTFail("expected a heading")
        }
        XCTAssertEqual(level, 2)
        XCTAssertEqual(String(text.characters), "Why it broke")
    }

    func testFencedCodeKeepsItsLinesAndItsLanguage() {
        guard case .code(let language, let text) = blocks("""
            ```rust
            let x = 1;
                let y = 2;
            ```
            """)[0] else { return XCTFail("expected code") }
        XCTAssertEqual(language, "rust")
        XCTAssertEqual(text, "let x = 1;\n    let y = 2;")
    }

    func testAFenceWithNoLanguageSaysSoRatherThanGuessing() {
        guard case .code(let language, _) = blocks("```\nplain\n```")[0] else {
            return XCTFail("expected code")
        }
        XCTAssertNil(language)
    }

    func testListsKeepTheirMarkersAndTheirDepth() {
        guard case .list(let ordered, let items) = blocks("""
            - one
            - two
              - nested
            """)[0] else { return XCTFail("expected a list") }
        XCTAssertFalse(ordered)
        XCTAssertEqual(items.map(\.depth), [0, 0, 1])
        XCTAssertEqual(items.map { String($0.text.characters) }, ["one", "two", "nested"])
    }

    func testANumberedListIsNumberedRatherThanBulleted() {
        guard case .list(let ordered, let items) = blocks("1. first\n2. second")[0] else {
            return XCTFail("expected a list")
        }
        XCTAssertTrue(ordered)
        XCTAssertEqual(items.map(\.marker), ["1.", "2."])
    }

    /// A table's second row is its alignment rule, which is punctuation. Left
    /// in, it renders as a row of dashes pretending to be data.
    func testATablesAlignmentRuleIsNotARow() {
        guard case .table(let header, let rows) = blocks("""
            | Layer | Reads |
            | --- | ---: |
            | claude | yes |
            | codex | no |
            """)[0] else { return XCTFail("expected a table") }
        XCTAssertEqual(header.map { String($0.characters) }, ["Layer", "Reads"])
        XCTAssertEqual(rows.count, 2)
        XCTAssertEqual(rows[0].map { String($0.characters) }, ["claude", "yes"])
    }

    func testQuotesKeepTheirLinesAndDropTheirMarker() {
        guard case .quote(let lines) = blocks("> first\n> second")[0] else {
            return XCTFail("expected a quote")
        }
        XCTAssertEqual(lines.map { String($0.characters) }, ["first", "second"])
    }

    func testLinesWithNoBlankLineBetweenThemAreOneParagraph() {
        let parsed = blocks("one line\nand its continuation\n\na second paragraph")
        XCTAssertEqual(parsed.count, 2)
        guard case .paragraph(let first) = parsed[0] else { return XCTFail("expected prose") }
        XCTAssertEqual(String(first.characters), "one line\nand its continuation")
    }

    func testEmphasisAndCodeSpansResolveRatherThanShowingTheirPunctuation() {
        guard case .paragraph(let text) = blocks("run `cargo test` **now**")[0] else {
            return XCTFail("expected prose")
        }
        XCTAssertEqual(String(text.characters), "run cargo test now")
    }

    func testALinkKeepsItsTextAndItsDestination() {
        guard case .paragraph(let text) = blocks("see [the spec](https://example.com)")[0] else {
            return XCTFail("expected prose")
        }
        XCTAssertEqual(String(text.characters), "see the spec")
        XCTAssertEqual(
            text.runs.compactMap(\.link?.absoluteString), ["https://example.com"])
        // Underlined rather than coloured: the one accent means "waiting for
        // you", and the system's blue is not a colour this design owns.
        XCTAssertEqual(text.runs.compactMap(\.underlineStyle), [.single])
    }

    /// A stray asterisk must not blank the sentence around it.
    func testMalformedInlineMarkupFallsBackToTheLiteralText() {
        guard case .paragraph(let text) = blocks("a * b")[0] else {
            return XCTFail("expected prose")
        }
        XCTAssertEqual(String(text.characters), "a * b")
    }

    func testAHorizontalRuleIsARuleAndNotAList() {
        XCTAssertEqual(blocks("---").count, 1)
        guard case .rule = blocks("---")[0] else { return XCTFail("expected a rule") }
    }

    func testPlainProseIsRecognisedSoItCanBeDrawnCheaply() {
        XCTAssertTrue(MarkdownDocument.parse("just a sentence").isPlainParagraph)
        XCTAssertFalse(MarkdownDocument.parse("# heading\n\ntext").isPlainParagraph)
    }

    // Every line the parser meets must be consumed by some block. A line
    // that starts like a heading or a table but is neither is still prose;
    // the chat measures rows on the main thread, so a parse that never
    // returns freezes the phone.

    /// A plan streams in a few characters at a time, so its next heading is
    /// at first only its marks: `##` with no title yet.
    func testAPlanStreamingInIsParsedWhileItsNextHeadingIsHalfWritten() {
        let parsed = blocksWithin("## Summary\n\nCreate `README.md` with `UPDATED`.\n\n##")
        guard case .heading(let level, let title) = parsed.first else {
            return XCTFail("expected the finished heading first, got \(parsed)")
        }
        XCTAssertEqual(level, 2)
        XCTAssertEqual(String(title.characters), "Summary")
        XCTAssertTrue(parsed.contains {
            if case .paragraph(let text) = $0 {
                String(text.characters) == "Create README.md with UPDATED."
            } else { false }
        })
    }

    /// `#42` or `#!/bin/sh` at the start of a line is not a heading: a
    /// heading's marks are followed by a space.
    func testAHashWithNoSpaceAfterItStartsAParagraphNotAHeading() {
        guard case .paragraph(let text) = blocksWithin("#42 is fixed").first else {
            return XCTFail("expected prose")
        }
        XCTAssertEqual(String(text.characters), "#42 is fixed")
    }

    /// A pipe with no second pipe after it is not a table row.
    func testAPipeLineThatIsNotATableIsAParagraph() {
        guard case .paragraph(let text) = blocksWithin("| one cell").first else {
            return XCTFail("expected prose")
        }
        XCTAssertEqual(String(text.characters), "| one cell")
    }

    /// An agent's single newline is a line break, as Claude's and Codex's
    /// own interfaces draw it: "one per line" answers, lists written as
    /// lines and log excerpts keep their lines. A blank line still parts
    /// two paragraphs.
    func testAnAgentsSingleLineBreaksStayLineBreaks() {
        let parsed = blocksWithin("alpha\nbeta\ngamma")
        XCTAssertEqual(parsed.count, 1, "three lines with no blank line are one paragraph")
        guard case .paragraph(let text) = parsed.first else { return XCTFail("expected prose") }
        XCTAssertEqual(
            String(text.characters).split(separator: "\n").map(String.init),
            ["alpha", "beta", "gamma"],
            "the lines run together: \(String(text.characters).debugDescription)")

        let paragraphs = blocksWithin("alpha\n\nbeta")
        XCTAssertEqual(paragraphs.count, 2, "a blank line parts two paragraphs")
    }

    /// Parses on a thread of its own and gives it two seconds. A parse that
    /// does not return cannot be stopped from outside and allocates without
    /// bound, so rather than let it take the machine's memory while the
    /// remaining tests run, the process ends here; xcodebuild reports the
    /// test as crashed with this message and runs the rest in a new process.
    private func blocksWithin(_ source: String) -> [MarkdownBlock] {
        final class Parsed: @unchecked Sendable { var blocks: [MarkdownBlock] = [] }
        let parsed = Parsed()
        let done = DispatchSemaphore(value: 0)
        Thread.detachNewThread {
            parsed.blocks = MarkdownDocument.parse(source).blocks
            done.signal()
        }
        if done.wait(timeout: .now() + 2) == .timedOut {
            fatalError(
                "MarkdownDocument.parse(\(source.debugDescription)) did not return within 2 seconds")
        }
        return parsed.blocks
    }
}
