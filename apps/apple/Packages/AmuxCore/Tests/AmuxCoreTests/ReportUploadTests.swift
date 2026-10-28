import AmuxValues
import Foundation
import XCTest
@testable import AmuxCore

/// A capture with everything in it, so a test that wants a part missing takes
/// that one part away rather than building a different capture.
private func wholeCapture() -> ReportCapture {
    ReportCapture(
        frame: FrozenFrame(png: Data([0x89, 0x50, 0x4E, 0x47]), width: 402, height: 874, scale: 3),
        dump: Task { .success(URL(fileURLWithPath: "/reports/dump-1")) },
        trace: "{\"kind\":\"route\",\"screen\":\"run\"}\n",
        route: "run")
}

private let dumped: Result<URL, PartAbsent> = .success(URL(fileURLWithPath: "/reports/dump-1"))

/// `report.json` as a reader sees it: parsed back out of the bytes that would
/// have been handed over, rather than read off the value that produced them.
private func header(_ upload: ReportUpload) throws -> [String: Any] {
    try XCTUnwrap(JSONSerialization.jsonObject(with: upload.header) as? [String: Any])
}

private func parts(_ upload: ReportUpload) throws -> [String: Any] {
    try XCTUnwrap(try header(upload)["parts"] as? [String: Any])
}

final class ReportUploadTests: XCTestCase {
    func testAStartupDiagnosticSurvivesWithoutARuntimeDump() throws {
        var capture = wholeCapture()
        capture.dump = nil
        capture.runtimeFailure = "installation profile path disagrees with its namespace"
        let upload = ReportAssembly.upload(
            from: capture, draft: ReportDraft(), build: "amux-ios/test",
            dump: .failure(PartAbsent("nothing was running, so there was nothing to dump")))
        XCTAssertEqual(try header(upload)["detail"] as? String,
                       "run\ninstallation profile path disagrees with its namespace")
        XCTAssertEqual(upload.path, "", "no dump for the runtime to find")
    }

    /// The phone declares the parts it hands over; the dump is the runtime's
    /// to declare, since only it knows whether it holds one.
    func testThePhoneDeclaresWhatItHandsOver() throws {
        let upload = ReportAssembly.upload(
            from: wholeCapture(),
            draft: ReportDraft(note: "the queued message stays on screen"),
            build: "amux-ios/0.1.0", dump: dumped)

        let declared = try parts(upload)
        XCTAssertEqual(declared["frame"] as? String, "present")
        XCTAssertEqual(declared["trace"] as? String, "present")
        XCTAssertNil(declared["dump"], "the runtime declares the dump it holds")
        XCTAssertEqual(upload.path, "/reports/dump-1")
        XCTAssertEqual(upload.frame, Data([0x89, 0x50, 0x4E, 0x47]))
        XCTAssertEqual(upload.trace, "{\"kind\":\"route\",\"screen\":\"run\"}\n")
    }

    /// A part that is missing says why. A reader who finds no trace has to be
    /// able to tell "withheld" from "lost" from "never existed".
    func testAMissingPartCarriesTheReasonItIsMissing() throws {
        var capture = wholeCapture()
        capture.trace = nil
        capture.traceAbsent = "the view-state recording could not be written"

        let upload = ReportAssembly.upload(
            from: capture, draft: ReportDraft(), build: "amux-ios/0.1.0",
            dump: .failure(PartAbsent("nothing was connected")))

        let declared = try parts(upload)
        for (part, reason) in [
            ("trace", "the view-state recording could not be written"),
            ("dump", "nothing was connected"),
        ] {
            let absence = try XCTUnwrap(
                (declared[part] as? [String: Any])?["absent"] as? [String: Any],
                "\(part) should be declared absent")
            XCTAssertEqual(absence["reason"] as? String, reason)
        }
        XCTAssertNil(upload.trace, "a trace declared absent is not handed over")
        XCTAssertNil(declared["trace_kind"], "a report with no trace names no recorder")
    }

    /// Which recorder made the trace decides where the trace can be replayed,
    /// so a trace that names none is not worth storing. The phone's came from
    /// a native view, which a terminal cannot put back.
    func testATraceNamesTheRecorderThatMadeIt() throws {
        let upload = ReportAssembly.upload(
            from: wholeCapture(), draft: ReportDraft(), build: "amux-ios/0.1.0", dump: dumped)

        XCTAssertEqual(try parts(upload)["trace_kind"] as? String, "native_view")
    }

    /// The picture alone says nothing about how big the screen was, so the
    /// rectangles beside it would mean nothing without the geometry.
    func testTheFrameRecordsThePointsItsRectanglesAreMeasuredIn() throws {
        let upload = ReportAssembly.upload(
            from: wholeCapture(),
            draft: ReportDraft(marks: [
                ReportMark(x: 24, y: 236.5, width: 354, height: 30, note: "this row"),
            ]),
            build: "amux-ios/0.1.0", dump: dumped)

        let read = try header(upload)
        let geometry = try XCTUnwrap(read["image_frame"] as? [String: Any])
        XCTAssertEqual(geometry["width_pt"] as? Double, 402)
        XCTAssertEqual(geometry["height_pt"] as? Double, 874)
        XCTAssertEqual(geometry["scale"] as? Int, 3)
        // A terminal's viewport is cells and this frame has none.
        XCTAssertTrue(read["viewport"] is NSNull)

        let marks = try XCTUnwrap(read["marks"] as? [[String: Any]])
        XCTAssertEqual(marks.count, 1)
        XCTAssertEqual(marks[0]["x"] as? Double, 24)
        XCTAssertEqual(marks[0]["y"] as? Double, 236.5)
        XCTAssertEqual(marks[0]["note"] as? String, "this row")
    }

    /// The header is read by the runtime and the account service, whose
    /// spellings are the contract.
    func testTheHeaderIsWrittenInTheWordsTheReaderKnows() throws {
        let upload = ReportAssembly.upload(
            from: wholeCapture(), draft: ReportDraft(note: "it stays on screen"),
            build: "amux-ios/0.1.0", gitSHA: String(repeating: "a", count: 40),
            createdAt: Date(timeIntervalSince1970: 1_788_395_144.348), dump: dumped)

        let read = try header(upload)
        XCTAssertEqual(read["build"] as? String, "amux-ios/0.1.0")
        XCTAssertEqual(read["git_sha"] as? String, String(repeating: "a", count: 40))
        XCTAssertEqual(read["note"] as? String, "it stays on screen")
        XCTAssertEqual(read["detail"] as? String, "run")
        XCTAssertEqual(read["created_at"] as? String, "2026-09-03T00:25:44.348Z")
        // The schema and the kind are the runtime's to write.
        XCTAssertNil(read["schema_version"])
        XCTAssertNil(read["kind"])
    }

    /// A failed send keeps everything. Somebody who wrote three notes about a
    /// bug on a train must not lose them to a tunnel.
    @MainActor
    func testAFailedSendKeepsTheDraftSoRetryIsOnePress() async throws {
        let reports = ReportStore(
            capture: wholeCapture(),
            draft: ReportDraft(note: "it stays on screen", marks: [
                ReportMark(x: 1, y: 2, width: 3, height: 4, note: "this row"),
            ]),
            open: true)
        let runtime = OneRuntime(notSent: "the account service could not be reached: offline")

        await reports.send(with: runtime, as: AccountId("ada"), build: "amux-ios/0.1.0",
                           now: Date(timeIntervalSince1970: 1000))

        XCTAssertEqual(reports.sending, .failed("the account service could not be reached: offline"))
        XCTAssertEqual(reports.draft.note, "it stays on screen")
        XCTAssertEqual(reports.draft.marks.count, 1)
        XCTAssertNotNil(reports.capture)
        let first = try XCTUnwrap(runtime.handed.first)
        XCTAssertEqual(first.path, "/reports/dump-1")
        XCTAssertEqual(try header(first)["created_at"] as? String, "1970-01-01T00:16:40.000Z")

        runtime.notSent = ""
        await reports.send(with: runtime, as: AccountId("ada"), build: "amux-ios/0.1.0",
                           now: Date(timeIntervalSince1970: 2000))

        XCTAssertEqual(reports.sending, .sent(ReportReceipt(
            id: "report-7", receivedAt: Date(timeIntervalSince1970: 1_788_395_144))))
        // The retry names the report the runtime kept, with every byte the
        // first attempt handed over: it is not a second, emptier report.
        XCTAssertEqual(runtime.handed.count, 2)
        let retry = runtime.handed[1]
        XCTAssertEqual(retry.path, OneRuntime.kept)
        XCTAssertEqual(retry.header, first.header, "retry preserves every byte, including the time")
        XCTAssertEqual(retry.frame, first.frame)
        XCTAssertEqual(retry.trace, first.trace)
        await reports.send(with: runtime, as: AccountId("ada"), build: "later")
        XCTAssertEqual(runtime.handed.count, 2, "sent is terminal")
    }

    @MainActor
    func testADeclaredSentReportCannotBeSentAgainEvenWithoutAnAccount() async {
        let receipt = ReportReceipt(id: "already-sent", receivedAt: Date())
        let reports = ReportStore(capture: wholeCapture(), sending: .sent(receipt), open: true)
        let runtime = OneRuntime(notSent: "")
        await reports.send(with: runtime, as: nil, build: "test")
        XCTAssertEqual(reports.sending, .sent(receipt))
        XCTAssertTrue(runtime.handed.isEmpty)
    }

    @MainActor
    func testDismissingAReportLetsTheNextCaptureSendItsOwn() async throws {
        let reports = ReportStore(capture: wholeCapture(), open: true)
        let runtime = OneRuntime(notSent: "")
        await reports.send(with: runtime, as: AccountId("ada"), build: "test",
                           now: Date(timeIntervalSince1970: 1000))
        reports.dismiss()
        XCTAssertNil(reports.sealed)
        XCTAssertTrue(reports.begin(UploadFreeze()))
        reports.draft.note = "a different problem"
        await reports.send(with: runtime, as: AccountId("ada"), build: "test",
                           now: Date(timeIntervalSince1970: 2000))
        XCTAssertEqual(runtime.handed.count, 2)
        XCTAssertEqual(runtime.handed[1].path, "/reports/dump-1", "a new report, from its own dump")
        XCTAssertEqual(try header(runtime.handed[1])["note"] as? String, "a different problem")
    }

    /// A phone nobody has signed in on can still take a screenshot and write
    /// on it. Pressing Send there says why it cannot go, in the place the
    /// account service's own refusals are said; what it must not do is
    /// nothing, leaving somebody pressing a button and waiting.
    @MainActor
    func testSendingWithNobodySignedInSaysWhyInsteadOfDoingNothing() async throws {
        let reports = ReportStore(
            capture: wholeCapture(),
            draft: ReportDraft(note: "the fleet is empty"),
            open: true)
        let runtime = OneRuntime(notSent: "")

        await reports.send(with: runtime, as: nil, build: "amux-ios/0.1.0")

        guard case .failed(let why) = reports.sending else {
            return XCTFail("the report was left at \(reports.sending)")
        }
        XCTAssertEqual(why, "Sign in to the account this report is about, then send it again.")
        // Nothing was handed over, and nothing written was thrown away:
        // signing in and pressing Send again sends this same report.
        XCTAssertEqual(runtime.handed.count, 0)
        XCTAssertEqual(reports.draft.note, "the fleet is empty")
        XCTAssertNotNil(reports.capture)
    }
}

/// A runtime that keeps every report where the real one does, sends it or
/// says why not, and remembers what it was handed.
private final class OneRuntime: ReportSubmitting, @unchecked Sendable {
    static let kept = "/reports/report-1"
    var notSent: String
    private(set) var handed: [ReportUpload] = []

    init(notSent: String) {
        self.notSent = notSent
    }

    func submitReport(_ upload: ReportUpload) async -> Result<ReportSent, RuntimeFailure> {
        handed.append(upload)
        return .success(ReportSent(
            reportPath: Self.kept, id: notSent.isEmpty ? "report-7" : "",
            receivedAtMs: notSent.isEmpty ? 1_788_395_144_000 : 0, notSent: notSent))
    }
}

@MainActor
private final class UploadFreeze: ReportFreezing {
    func freeze() -> ReportCapture? { wholeCapture() }
}
