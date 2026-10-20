import AmuxValues
import Foundation
import XCTest

@testable import AmuxCore

/// A schedule of the Rust client model as the runtime answered the phone at
/// each wake, written by `just client-traces`.
private struct ClientTrace: Decodable {
    var seed: UInt64
    var every: Int
    var cuts: [Cut]

    struct Cut: Decodable {
        var actions: [String]
        var messages: [String]
        var changes: ChatChanges
        var keys: [String]
        var rows: [Row]
        var frame: ChatFrame
    }
}

/// A chat source answering as the runtime did at one cut of a trace: keys
/// at the window's edges, rows by key and the frame from the Rust session,
/// and the changes it gathered since the cut before.
private final class TraceChat: ChatSource, @unchecked Sendable {
    var cut: ClientTrace.Cut
    private var pending: ChatChanges

    init(_ cut: ClientTrace.Cut) {
        self.cut = cut
        pending = cut.changes
    }

    /// The session moved on to `cut`: its changes wait for the next take.
    func move(to cut: ClientTrace.Cut) {
        self.cut = cut
        pending = cut.changes
    }

    func keys() -> [String] { cut.keys }

    func keys(above newest: String) -> [String]? {
        guard let index = cut.keys.firstIndex(of: newest) else { return nil }
        return Array(cut.keys[(index + 1)...])
    }

    func keys(below oldest: String) -> [String]? {
        guard let index = cut.keys.firstIndex(of: oldest) else { return nil }
        return Array(cut.keys[..<index])
    }

    func oldestKey() -> String? { cut.keys.first }

    func follow(_ following: Bool) {}

    func rows(for keys: [String], options: RowOptions?) -> [Row] {
        // The trace holds rows read with every run folded and none open,
        // which is what the chat model asks for until the reader opens one.
        precondition(options == RowOptions(tools: .collapse(open: [])), "rows asked with \(String(describing: options))")
        let asked = Set(keys)
        return cut.rows.filter { asked.contains($0.id) }
    }

    func askCard() -> AskCard? { nil }
    func overview() -> Overview? { nil }
    func settings() -> SettingsView? { nil }
    func frame() -> ChatFrame? { cut.frame }

    func takeChanges() -> ChatChanges {
        defer { pending = ChatChanges(keys: [], reloaded: false, session: false) }
        return pending
    }

    // The traces hold no acts: the person in them only reads.
    func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> { .failure(RuntimeFailure("a trace sends nothing")) }
    func answer(_ ask: String, choice: Int, note: String?) async -> ActOutcome? { nil }
    func answer(_ ask: String, responses: [QuestionResponse]) async -> ActOutcome? { nil }
    func replyInstead(_ ask: String, text: String, soFar: [QuestionResponse]) async -> ActOutcome? { nil }
    func toggleRun(_ member: String, open: [String]) -> [String] { open }
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
    func putBlob(_ data: Data, name: String, mime: String) async -> Result<BlobRef, RuntimeFailure> { .failure(RuntimeFailure("a trace stores nothing")) }
    func blob(_ hash: [UInt8]) -> Data? { nil }
    func review(_ comparison: Comparison) async -> Result<FrozenReview, RuntimeFailure> { .failure(RuntimeFailure("a trace reviews nothing")) }
    func openOverview(_ comparison: Comparison) async -> Result<Overview, RuntimeFailure> { .failure(RuntimeFailure("a trace has no overview")) }
}

/// The phone's chat model keeps its own sequence of keys and its own cells,
/// reading the runtime only at the sequence's edges and for the keys a
/// change names. Replaying the Rust client model's schedules through it,
/// it must hold the same keys and draw the same rows as the Rust session at
/// every wake: across live rows, appends and revisions, a lagging stream's
/// fresh tail, a Reset's swap and older pages.
@MainActor
final class ChatModelTraceTests: XCTestCase {
    func testEveryRecordedTraceProjectsTheSameRowsAsTheRustModel() throws {
        let traces = try Self.traces()
        XCTAssertFalse(traces.isEmpty, "no client traces were found among the fixtures")
        for (name, trace) in traces {
            guard let first = trace.cuts.first else {
                XCTFail("\(name) has no cuts")
                continue
            }
            let source = TraceChat(first)
            let model = ChatModel(source: source, loadingHintAfter: .zero)
            Self.assertSame(model, first, at: "\(name), cut 0")
            for (index, cut) in trace.cuts.enumerated().dropFirst() {
                source.move(to: cut)
                model.woke()
                Self.assertSame(model, cut, at: "\(name), cut \(index) after \(cut.actions)")
            }
        }
    }

    private static func assertSame(_ model: ChatModel, _ cut: ClientTrace.Cut, at place: String) {
        XCTAssertEqual(model.ids, cut.keys, "the chat model's keys at \(place)")
        XCTAssertEqual(model.heldCount, cut.keys.count, "the held count at \(place)")
        let byKey = Dictionary(uniqueKeysWithValues: cut.rows.map { ($0.id, $0) })
        for id in model.ids {
            XCTAssertEqual(model.cell(for: id).row, byKey[id], "row \(id) at \(place)")
        }
        XCTAssertEqual(model.frame.map(drawn), drawn(cut.frame), "the frame at \(place)")
    }

    /// The frame as the chat model assigns it: a running activity's elapsed
    /// time moves with the clock alone and is not compared.
    private static func drawn(_ frame: ChatFrame) -> ChatFrame {
        var frame = frame
        frame.composer.activity?.elapsedMs = 0
        return frame
    }

    private static func traces() throws -> [(String, ClientTrace)] {
        let directory = try XCTUnwrap(
            Bundle.module.url(forResource: "client-traces", withExtension: nil, subdirectory: "Fixtures"),
            "the client traces are bundled with the tests")
        let files = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" }
            .sorted { $0.lastPathComponent < $1.lastPathComponent }
        return try files.map { file in
            (file.lastPathComponent, try AmuxJSON.decoder.decode(ClientTrace.self, from: Data(contentsOf: file)))
        }
    }
}
