import SwiftUI
import UIKit
import XCTest
@testable import AmuxFeatures

/// A row's text is drawn in its first frame when the feed has already
/// parsed it to measure the row, rather than standing as an empty slot
/// until the row's own parse returns.
@MainActor
final class ProseTests: XCTestCase {
    func testTextParsedToMeasureItsRowIsDrawnAtOnce() {
        let markdown = "measured before its cell: \(UUID())"
        _ = ParsedMarkdown.parse(markdown)
        XCTAssertGreaterThan(firstHeight(of: markdown), 10, "the measured text was not drawn at once")
    }

    func testTextNotYetParsedWaitsForItsParse() {
        // The other side: text nobody has parsed is parsed away from the
        // main thread, and stands empty meanwhile. Without this the test
        // above could pass with a row that never waits at all.
        XCTAssertLessThan(firstHeight(of: "never measured: \(UUID())"), 1)
    }

    /// How tall the prose stands in its first layout, before any task runs.
    private func firstHeight(of markdown: String) -> CGFloat {
        let host = UIHostingController(rootView: Prose(markdown: markdown))
        host.view.frame = CGRect(x: 0, y: 0, width: 320, height: 0)
        host.view.layoutIfNeeded()
        return host.sizeThatFits(in: CGSize(width: 320, height: CGFloat.greatestFiniteMagnitude)).height
    }
}
