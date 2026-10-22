import Foundation
import XCTest

/// Every control this app draws, checked for the two things a control has to
/// have before anybody can use it without looking at it: a name VoiceOver can
/// read out, and enough of it to hit.
///
/// It is a UI test rather than something the app asks itself, and it has to
/// be. SwiftUI builds an accessibility tree only for an attached accessibility
/// client, and an app is not one from inside its own process: asked from in
/// there, every control answers with no traits at all and nothing can be told
/// from a label. XCUITest is that client, so the element kind, the name and
/// the rectangle read here are the ones VoiceOver and a thumb would get.
///
/// The pages it audits are live ones, reached the way a person reaches them:
/// `scripts/ios-accessibility.py` serves machines with agents that act, pairs
/// the app with one by its code and taps through to each page, then asks this
/// test to audit what is on screen. The two take turns through files in a
/// directory both can see, named in `AMUX_AUDIT_DIRECTORY`: the driver writes
/// `page-N.json` saying which page is up and where the app's door listens,
/// this test audits it and answers `page-N.done`, and `end` says there are no
/// more.
///
/// Two kinds of thing are judged on their names and not on their size. The
/// first is an item in the system's navigation bar, which the bar lays out
/// and answers to itself: the slot it gives an item is 36 points tall, the
/// glass it draws round it 44, and only the slot can be measured from here.
///
/// The second is a link the markdown parser made out of a run of an agent's
/// prose. That is not a control the app draws. The app draws a paragraph; the
/// parser turns a span of a sentence inside it into something tappable, laid
/// out as part of a line and split across two of them when the line wraps.
/// There is no rectangle to grow, and its height is the height of the prose
/// around it. WCAG's target-size rule carves out inline targets in a sentence
/// for exactly this reason, so the exception here is the standard's rather
/// than this app's. It is kept narrow:
/// only a link with no identifier of its own, sitting inside a block of agent
/// prose, and only its size is excused — it must still say something VoiceOver
/// can read out. Each one excused is written into the record with the state it
/// was found in, its label and its URL, and counted in the run's summary line,
/// so a small tappable thing nobody expected is reported rather than lost.
final class AccessibilityAuditTests: XCTestCase {
    /// The smallest a control may be, in points, in either direction. Apple's
    /// own number, and the one a thumb actually needs.
    static let smallest: CGFloat = 44
    /// What placing a rectangle on the display's pixels can take off it:
    /// a 44-point row comes back a hair under 44.
    static let pixelSnap: CGFloat = 0.5

    /// Controls that are smaller than that on purpose, by identifier prefix,
    /// each with the reason.
    ///
    /// Nothing is here to make the audit quiet. A control belongs here only
    /// when its hit area is larger than its drawing and the tree reports the
    /// drawing — which the system's own keyboard and the status bar both do,
    /// and neither is this app's to change.
    static let notOurs = ["UIKeyboard", "Key.", "com.apple."]

    /// What the screen calls a block of an agent's own markdown. A link with
    /// no name of its own found inside one of these was made by the parser out
    /// of a run of a sentence, not drawn by this app as a control.
    static let proseBlocks = ["transcript.prose"]

    /// The app the driver launched and navigates; this test only reads it.
    static let bundle = "sh.amux.app"

    /// How long the driver may take to bring the next page up: launching the
    /// app, pairing a machine and waiting on its agents come first.
    static let patience: TimeInterval = 600

    func testEveryControlIsNamedAndBigEnoughToHit() throws {
        let path = try XCTUnwrap(
            ProcessInfo.processInfo.environment["AMUX_AUDIT_DIRECTORY"],
            "the audit audits live pages the driver reaches: run it with just ios accessibility")
        let directory = URL(fileURLWithPath: path, isDirectory: true)
        try Data().write(to: directory.appendingPathComponent("ready"))

        var pages: [String] = []
        var audited = 0
        var complaints: [String] = []
        var inlineLinks: [[String: String]] = []
        while let page = try next(pages.count, in: directory) {
            guard let name = page["page"] as? String, let port = page["port"] as? Int else {
                throw DoorLines.Failure("page \(pages.count) does not say its name and port: \(page)")
            }
            let app = XCUIApplication(bundleIdentifier: Self.bundle)
            app.activate()
            let door = try DoorLines(address: "127.0.0.1:\(port)")
            _ = try door.ask(["kind": "settle"])
            let laidOut = try laidOut(door)
            let barItems = Self.barItems(app)
            var found = 0
            for control in controls(app) {
                found += 1
                let inline = inlineProseLink(control, laidOut: laidOut)
                if inline {
                    inlineLinks.append([
                        "state": name,
                        "label": control.label,
                        "url": control.identifier,
                        "size": rectangle(control.frame),
                    ])
                }
                let inBar = !control.identifier.isEmpty && barItems.contains(control.identifier)
                complaints += fault(
                    control, on: name, laidOut: laidOut, judgeSize: !inline && !inBar)
            }
            XCTAssertGreaterThan(found, 0, "\(name) has no controls at all")
            audited += found
            try JSONSerialization.data(withJSONObject: ["controls": found])
                .write(to: directory.appendingPathComponent("page-\(pages.count).done"))
            pages.append(name)
        }

        XCTAssertFalse(pages.isEmpty, "the driver brought no page up")
        record["states"] = pages.count
        record["pages"] = pages
        record["controls"] = audited
        record["faults"] = complaints
        record["inlineLinks"] = inlineLinks
        write(record, to: "accessibility-audit.json")
        XCTAssertTrue(
            complaints.isEmpty,
            "\(complaints.count) of \(audited) controls across \(pages.count) pages are "
            + "unusable without sight or without a steady thumb:\n" + complaints.joined(separator: "\n"))
    }

    /// The page the driver brought up next, or nothing once it says it is
    /// done.
    private func next(_ index: Int, in directory: URL) throws -> [String: Any]? {
        let page = directory.appendingPathComponent("page-\(index).json")
        let end = directory.appendingPathComponent("end")
        let deadline = Date().addingTimeInterval(Self.patience)
        while Date() < deadline {
            if let data = try? Data(contentsOf: page),
               let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                return object
            }
            if FileManager.default.fileExists(atPath: end.path) { return nil }
            Thread.sleep(forTimeInterval: 0.2)
        }
        throw DoorLines.Failure("the driver brought no page \(index) up within \(Int(Self.patience)) s")
    }

    // MARK: - Reading the tree

    /// Everything on screen a person can press, as the accessibility client
    /// sees it.
    ///
    /// Buttons and the things that behave like them. An element with no
    /// rectangle is not on screen — a row scrolled out of a list is still in
    /// the tree — and judging one of those on its size would be judging it on
    /// nothing. Nor is one a sheet covers, which stays in the tree under it:
    /// it is judged on the page where it is on top.
    private func controls(_ app: XCUIApplication) -> [XCUIElement] {
        let kinds: [XCUIElement.ElementType] = [.button, .switch, .link]
        return kinds.flatMap { kind in
            app.descendants(matching: kind).allElementsBoundByIndex.filter { control in
                control.exists && control.frame.width > 0 && control.frame.height > 0
                    && control.isHittable
                    && !Self.notOurs.contains { control.identifier.hasPrefix($0) }
            }
        }
    }

    /// Where the screen laid each named thing out, in window points, asked of
    /// the screen itself.
    ///
    /// A rectangle has to come from here rather than from XCUITest. The frame
    /// XCUITest hands back is the accessibility frame, and that is not the
    /// frame the screen laid out: a round control drawn at 44x44 comes back
    /// 42x42, and a control whose name is declared on the button comes back as
    /// the line of text inside it, tens of points shorter than the button
    /// around it. Judging a hit area on either of those judges a rectangle
    /// nobody laid out. What only XCUITest can say — whether a thing is a
    /// button at all, and what VoiceOver would read out — is still read from
    /// over there.
    ///
    /// A name can be laid out more than once on a page, so every rectangle
    /// under a name is kept and the one covering the control is chosen later.
    private func laidOut(_ door: DoorLines) throws -> [String: [CGRect]] {
        let answered = try door.ask(["kind": "query"])
        let elements = (answered["state"] as? [String: Any])?["elements"] as? [[String: Any]] ?? []
        var found: [String: [CGRect]] = [:]
        for element in elements {
            guard let identifier = element["identifier"] as? String, !identifier.isEmpty,
                  let frame = element["frame"] as? [String: Any],
                  let x = frame["x"] as? Double, let y = frame["y"] as? Double,
                  let width = frame["width"] as? Double, let height = frame["height"] as? Double
            else { continue }
            found[identifier, default: []].append(
                CGRect(x: x, y: y, width: width, height: height))
        }
        return found
    }

    /// The rectangle to judge one control on: the laid-out one the screen
    /// declared under that name, where the screen declared one big enough to
    /// hold what XCUITest found, and XCUITest's own where it did not.
    ///
    /// Size and not position, because the two do not have to agree on where a
    /// control is and are both right. A screen that slides sideways — a page
    /// being pushed — is moved by a transform the layout
    /// never sees, so the screen's declaration stays at the untranslated
    /// place while XCUITest reports where it ended up. What a thumb needs is
    /// the size either way.
    ///
    /// The smallest declaration that could hold the control is the one taken,
    /// which is what keeps a name declared once per row honest: a short row
    /// among tall ones is judged on the short one.
    private func judged(_ control: XCUIElement, laidOut: [String: [CGRect]]) -> CGRect {
        let frame = control.frame
        let holding = (laidOut[control.identifier] ?? []).filter {
            $0.width >= frame.width && $0.height >= frame.height
        }
        return holding.min(by: { area($0) < area($1) }) ?? frame
    }

    private func area(_ rectangle: CGRect) -> CGFloat {
        rectangle.isNull ? 0 : rectangle.width * rectangle.height
    }

    /// What the system's navigation bar holds: back, close, a title, a menu.
    /// They are judged on their names and not on their size. The bar lays
    /// each one out in a slot 36 points tall, which is what the app can
    /// measure, and draws and answers to a 44-point circle around it.
    private static func barItems(_ app: XCUIApplication) -> Set<String> {
        let kinds: [XCUIElement.ElementType] = [.button, .switch, .link]
        return Set(kinds.flatMap { kind in
            app.navigationBars.descendants(matching: kind).allElementsBoundByIndex.map { $0.identifier }
        })
    }

    /// Whether this is a link the markdown parser made inside a run of an
    /// agent's prose, which is judged on its name but not on its size.
    ///
    /// Three things at once, so nothing else can slip through: it has to be a
    /// link, it has to carry no identifier the screen declared — the app names
    /// every control it draws, and XCUITest falls back to reporting a bare
    /// link's URL as its identifier — and it has to sit inside a rectangle the
    /// screen declared as a block of agent markdown. Containment rather than
    /// ancestry because the prose block combines its children for VoiceOver,
    /// which lifts the links inside it out to the top of the tree.
    private func inlineProseLink(
        _ control: XCUIElement, laidOut: [String: [CGRect]]
    ) -> Bool {
        guard control.elementType == .link, laidOut[control.identifier] == nil else { return false }
        let frame = control.frame
        guard !frame.isNull, !frame.isEmpty else { return false }
        return Self.proseBlocks
            .flatMap { laidOut[$0] ?? [] }
            .contains { $0.insetBy(dx: -1, dy: -1).contains(frame) }
    }

    /// What is wrong with one control, said in words a person can act on.
    private func fault(
        _ control: XCUIElement, on state: String, laidOut: [String: [CGRect]],
        judgeSize: Bool
    ) -> [String] {
        let named = control.identifier.isEmpty ? "an unnamed control" : control.identifier
        var faults: [String] = []
        if control.label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            faults.append(
                "\(state): \(named) has nothing for VoiceOver to read out; it is at "
                + "\(rectangle(control.frame))")
        }
        guard judgeSize else { return faults }
        let frame = judged(control, laidOut: laidOut)
        let least = Self.smallest - Self.pixelSnap
        if frame.width < least || frame.height < least {
            faults.append(
                "\(state): \(named) (\"\(control.label)\") is \(rectangle(frame)), under the "
                + "\(Int(Self.smallest)) pt a thumb needs")
        }
        return faults
    }

    /// Whole points as they are and anything else to a tenth, so a control
    /// a fraction under the limit does not read as exactly on it.
    private func rectangle(_ frame: CGRect) -> String {
        func points(_ value: CGFloat) -> String {
            value == value.rounded() ? String(format: "%.0f", value) : String(format: "%.1f", value)
        }
        return "\(points(frame.width))×\(points(frame.height)) pt at (\(points(frame.minX)), \(points(frame.minY)))"
    }

    // MARK: - What it leaves behind

    private var record: [String: Any] = [:]

    private func write(_ record: [String: Any], to name: String) {
        let url = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent(name)
        guard let data = try? JSONSerialization.data(
            withJSONObject: record, options: [.prettyPrinted, .sortedKeys]) else { return }
        try? data.write(to: url)
    }
}
