import AmuxCore
import AmuxDesign
import SwiftUI
import UIKit
import XCTest
@testable import AmuxFeatures

/// The list's own behaviour, laid out in a collection view without a window.
@MainActor
final class TranscriptListTests: XCTestCase {
    func testTheTopIsReachedPastCollapsedRows() {
        // A long collapsed tool run leaves the window beginning with many
        // rows that stand at no height. The list judges the top by row
        // index, which holds because the collection view displays a cell
        // at no height like any other, so the oldest rows coming on screen
        // are the oldest rows; a review claimed otherwise, and this pins
        // what was found.
        let collapsed = (0..<25).map { row("c\($0)", UInt64($0), collapsed: true) }
        let shown = (25..<85).map { row("m\($0)", UInt64($0)) }
        let chat = StubChat(rows: collapsed + shown, hasOlder: true)
        let model = ChatModel(source: chat)
        model.reading(atNewest: false)
        let list = FeedCoordinator(
            model: model, environment: environment(), reported: ReportedElements())
        list.view.frame = CGRect(x: 0, y: 0, width: 390, height: 600)

        list.update(
            items: model.ids.map { .row($0) }, notices: [], environment: environment(),
            revision: model.revision, toNewest: model.toNewest, insets: EdgeInsets())
        list.view.layoutIfNeeded()

        XCTAssertEqual(model.paging, .fetching, "the reader at the top was not given older history")
    }

    func testATypeSizeChangeMeasuresTheRowsAgain() {
        // A hosted row takes the type size the screen carries in; when it
        // changes, in Settings or by the driving door, every cached height
        // is stale and the rows are measured again at the new size.
        let chat = StubChat(rows: (0..<10).map { row("m\($0)", UInt64($0)) }, hasOlder: false)
        let model = ChatModel(source: chat)
        let list = FeedCoordinator(
            model: model, environment: environment(), reported: ReportedElements())
        list.view.frame = CGRect(x: 0, y: 0, width: 390, height: 600)
        let items = model.ids.map { ListItem.row($0) }
        list.update(
            items: items, notices: [], environment: environment(), revision: model.revision,
            toNewest: model.toNewest, insets: EdgeInsets())
        list.view.layoutIfNeeded()
        let before = list.view.layoutAttributesForItem(at: IndexPath(item: 0, section: 0))!.frame.height

        list.update(
            items: items, notices: [], environment: environment(typeSize: .accessibility3),
            revision: model.revision, toNewest: model.toNewest, insets: EdgeInsets())
        list.view.layoutIfNeeded()
        let after = list.view.layoutAttributesForItem(at: IndexPath(item: 0, section: 0))!.frame.height

        XCTAssertGreaterThan(after, before, "the row kept its height at the old size")
    }

    func testARowLandingBelowMeasuresTheRowAboveAgain() {
        // A step's rail runs on into a step drawn below it, with a shorter
        // gap under it than a step that ends its run. The step above does
        // not change when one lands under it, but its height does.
        let chat = StubChat(rows: [row("p0", 0), step("s1", 1)], hasOlder: false)
        let model = ChatModel(source: chat)
        let list = FeedCoordinator(
            model: model, environment: environment(), reported: ReportedElements())
        list.view.frame = CGRect(x: 0, y: 0, width: 390, height: 600)
        list.update(
            items: model.ids.map { .row($0) }, notices: [], environment: environment(),
            revision: model.revision, toNewest: model.toNewest, insets: EdgeInsets())
        list.view.layoutIfNeeded()
        let alone = list.view.layoutAttributesForItem(at: IndexPath(item: 1, section: 0))!.frame.height

        chat.land(step("s2", 2))
        model.woke()
        list.update(
            items: model.ids.map { .row($0) }, notices: [], environment: environment(),
            revision: model.revision, toNewest: model.toNewest, insets: EdgeInsets())
        list.view.layoutIfNeeded()
        let joined = list.view.layoutAttributesForItem(at: IndexPath(item: 1, section: 0))!.frame.height

        XCTAssertEqual(model.ids, ["p0", "s1", "s2"])
        XCTAssertEqual(joined, alone - (RowGrid.afterRun - RowGrid.inRun), "the step kept the gap of a run's end")
    }

    func testAFollowingListKeepsItsNewestRowAboveAStandingAreaThatGrows() {
        // The keyboard coming up, a notice appearing above the composer, a
        // taller card docking: the bottom inset grows while the reader
        // follows. The newest row must stay above it, not under it.
        let chat = StubChat(rows: (0..<40).map { row("m\($0)", UInt64($0)) }, hasOlder: false)
        let model = ChatModel(source: chat)
        let list = FeedCoordinator(
            model: model, environment: environment(), reported: ReportedElements())
        list.view.frame = CGRect(x: 0, y: 0, width: 390, height: 600)
        list.update(
            items: model.ids.map { .row($0) }, notices: [], environment: environment(),
            revision: model.revision, toNewest: model.toNewest, insets: EdgeInsets())
        list.view.layoutIfNeeded()
        XCTAssertEqual(list.view.contentOffset.y, list.view.bottomOffset, accuracy: 0.5)

        list.update(
            items: model.ids.map { .row($0) }, notices: [], environment: environment(),
            revision: model.revision, toNewest: model.toNewest,
            insets: EdgeInsets(top: 0, leading: 0, bottom: 300, trailing: 0))
        list.view.layoutIfNeeded()
        XCTAssertEqual(
            list.view.contentOffset.y, list.view.bottomOffset, accuracy: 0.5,
            "the newest row stayed where it was and is now under the standing area")
    }

    func testALongReplyIsPlacedAsTallAsItsCellDrawsIt() {
        // The turn's caption is drawn over the last lines of a long reply on
        // the phone: the reply's row is placed at the height it was measured
        // at, so a cell that draws taller runs under the row below it.
        let numbers = (1...400).map(String.init).joined(separator: "\n")
        let markdown = "Here is the list:\n\n- one thing\n- another thing, longer, that wraps onto a second line on a phone\n\nOr something else."
        for (name, text) in [("numbers", numbers), ("markdown", markdown)] {
            // Drawn as photographed, as the measurer measures: the row's
            // final shape, which the live cell reaches once it has parsed.
            do {
                let photographed = true
                let rows = [prose("r", 0, text), step("s", 1)]
                let chat = StubChat(rows: rows, hasOlder: false)
                let model = ChatModel(source: chat)
                let list = FeedCoordinator(
                    model: model, environment: environment(photographed: photographed),
                    reported: ReportedElements())
                let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 402, height: 874))
                window.addSubview(list.view)
                window.isHidden = false
                list.view.frame = window.bounds
                list.update(
                    items: model.ids.map { .row($0) }, notices: [],
                    environment: environment(photographed: photographed), revision: model.revision,
                    toNewest: model.toNewest, insets: EdgeInsets())
                list.view.scrollToItem(at: IndexPath(item: 0, section: 0), at: .top, animated: false)
                list.view.layoutIfNeeded()
                let placed = list.view.layoutAttributesForItem(at: IndexPath(item: 0, section: 0))!.frame
                guard let cell = list.view.cellForItem(at: IndexPath(item: 0, section: 0)) else {
                    XCTFail("\(name): the reply's cell was not drawn"); continue
                }
                let drawn = cell.contentView.systemLayoutSizeFitting(
                    CGSize(width: placed.width, height: UIView.layoutFittingCompressedSize.height),
                    withHorizontalFittingPriority: .required, verticalFittingPriority: .fittingSizeLevel
                ).height
                XCTAssertEqual(
                    placed.height, drawn, accuracy: 1,
                    "\(name), photographed \(photographed): the reply is placed \(placed.height) tall but its cell draws \(drawn)")
                window.isHidden = true
            }
        }
    }
}

private func environment(typeSize: DynamicTypeSize = .large, photographed: Bool = true) -> CellEnvironment {
    CellEnvironment(
        design: .app, photographed: photographed, reducesMotion: true, reducesTransparency: true,
        hidesNeedsYouDot: false, reportsElements: false, reportedPrefix: nil,
        reportsGeometry: false, dynamicTypeSize: typeSize)
}

private func row(_ id: String, _ order: UInt64, collapsed: Bool = false) -> Row {
    Row(
        id: id, order: order, atMs: 0,
        kind: .prose(text: [.text(id)], streaming: false, workingNote: false),
        collapsed: collapsed, attention: false, decision: nil, parent: nil, run: nil)
}

private func prose(_ id: String, _ order: UInt64, _ text: String) -> Row {
    Row(
        id: id, order: order, atMs: 0,
        kind: .prose(text: [.text(text)], streaming: false, workingNote: false),
        collapsed: false, attention: false, decision: nil, parent: nil, run: nil)
}

/// A step on the rail: a finished background command.
private func step(_ id: String, _ order: UInt64) -> Row {
    Row(
        id: id, order: order, atMs: 0, kind: .background(command: id, running: false, durationMs: nil),
        collapsed: false, attention: false, decision: nil, parent: nil, run: nil)
}

/// A chat that holds its rows and answers nothing else.
private final class StubChat: ChatSource, @unchecked Sendable {
    private(set) var ordered: [Row]
    let hasOlder: Bool
    private var landed: [String] = []

    init(rows: [Row], hasOlder: Bool) {
        ordered = rows
        self.hasOlder = hasOlder
    }

    /// A row arriving after the newest, as the next wake reports it.
    func land(_ row: Row) {
        ordered.append(row)
        landed.append(row.id)
    }

    func keys() -> [String] { ordered.map(\.id) }
    func keys(above newest: String) -> [String]? {
        ordered.firstIndex { $0.id == newest }.map { ordered[($0 + 1)...].map(\.id) }
    }
    func keys(below oldest: String) -> [String]? {
        ordered.firstIndex { $0.id == oldest }.map { ordered[..<$0].map(\.id) }
    }
    func oldestKey() -> String? { ordered.first?.id }
    func follow(_ following: Bool) {}
    func rows(for keys: [String], options: RowOptions?) -> [Row] {
        let wanted = Set(keys)
        return ordered.filter { wanted.contains($0.id) }
    }
    func askCard() -> AskCard? { nil }
    func overview() -> Overview? { nil }
    func settings() -> SettingsView? { nil }
    func frame() -> ChatFrame? {
        ChatFrame(
            agent: AgentKey(host: [1], agent: [2]), name: "a", kind: .claudeSdk, phase: .idle,
            composer: ComposerView(mode: .send, switchedAway: false, activity: nil), controls: ControlsSummary(effort: nil, mode: nil, model: nil, permission: nil), connection: .live, caughtUp: true,
            hasOlder: hasOlder, arrivalsHeld: false, queue: [], underway: [], refused: [], askInput: nil,
            context: nil, ended: nil, git: nil, signIn: nil, waiting: nil)
    }
    func takeChanges() -> ChatChanges {
        defer { landed = [] }
        return ChatChanges(keys: landed, reloaded: false, session: false)
    }
    func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> { .failure(RuntimeFailure("stub")) }
    func answer(_ ask: String, choice: Int, note: String?) async -> ActOutcome? { nil }
    func answer(_ ask: String, responses: [QuestionResponse]) async -> ActOutcome? { nil }
    func replyInstead(_ ask: String, text: String, soFar: [QuestionResponse]) async -> ActOutcome? { nil }
    func toggleRun(_ member: String, open: [String]) -> [String] {
        open.contains(member) ? open.filter { $0 != member } : open + [member]
    }
    func keepOpenRuns(_ open: [String]) -> [String] { open }
    func answerForm(_ ask: String, values: [FormValue]) async -> ActOutcome? { nil }
    func withdraw(_ input: [UInt8]) async -> ActOutcome? { nil }
    func draft(of input: [UInt8]) -> Draft? { nil }
    func sendNow(_ input: [UInt8]) async -> ActOutcome? { nil }
    func resend(_ input: [UInt8]) async -> SendOutcome? { nil }
    func discard(_ input: [UInt8]) {}
    func interrupt() async -> ActOutcome? { nil }
    func change(_ setting: SettingChange) async -> ActOutcome? { nil }
    func resume(with draft: Draft) async -> ActOutcome? { nil }
    func pageOlder(_ rows: UInt32) async -> PageOutcome? { nil }
    func putBlob(_ data: Data, name: String, mime: String) async -> Result<BlobRef, RuntimeFailure> {
        .failure(RuntimeFailure("stub"))
    }
    func blob(_ hash: [UInt8]) -> Data? { nil }
    func review(_ comparison: Comparison) async -> Result<FrozenReview, RuntimeFailure> { .failure(RuntimeFailure("stub")) }
    func openOverview(_ comparison: Comparison) async -> Result<Overview, RuntimeFailure> { .failure(RuntimeFailure("no")) }
}
