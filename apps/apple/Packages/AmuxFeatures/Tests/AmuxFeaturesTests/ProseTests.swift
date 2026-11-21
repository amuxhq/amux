import AmuxFixtures
import AmuxValues
import SwiftUI
import UIKit
import XCTest
@testable import AmuxFeatures

/// A row's text arrives parsed, so it is drawn in its first frame rather
/// than standing as an empty slot the feed measures too short.
@MainActor
final class ProseTests: XCTestCase {
    func testProseIsDrawnInItsFirstLayout() {
        XCTAssertGreaterThan(firstHeight(of: Markdown(words: "drawn at once: \(UUID())")), 10)
        XCTAssertGreaterThan(
            firstHeight(of: MarkdownFixtures.parsed("1. Split read and write paths.\n2. Add unit tests.")), 30,
            "a list's two items stand on two lines")
    }

    /// How tall the prose stands in its first layout, before any task runs.
    private func firstHeight(of markdown: Markdown) -> CGFloat {
        let host = UIHostingController(rootView: Prose(markdown: markdown))
        host.view.frame = CGRect(x: 0, y: 0, width: 320, height: 0)
        host.view.layoutIfNeeded()
        return host.sizeThatFits(in: CGSize(width: 320, height: CGFloat.greatestFiniteMagnitude)).height
    }
}
