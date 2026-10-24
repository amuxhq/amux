import AmuxCore
import AmuxFeatures
import SwiftUI
import UIKit
import XCTest
@testable import Amux

/// The chat's list on screen while rows land above or below the reader:
/// the rows the reader is looking at stay exactly where they are, and a
/// reader following the newest row is kept at the bottom.
@MainActor
final class ChatFeedTests: XCTestCase {
    private var window: UIWindow?
    private let probe = Probe()

    override func tearDown() async throws {
        window?.isHidden = true
        window?.rootViewController = nil
        window = nil
    }

    /// What the list reported last: each prose row's text and where it is.
    @MainActor
    private final class Probe {
        var elements: [IdentifiedElement] = []
    }

    /// Messages of a few different lengths, so rows are not all one height.
    private static func messages(_ numbers: ClosedRange<Int>) -> [Row] {
        numbers.map { n in
            let words = "message \(n): the agent reports progress on its task"
                + String(repeating: " and then some more", count: n % 4 * 3)
            return ScriptedChat.row(
                "m\(n)", UInt64(n), .prose(text: [.text(words)], streaming: false, workingNote: false))
        }
    }

    private func show(_ model: ChatModel) throws {
        let probe = self.probe
        let root = ChatScreen(
            model: model,
            subject: ChatSubject(
                name: "flood", host: "desk", directory: "~/src", reach: .online(.direct))
        ) { _ in }
            .reportingIdentifiedElements(prefix: "chat.row.prose")
            .onPreferenceChange(IdentifiedElements.self) { declared in
                MainActor.assumeIsolated { probe.elements = declared }
            }
        let controller = UIHostingController(rootView: root)
        let scene = try XCTUnwrap(
            UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: 402, height: 874)
        window.rootViewController = controller
        window.makeKeyAndVisible()
        self.window = window
    }

    private func spin(_ seconds: Double = 0.4) async {
        try? await Task.sleep(for: .seconds(seconds))
    }

    /// The chat's scroll view: the tallest one that is not a text field.
    private func list() throws -> UIScrollView {
        var found: [UIScrollView] = []
        func walk(_ view: UIView) {
            if let scroll = view as? UIScrollView, !(view is UITextView) { found.append(scroll) }
            view.subviews.forEach(walk)
        }
        walk(try XCTUnwrap(window))
        return try XCTUnwrap(found.max { $0.bounds.height < $1.bounds.height })
    }

    /// Where each prose row on screen is, by its text.
    private func onScreen() throws -> [String: CGFloat] {
        let bounds = try XCTUnwrap(window).bounds
        var rows: [String: CGFloat] = [:]
        for element in probe.elements where element.frame.intersects(bounds) {
            rows[element.label ?? ""] = element.frame.minY
        }
        return rows
    }

    private func assertStill(
        _ before: [String: CGFloat], _ what: String, file: StaticString = #filePath, line: UInt = #line
    ) throws {
        let after = try onScreen()
        let kept = before.keys.filter { after[$0] != nil }
        XCTAssertGreaterThan(kept.count, 3, "\(what): the rows on screen went", file: file, line: line)
        for label in kept {
            XCTAssertEqual(
                after[label]!, before[label]!, accuracy: 0.5, "\(what) moved \(label.prefix(12))",
                file: file, line: line)
        }
    }

    /// The pixels of the rows between the header and the composer, as the
    /// screen shows them. The band starts below the soft edge under the
    /// header, which blurs in whatever rows are scrolled past above it.
    private func rowsBand() throws -> Data {
        let window = try XCTUnwrap(self.window)
        let band = CGRect(x: 0, y: 200, width: window.bounds.width, height: 500)
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.preferredRange = .standard
        let image = UIGraphicsImageRenderer(size: band.size, format: format).image { _ in
            window.drawHierarchy(
                in: CGRect(origin: CGPoint(x: 0, y: -band.minY), size: window.bounds.size),
                afterScreenUpdates: true)
        }
        return try XCTUnwrap(image.cgImage?.dataProvider?.data as Data?)
    }

    /// Up into history, moved the way the door moves it.
    private func scrollUp(_ list: UIScrollView, _ model: ChatModel, screens: CGFloat) async {
        model.reading(atNewest: false)
        list.setContentOffset(
            CGPoint(x: 0, y: max(-list.adjustedContentInset.top, list.contentOffset.y - screens * list.bounds.height)),
            animated: false)
        await spin()
    }

    /// Where the bottom of the list's space is, in its content.
    private func bottom(of list: UIScrollView) -> CGFloat {
        list.contentOffset.y + list.bounds.height - list.adjustedContentInset.bottom
    }

    func testRowsArrivingBelowAReaderInHistoryAreHeldAndMoveNothingOnScreen() async throws {
        let source = ScriptedChat(rows: Self.messages(1...600), frame: ScriptedChat.frame(hasOlder: false))
        let model = ChatModel(source: source)
        try show(model)
        await spin(1)
        let list = try list()
        XCTAssertEqual(bottom(of: list), list.contentSize.height, accuracy: 1, "a chat opens at its newest row")
        await scrollUp(list, model, screens: 2)
        XCTAssertFalse(model.following)
        XCTAssertGreaterThan(try onScreen().count, 3, "prose rows report where they are")

        let before = try onScreen()
        let sequence = model.sequence
        source.append(Self.messages(601...640))
        source.deliver(to: model)
        await spin()
        try assertStill(before, "rows arriving")
        XCTAssertEqual(model.sequence, sequence, "the session holds them apart")
        XCTAssertTrue(model.newActivity)
    }

    func testAPageLandingAboveAReaderAtTheTopMovesNothingOnScreen() async throws {
        let source = ScriptedChat(rows: Self.messages(961...1_000), frame: ScriptedChat.frame(hasOlder: true))
        source.paged = .arrived(0)
        // The page answers after the reader has come to rest at the top, so
        // the loading notice is showing above the rows when they are
        // photographed and goes as the page lands.
        source.pageTakes = .seconds(0.6)
        let model = ChatModel(source: source)
        try show(model)
        await spin(1)
        let list = try list()
        await scrollUp(list, model, screens: 40)
        XCTAssertEqual(model.paging, .fetching)

        let before = try onScreen()
        let photographed = try rowsBand()
        source.prepend(Self.messages(921...960))
        source.deliver(to: model)
        await spin(1)
        XCTAssertEqual(model.paging, .idle)
        try assertStill(before, "a page landing")
        let now = try rowsBand()
        XCTAssertEqual(now, photographed, "a page landing moved the rows on screen")
        XCTAssertEqual(model.ids.first, "m921", "the page the reader waited at the top for is held above")
        XCTAssertEqual(model.ids.count, 80)
    }

    func testAFollowingListKeepsItsNewestRowAtTheBottomAsRowsArrive() async throws {
        let source = ScriptedChat(rows: Self.messages(1...300), frame: ScriptedChat.frame())
        let model = ChatModel(source: source)
        try show(model)
        await spin(1)
        let list = try list()
        for n in 301...340 {
            source.append(Self.messages(n...n))
            source.deliver(to: model)
            await spin(0.05)
        }
        await spin()
        XCTAssertTrue(model.following)
        let newest = try XCTUnwrap(probe.elements.last)
        XCTAssertTrue(newest.label?.hasPrefix("message 340:") == true)
        XCTAssertEqual(bottom(of: list), list.contentSize.height, accuracy: 1, "the list is at its newest row")
    }

    /// Under a stream the window drops its oldest row for every one that
    /// arrives: what the screen shows at the end is exactly what it shows
    /// for a chat opened on those same rows.
    func testAFollowingListStaysRightAsTheWindowDropsItsOldestRows() async throws {
        let source = ScriptedChat(rows: Self.messages(1...200), frame: ScriptedChat.frame(hasOlder: true))
        source.cap = 200
        let model = ChatModel(source: source)
        try show(model)
        await spin(1)
        let list = try list()
        for n in 201...260 {
            source.append(Self.messages(n...n))
            source.deliver(to: model)
            await spin(0.03)
        }
        await spin()
        XCTAssertEqual(model.ids.first, "m61")
        XCTAssertEqual(bottom(of: list), list.contentSize.height, accuracy: 1, "the list is at its newest row")
        let streamed = try rowsBand()

        let opened = ChatModel(source: ScriptedChat(rows: Self.messages(61...260), frame: ScriptedChat.frame(hasOlder: true)))
        try show(opened)
        await spin(1)
        XCTAssertEqual(try rowsBand(), streamed, "the streamed rows are not drawn as the same rows opened")
    }

    /// A row on screen whose content grows takes its new height, as a
    /// streaming reply does, with the rows below moved down and the bottom
    /// kept for a follower.
    func testARowThatGrowsTakesItsNewHeight() async throws {
        let source = ScriptedChat(rows: Self.messages(1...40), frame: ScriptedChat.frame())
        let model = ChatModel(source: source)
        try show(model)
        await spin(1)
        let list = try XCTUnwrap(try list() as? UICollectionView)
        let path = IndexPath(item: 37, section: 0)
        let before = try XCTUnwrap(list.cellForItem(at: path)).bounds.height
        let longer = "message 38: " + String(repeating: "a much longer reply that runs on ", count: 12)
        source.revise([ScriptedChat.row("m38", 38, .prose(text: [.text(longer)], streaming: true, workingNote: false))])
        source.deliver(to: model)
        await spin()
        let cell = try XCTUnwrap(list.cellForItem(at: path))
        XCTAssertGreaterThan(cell.bounds.height, before + 40, "the row did not grow")
        let next = try XCTUnwrap(list.cellForItem(at: IndexPath(item: 38, section: 0)))
        XCTAssertEqual(next.frame.minY, cell.frame.maxY, accuracy: 0.5, "the row below did not move down")
        XCTAssertEqual(bottom(of: list), list.contentSize.height, accuracy: 1, "the list is at its newest row")
    }

    func testNewActivityFromHistoryLandsAtTheNewestRow() async throws {
        let source = ScriptedChat(rows: Self.messages(1...600), frame: ScriptedChat.frame())
        let model = ChatModel(source: source)
        try show(model)
        await spin(1)
        let list = try list()
        await scrollUp(list, model, screens: 3)
        source.append(Self.messages(601...620))
        source.deliver(to: model)
        await spin()
        XCTAssertTrue(model.newActivity)
        model.jumpToNewest()
        source.deliver(to: model)
        await spin(1)
        XCTAssertTrue(model.following)
        XCTAssertEqual(model.ids.last, "m620")
        XCTAssertEqual(bottom(of: list), list.contentSize.height, accuracy: 1, "the list is at its newest row")
        let newest = try XCTUnwrap(probe.elements.last)
        XCTAssertTrue(newest.label?.hasPrefix("message 620:") == true)
    }
}
