import AmuxCore
import UIKit
import XCTest
@testable import Amux

/// The freeze every build takes, including one with no driving tools in it.
@MainActor
final class ReportFreezeTests: XCTestCase {
    private func window() -> UIWindow {
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 120, height: 200))
        let root = UIViewController()
        root.view.backgroundColor = .systemTeal
        window.rootViewController = root
        window.isHidden = false
        return window
    }

    /// Photographed, named, and honest about the one recording it cannot make:
    /// a build without the driving tools records no view state, and the bundle
    /// says why rather than leaving the part out.
    func testAFreezeWithoutTheDrivingToolsDeclaresTheTraceAbsent() async throws {
        let shown = window()
        let freezer = ReportFreeze(window: { shown }, route: { "you" })

        let capture = try XCTUnwrap(freezer.freeze())

        XCTAssertEqual(capture.frame.width, 120)
        XCTAssertEqual(capture.frame.height, 200)
        XCTAssertNotNil(capture.frame.image, "the photograph does not decode")
        XCTAssertEqual(capture.route, "you")
        XCTAssertNil(capture.trace)
        XCTAssertEqual(capture.traceAbsent, "this build does not record view state")
        // With nothing running there is no dump, and the bundle says why
        // rather than leaving the part out.
        XCTAssertNil(capture.dump)
        let dump = await ReportAssembly.dumped(capture.dump)
        let upload = ReportAssembly.upload(
            from: capture, draft: ReportDraft(note: "wrong"), build: "amux-ios/test",
            dump: dump)
        let header = try XCTUnwrap(
            JSONSerialization.jsonObject(with: upload.header) as? [String: Any])
        let parts = try XCTUnwrap(header["parts"] as? [String: Any])
        XCTAssertEqual(parts["frame"] as? String, "present")
        let trace = (parts["trace"] as? [String: Any])?["absent"] as? [String: Any]
        XCTAssertEqual(trace?["reason"] as? String, "this build does not record view state")
        XCTAssertNil(upload.trace)
        XCTAssertEqual(upload.path, "", "no dump for the runtime to find")
    }

    /// The dump starts when the screen freezes, so it describes the moment
    /// somebody saw what they are reporting.
    func testTheDumpStartsWithTheFreeze() async throws {
        let shown = window()
        var started = 0
        let freezer = ReportFreeze(window: { shown }, dump: {
            started += 1
            return Task { .failure(PartAbsent("a test dumps nothing")) }
        })
        let capture = try XCTUnwrap(freezer.freeze())
        XCTAssertEqual(started, 1)
        let dump = await ReportAssembly.dumped(capture.dump)
        XCTAssertThrowsError(try dump.get())
    }

    func testNothingOnScreenIsNothingToReport() {
        let freezer = ReportFreeze(window: { nil })
        XCTAssertNil(freezer.freeze())
    }
}
