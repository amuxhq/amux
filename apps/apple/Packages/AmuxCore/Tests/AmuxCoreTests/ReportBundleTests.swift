import Foundation
import XCTest
@testable import AmuxCore

/// A capture with everything in it, so a test that wants a part missing takes
/// that one part away rather than building a different capture.
private func wholeCapture() -> ReportCapture {
    ReportCapture(
        frame: FrozenFrame(png: Data([0x89, 0x50, 0x4E, 0x47]), width: 402, height: 874, scale: 3),
        dump: Task { .success(dumpDirectory) },
        trace: "{\"kind\":\"route\",\"screen\":\"run\"}\n",
        route: "run")
}

/// A dump as the runtime writes one: a manifest and a part per stage.
private let dumpDirectory: URL = {
    let directory = FileManager.default.temporaryDirectory
        .appendingPathComponent("dump-\(UUID().uuidString)", isDirectory: true)
    let client = directory.appendingPathComponent("client/fleet", isDirectory: true)
    try? FileManager.default.createDirectory(at: client, withIntermediateDirectories: true)
    try? Data("{\"reason\":\"report\"}".utf8)
        .write(to: directory.appendingPathComponent("manifest.json"))
    try? Data("fleet".utf8).write(to: client.appendingPathComponent("state.txt"))
    return directory
}()

private let dumped: Result<[ReportPart], PartAbsent> = .success([
    ReportPart(name: "dump/client/fleet/state.txt", data: Data("fleet".utf8)),
    ReportPart(name: "dump/manifest.json", data: Data("{}".utf8)),
])

/// `report.json` as a reader sees it: parsed back out of the bytes that would
/// have been sent, rather than read off the value that produced them.
private func header(_ bundle: ReportBundle) throws -> [String: Any] {
    let report = try XCTUnwrap(bundle.part(ReportAssembly.reportFile))
    let bytes = try XCTUnwrap(report.data)
    return try XCTUnwrap(JSONSerialization.jsonObject(with: bytes) as? [String: Any])
}

private func parts(_ bundle: ReportBundle) throws -> [String: Any] {
    try XCTUnwrap(try header(bundle)["parts"] as? [String: Any])
}

final class ReportBundleTests: XCTestCase {
    func testAStartupDiagnosticSurvivesWithoutARuntimeDump() throws {
        var capture = wholeCapture()
        capture.dump = nil
        capture.runtimeFailure = "installation profile path disagrees with its namespace"
        let bundle = ReportAssembly.bundle(
            from: capture, draft: ReportDraft(), build: "amux-ios/test",
            dump: .failure(PartAbsent("nothing was running, so there was nothing to dump")))
        XCTAssertEqual(try header(bundle)["detail"] as? String,
                       "run\ninstallation profile path disagrees with its namespace")
        XCTAssertFalse(bundle.parts.contains { $0.name.hasPrefix("dump/") })
    }

    /// Every part the layout names is declared, and each declaration matches a
    /// file that is actually here. The account service refuses a bundle whose
    /// declarations and whose files disagree, so this is the whole contract.
    func testEveryPartIsDeclaredAndCarried() throws {
        let bundle = ReportAssembly.bundle(
            from: wholeCapture(),
            draft: ReportDraft(note: "the queued message stays on screen"),
            build: "amux-ios/0.1.0", dump: dumped)

        let declared = try parts(bundle)
        for part in ["frame", "trace", "dump"] {
            XCTAssertEqual(
                declared[part] as? String, "present",
                "\(part) should be declared present")
        }
        // The runtime's log rides in the dump, redacted; no raw copy beside it.
        let log = try XCTUnwrap((declared["log"] as? [String: Any])?["absent"] as? [String: Any])
        XCTAssertEqual(log["reason"] as? String, "the runtime's log is in the dump, redacted")
        for file in [
            ReportAssembly.reportFile, ReportAssembly.frameFile, ReportAssembly.traceFile,
            "dump/manifest.json", "dump/client/fleet/state.txt",
        ] {
            XCTAssertEqual(
                bundle.part(file)?.present, true, "\(file) should be carried")
        }
        XCTAssertEqual(bundle.parts.first?.name, ReportAssembly.reportFile)
    }

    /// A part that is missing says why. A reader who finds no trace has to be
    /// able to tell "withheld" from "lost" from "never existed".
    func testAMissingPartCarriesTheReasonItIsMissing() throws {
        var capture = wholeCapture()
        capture.trace = nil
        capture.traceAbsent = "the view-state recording could not be written"

        let bundle = ReportAssembly.bundle(
            from: capture, draft: ReportDraft(), build: "amux-ios/0.1.0",
            dump: .failure(PartAbsent("nothing was connected")))

        let declared = try parts(bundle)
        for (part, reason) in [
            ("trace", "the view-state recording could not be written"),
            ("dump", "nothing was connected"),
            ("log", "the runtime's log is in the dump, redacted"),
        ] {
            let absence = try XCTUnwrap(
                (declared[part] as? [String: Any])?["absent"] as? [String: Any],
                "\(part) should be declared absent")
            XCTAssertEqual(absence["reason"] as? String, reason)
        }
        // Declared absent means not carried. A bundle that declared a part
        // absent and shipped it anyway is refused by the account service.
        XCTAssertEqual(
            bundle.part(ReportAssembly.traceFile)?.present, false, "the trace should not be carried")
        XCTAssertNil(bundle.part("log.txt"), "no log file is carried")
        XCTAssertNil(declared["trace_kind"], "a bundle with no trace names no recorder")
    }

    /// Which recorder made the trace decides where the trace can be replayed,
    /// so a trace that names none is not worth storing. The phone's came from
    /// a native view, which a terminal cannot put back.
    func testATraceNamesTheRecorderThatMadeIt() throws {
        let bundle = ReportAssembly.bundle(
            from: wholeCapture(), draft: ReportDraft(), build: "amux-ios/0.1.0", dump: dumped)

        XCTAssertEqual(try parts(bundle)["trace_kind"] as? String, "native_view")
    }

    /// The picture alone says nothing about how big the screen was, so the
    /// rectangles beside it would mean nothing without the geometry.
    func testTheFrameRecordsThePointsItsRectanglesAreMeasuredIn() throws {
        let bundle = ReportAssembly.bundle(
            from: wholeCapture(),
            draft: ReportDraft(marks: [
                ReportMark(x: 24, y: 236.5, width: 354, height: 30, note: "this row"),
            ]),
            build: "amux-ios/0.1.0", dump: dumped)

        let read = try header(bundle)
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

    /// The header is read by a Rust type whose spellings are the contract.
    func testTheHeaderIsWrittenInTheWordsTheReaderKnows() throws {
        let bundle = ReportAssembly.bundle(
            from: wholeCapture(), draft: ReportDraft(note: "it stays on screen"),
            build: "amux-ios/0.1.0", gitSHA: String(repeating: "a", count: 40),
            createdAt: Date(timeIntervalSince1970: 1_788_395_144.348), dump: dumped)

        let read = try header(bundle)
        XCTAssertEqual(read["schema_version"] as? Int, 2)
        XCTAssertEqual(read["build"] as? String, "amux-ios/0.1.0")
        XCTAssertEqual(read["git_sha"] as? String, String(repeating: "a", count: 40))
        XCTAssertEqual(read["kind"] as? String, "bug")
        XCTAssertEqual(read["status"] as? String, "open")
        XCTAssertEqual(read["note"] as? String, "it stays on screen")
        XCTAssertEqual(read["detail"] as? String, "run")
        XCTAssertEqual(read["created_at"] as? String, "2026-09-03T00:25:44.348Z")
        // Nothing on the phone replayed it. A native trace replays on the
        // platform that drew it, so the phone declines to claim a verdict.
        XCTAssertEqual(read["replay"] as? String, "unchecked")
    }

    /// The dump's files travel under `dump/` by their paths inside it, read
    /// from the directory the runtime wrote.
    func testTheDumpTravelsFileByFile() async throws {
        let read = await ReportAssembly.dumpParts(Task { .success(dumpDirectory) })
        let parts = try read.get()
        XCTAssertEqual(parts.map(\.name), ["dump/client/fleet/state.txt", "dump/manifest.json"])
        XCTAssertEqual(parts.first?.data, Data("fleet".utf8))
        let missing = await ReportAssembly.dumpParts(nil)
        XCTAssertThrowsError(try missing.get())
    }

    /// A failed upload keeps everything. Somebody who wrote three notes about
    /// a bug on a train must not lose them to a tunnel.
    @MainActor
    func testAFailedUploadKeepsTheDraftSoRetryIsOnePress() async throws {
        let reports = ReportStore(
            capture: wholeCapture(),
            draft: ReportDraft(note: "it stays on screen", marks: [
                ReportMark(x: 1, y: 2, width: 3, height: 4, note: "this row"),
            ]),
            open: true)
        let cloud = OneUpload(.failure(.network("offline")))

        await reports.send(with: cloud, as: AccountId("ada"), build: "amux-ios/0.1.0",
                           now: Date(timeIntervalSince1970: 1000))

        XCTAssertEqual(reports.sending, .failed("offline"))
        XCTAssertEqual(reports.draft.note, "it stays on screen")
        XCTAssertEqual(reports.draft.marks.count, 1)
        XCTAssertNotNil(reports.capture)

        let refused = try XCTUnwrap(cloud.sent.first)
        let refusedHeader = try header(ReportBundle(parts: refused))
        XCTAssertEqual(refusedHeader["created_at"] as? String, "1970-01-01T00:16:40.000Z")

        cloud.answer = .success(ReportReceipt(id: "report-7", receivedAt: Date()))
        await reports.send(with: cloud, as: AccountId("ada"), build: "amux-ios/0.1.0",
                           now: Date(timeIntervalSince1970: 2000))

        XCTAssertEqual(reports.sending, .sent(ReportReceipt(id: "report-7", receivedAt: cloud.at)))
        // The same report went both times, with everything that was written on
        // it: a retry is not a second, emptier report.
        XCTAssertEqual(cloud.sent.count, 2)
        XCTAssertEqual(cloud.sent.last, refused, "retry preserves every byte, including stamp")
        let again = await reports.assembled(build: "later")
        XCTAssertEqual(again?.parts, refused)
        await reports.send(with: cloud, as: AccountId("ada"), build: "later")
        XCTAssertEqual(cloud.sent.count, 2, "sent is terminal")
        XCTAssertEqual(reports.sending, .sent(ReportReceipt(id: "report-7", receivedAt: cloud.at)))
        XCTAssertEqual(cloud.sent.last?.map(\.name), [
            ReportAssembly.reportFile, ReportAssembly.frameFile, ReportAssembly.traceFile,
            "dump/client/fleet/state.txt", "dump/manifest.json",
        ])
    }

    @MainActor
    func testADeclaredSentReportCannotBeSentAgainEvenWithoutAnAccount() async {
        let receipt = ReportReceipt(id: "already-sent", receivedAt: Date())
        let reports = ReportStore(capture: wholeCapture(), sending: .sent(receipt), open: true)
        let cloud = OneUpload(.failure(.network("offline")))
        await reports.send(with: cloud, as: nil, build: "test")
        XCTAssertEqual(reports.sending, .sent(receipt))
        XCTAssertTrue(cloud.sent.isEmpty)
    }

    @MainActor
    func testDismissingAReportLetsTheNextCaptureSendItsOwnBundle() async throws {
        let reports = ReportStore(capture: wholeCapture(), open: true)
        let cloud = OneUpload(.success(ReportReceipt(id: "sent", receivedAt: Date())))
        await reports.send(with: cloud, as: AccountId("ada"), build: "test",
                           now: Date(timeIntervalSince1970: 1000))
        reports.dismiss()
        XCTAssertNil(reports.uploadBundle)
        XCTAssertTrue(reports.begin(BundleFreeze()))
        reports.draft.note = "a different problem"
        await reports.send(with: cloud, as: AccountId("ada"), build: "test",
                           now: Date(timeIntervalSince1970: 2000))
        XCTAssertEqual(cloud.sent.count, 2)
        let first = try header(ReportBundle(parts: cloud.sent[0]))
        let second = try header(ReportBundle(parts: cloud.sent[1]))
        XCTAssertNotEqual(first["stamp"] as? String, second["stamp"] as? String)
        XCTAssertEqual(second["note"] as? String, "a different problem")
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
        let cloud = OneUpload(.success(ReportReceipt(id: "report-7", receivedAt: Date())))

        await reports.send(with: cloud, as: nil, build: "amux-ios/0.1.0")

        guard case .failed(let why) = reports.sending else {
            return XCTFail("the report was left at \(reports.sending)")
        }
        XCTAssertEqual(why, "Sign in to the account this report is about, then send it again.")
        // Nothing was uploaded, and nothing written was thrown away: signing
        // in and pressing Send again sends this same report.
        XCTAssertEqual(cloud.sent.count, 0)
        XCTAssertEqual(reports.draft.note, "the fleet is empty")
        XCTAssertNotNil(reports.capture)
    }
}

/// A cloud that answers an upload one way and remembers what it was handed.
@MainActor
private final class OneUpload: CloudService {
    var answer: Result<ReportReceipt, CloudError>
    private(set) var sent: [[ReportPart]] = []
    let at = Date(timeIntervalSince1970: 1_788_395_144)

    init(_ answer: Result<ReportReceipt, CloudError>) {
        self.answer = answer
    }

    func uploadReport(
        _ id: AccountId, bundle: ReportBundle
    ) async throws(CloudError) -> ReportReceipt {
        sent.append(bundle.parts)
        switch answer {
        case .success(let receipt): return ReportReceipt(id: receipt.id, receivedAt: at)
        case .failure(let error): throw error
        }
    }

    func handOver(_ id: AccountId) async -> Handover? { nil }
    func lend(from lender: @escaping @Sendable (AccountId) async -> String?) async {}
    func forgetSession(_ id: AccountId) async throws {}
    func signIn(_ intent: SignInIntent, presenting: any WebAuthPresenter) async throws(CloudError) -> SignedInAccount {
        throw .unauthenticated
    }
    func account(_ id: AccountId) async throws(CloudError) -> AccountFacts { throw .timeout }
    func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement { .none }
    func recordPurchase(_ id: AccountId, signedTransaction: String) async throws(CloudError) {
        throw .timeout
    }
    func requestDeletion(
        _ id: AccountId, confirmedEmail: String
    ) async throws(CloudError) -> DeletionOutcome { .deleted }
}


@MainActor
private final class BundleFreeze: ReportFreezing {
    func freeze() -> ReportCapture? { wholeCapture() }
}
