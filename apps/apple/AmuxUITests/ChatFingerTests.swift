import XCTest

/// The chat's list under a real finger. UIKit asks a list for its rows at
/// moments of its own while a drag moves it, which nothing but real touches
/// brings about: a test that sets the list's offset never sees them.
final class ChatFingerTests: XCTestCase {
    func testDraggingUpFromTheBottomStaysUpAndOffersTheWayBack() {
        let app = XCUIApplication()
        app.launchEnvironment["AMUX_FINGER_STAGE"] = "1"
        app.launch()

        let newest = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@", "short 196")).firstMatch
        XCTAssertTrue(newest.waitForExistence(timeout: 30), "the chat did not open at its newest rows")
        let toNewest = app.buttons["chat.toNewest"]
        XCTAssertFalse(toNewest.exists, "a chat at its newest row offers no way back to it")

        // In short strokes, slow and held at the end so that nothing but the
        // finger moves the list. Some stroke brings near the screen rows that
        // measure shorter than their estimate: reading that as the reader
        // having reached the bottom pulled the list back to it when the
        // finger left, and which stroke depends only on where the list
        // happens to ask for its rows.
        let top = app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.4))
        let bottom = app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.47))
        for stroke in 1...8 {
            let (label, before) = messageOnScreen(in: app)
            top.press(forDuration: 0.1, thenDragTo: bottom, withVelocity: XCUIGestureVelocity(200), thenHoldForDuration: 0.3)
            XCTAssertTrue(
                toNewest.waitForExistence(timeout: 5),
                "stroke \(stroke) left the list at the bottom under the reader")
            let moved = message(label, in: app).frame.minY - before
            XCTAssertTrue(
                (20...110).contains(moved),
                "stroke \(stroke) moved \(label) \(moved) pt under a drag of about 60")
        }

        toNewest.tap()
        XCTAssertTrue(toNewest.waitForNonExistence(timeout: 5), "going to the newest row left the way back on screen")
        XCTAssertTrue(newest.waitForExistence(timeout: 5), "going to the newest row did not reach it")
    }

    /// A message well inside the screen, by its words, and where it stands.
    /// The range a stroke may move it by is wide, as UIKit keeps the first
    /// points of a drag; a row measured under the finger moves it by hundreds.
    private func messageOnScreen(in app: XCUIApplication) -> (String, CGFloat) {
        let messages = app.descendants(matching: .any).matching(
            NSPredicate(format: "label BEGINSWITH %@ OR label BEGINSWITH %@", "short ", "message "))
        for element in messages.allElementsBoundByIndex {
            let top = element.frame.minY
            if (120...700).contains(top) { return (element.label, top) }
        }
        XCTFail("no message stands well inside the screen")
        return ("", 0)
    }

    private func message(_ label: String, in app: XCUIApplication) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label == %@", label)).firstMatch
    }
}
