import AmuxValues
@testable import AmuxWords
import Foundation
import XCTest

/// The agent's prose arrives parsed by the shared view layer; what is left
/// to this app is drawing its runs and its lists' markers, and refusing to
/// open a link that could act on this device.
final class ProseTextTests: XCTestCase {
    private func run(_ text: String, link: String? = nil, strong: Bool = false, code: Bool = false) -> InlineRun {
        InlineRun(text: text, strong: strong, emphasis: false, strikethrough: false, code: code, link: link)
    }

    func testALinkKeepsItsTextAndItsDestination() {
        let text = ProseText.attributed([run("see "), run("the spec", link: "https://example.com")])
        XCTAssertEqual(String(text.characters), "see the spec")
        XCTAssertEqual(text.runs.compactMap(\.link?.absoluteString), ["https://example.com"])
        // Underlined rather than coloured: the one accent means "waiting for
        // you", and the system's blue is not a colour this design owns.
        XCTAssertEqual(text.runs.compactMap(\.underlineStyle), [.single])
    }

    /// Only web pages and email addresses open; a link to this app or any
    /// other is its words and nothing more.
    func testOnlyWebAndMailLinksStayLinks() {
        let targets = [
            "https://example.com", "http://example.com", "mailto:ada@example.com",
            "amux://callback?code=abc", "tel:123", "file:///etc/hosts",
        ]
        let text = ProseText.attributed(targets.flatMap { [run($0, link: $0), run(" ")] })
        XCTAssertEqual(
            text.runs.compactMap(\.link?.absoluteString),
            ["https://example.com", "http://example.com", "mailto:ada@example.com"])
        XCTAssertEqual(text.runs.compactMap(\.underlineStyle).count, 3)
        XCTAssertTrue(String(text.characters).contains("amux://callback"))
    }

    func testAnOpenableLinkIsAWebPageOrAnEmailAddress() {
        XCTAssertNotNil(OpenableLink(string: "HTTPS://example.com"))
        XCTAssertNotNil(OpenableLink(string: "mailto:ada@example.com"))
        XCTAssertNil(OpenableLink(string: "amux://callback?code=abc"))
        XCTAssertNil(OpenableLink(string: "shortcuts://run-shortcut?name=x"))
        XCTAssertNil(OpenableLink(string: "javascript:alert(1)"))
        XCTAssertNil(OpenableLink(string: "example.com"))
    }

    /// Strong words and code keep the intents `Text` draws them by, and
    /// none of their markup.
    func testStylesAreIntentsNotPunctuation() {
        let text = ProseText.attributed([run("bold", strong: true), run(" and "), run("code", code: true)])
        XCTAssertEqual(String(text.characters), "bold and code")
        XCTAssertEqual(
            text.runs.map(\.inlinePresentationIntent),
            [.stronglyEmphasized, nil, .code])
        XCTAssertEqual(ProseText.plain([run("bold", strong: true), run(" words")]), "bold words")
    }

    /// Numbers count up from the list's first, as CommonMark reads them; a
    /// task says whether it is done.
    func testAListsMarkersCountUpFromItsStart() {
        let item = ProseItem(blocks: [], task: nil)
        XCTAssertEqual(
            ProseText.markers(ordered: true, start: 9, items: [item, item]), ["9.", "10."])
        XCTAssertEqual(ProseText.markers(ordered: false, start: 1, items: [item]), ["\u{2022}"])
        XCTAssertEqual(
            ProseText.markers(
                ordered: false, start: 1,
                items: [ProseItem(blocks: [], task: true), ProseItem(blocks: [], task: false)]),
            ["\u{2713}", "\u{25CB}"])
    }
}
