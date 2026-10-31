import Foundation
import XCTest
@testable import AmuxCore

/// A freeze that answers with a picture it was told to answer with, and counts
/// how many times it was asked. Nothing here photographs anything: what the
/// store has to get right is *when* it asks, not what comes back.
@MainActor
private final class OneFreeze: ReportFreezing {
    var answer: ReportCapture?
    private(set) var asked = 0

    init(_ answer: ReportCapture?) {
        self.answer = answer
    }

    func freeze() -> ReportCapture? {
        asked += 1
        return answer
    }
}

@MainActor
private func capture(_ route: String) -> ReportCapture {
    ReportCapture(
        frame: FrozenFrame(png: Data([0x89, 0x50]), width: 402, height: 874, scale: 3),
        route: route)
}

final class ReportStoreTests: XCTestCase {
    /// The screenshot path: the picture is taken before the offer appears, so
    /// nothing the person does about reporting can be in it.
    @MainActor
    func testTheScreenIsFrozenBeforeTheOfferIsMade() {
        let freeze = OneFreeze(capture("run"))
        let reports = ReportStore()
        XCTAssertFalse(reports.offering)
        XCTAssertNil(reports.capture)

        XCTAssertTrue(reports.offer(freeze))
        XCTAssertEqual(freeze.asked, 1)
        XCTAssertTrue(reports.offering)
        XCTAssertEqual(reports.capture?.route, "run")
        XCTAssertFalse(reports.open)
    }

    /// Taking the offer opens the report on the frame that was already frozen.
    /// A second freeze here would be a picture of the offer.
    @MainActor
    func testTakingTheOfferPhotographsNothingFurther() {
        let freeze = OneFreeze(capture("run"))
        let reports = ReportStore()
        reports.offer(freeze)

        reports.accept()

        XCTAssertEqual(freeze.asked, 1)
        XCTAssertTrue(reports.open)
        XCTAssertFalse(reports.offering)
        XCTAssertEqual(reports.capture?.route, "run")
    }

    /// The system's own preview slides over the app straight afterwards and
    /// takes the person out of it. Coming back finds the same offer over the
    /// same frozen frame: the capture is the store's, not a screen's.
    @MainActor
    func testTheOfferOutlivesBeingCoveredUp() {
        let freeze = OneFreeze(capture("hosts"))
        let reports = ReportStore()
        reports.offer(freeze)

        // Nothing is told about the app going away, which is the point: there
        // is no lifecycle path that clears this, so nothing to drive here.
        XCTAssertTrue(reports.offering)
        XCTAssertEqual(reports.capture?.route, "hosts")
    }

    /// A second screenshot while a report is already in hand changes nothing.
    /// Swapping the picture under somebody mid-report would lose what they had
    /// already said about it.
    @MainActor
    func testASecondScreenshotDoesNotReplaceTheFirst() {
        let freeze = OneFreeze(capture("run"))
        let reports = ReportStore()
        reports.offer(freeze)
        freeze.answer = capture("you")

        XCTAssertFalse(reports.offer(freeze))
        XCTAssertEqual(freeze.asked, 1)
        XCTAssertEqual(reports.capture?.route, "run")
    }

    /// Help freezes and opens in one go: somebody who went looking for the row
    /// has already said yes, so there is no offer in between.
    @MainActor
    func testHelpFreezesTheScreenBehindItAndOpensStraightAway() {
        let freeze = OneFreeze(capture("you"))
        let reports = ReportStore()

        XCTAssertTrue(reports.begin(freeze))

        XCTAssertEqual(freeze.asked, 1)
        XCTAssertTrue(reports.open)
        XCTAssertFalse(reports.offering)
        XCTAssertEqual(reports.capture?.route, "you")
    }

    /// Turning it down lets go of the picture. An accidental screenshot must
    /// not leave the app holding a photograph of somebody's screen.
    @MainActor
    func testTurningTheOfferDownLetsGoOfThePicture() {
        let freeze = OneFreeze(capture("run"))
        let reports = ReportStore()
        reports.offer(freeze)

        reports.dismiss()

        XCTAssertFalse(reports.offering)
        XCTAssertFalse(reports.open)
        XCTAssertNil(reports.capture)
    }

    /// A screen that could not be photographed makes no offer at all, rather
    /// than an offer that leads to an empty report.
    @MainActor
    func testNothingIsOfferedWhenTheScreenCouldNotBePhotographed() {
        let freeze = OneFreeze(nil)
        let reports = ReportStore()

        XCTAssertFalse(reports.offer(freeze))
        XCTAssertFalse(reports.begin(freeze))
        XCTAssertFalse(reports.offering)
        XCTAssertNil(reports.capture)
    }
}
