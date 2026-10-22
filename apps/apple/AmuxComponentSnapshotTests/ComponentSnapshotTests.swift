import SnapshotTesting
import SwiftUI
import UIKit
import XCTest

@testable import Amux

@MainActor
final class ComponentSnapshotTests: XCTestCase {
    private let appearances: [Appearance] = [
        ("light", .light, .light),
        ("dark", .dark, .dark),
    ]

    /// The golden manifest's components are exactly this catalogue, each
    /// with a sentence saying what it shows, so the index a reviewer reads
    /// is never missing a picture or naming one that is gone.
    func testTheManifestSaysWhatEveryComponentShows() throws {
        let catalogIDs = ComponentCatalog.examples.map(\.id)
        XCTAssertFalse(catalogIDs.contains(where: \.isEmpty), "Component catalog IDs must not be empty")
        XCTAssertEqual(
            Set(catalogIDs).count,
            catalogIDs.count,
            "Component catalog IDs must be unique"
        )

        let manifest = try XCTUnwrap(
            Bundle(for: Self.self).url(forResource: "manifest", withExtension: "json"),
            "Goldens/manifest.json was not copied into the component snapshot test bundle"
        )
        let object = try JSONSerialization.jsonObject(with: Data(contentsOf: manifest))
        let root = try XCTUnwrap(object as? [String: Any])
        let components = try XCTUnwrap(root["components"] as? [[String: Any]])
        var described: [String] = []
        for component in components {
            let id = try XCTUnwrap(component["id"] as? String, "a component without an id")
            let shows = component["shows"] as? String ?? ""
            XCTAssertFalse(
                shows.trimmingCharacters(in: .whitespaces).isEmpty,
                "\(id) does not say what it shows"
            )
            described.append(id)
        }
        XCTAssertEqual(
            Set(described).subtracting(catalogIDs).sorted(), [],
            "The manifest describes components the catalogue no longer draws"
        )
        XCTAssertEqual(
            Set(catalogIDs).subtracting(described).sorted(), [],
            "The catalogue draws components the manifest does not describe"
        )
    }

    func testComponents() async throws {
        UIView.setAnimationsEnabled(false)
        defer { UIView.setAnimationsEnabled(true) }
        // A search field holding words draws its clear button on a Mac's
        // simulator and not on a CI runner's, with nothing in the example
        // to tell them apart; drawn always, the picture is the same on both.
        UISearchTextField.appearance().clearButtonMode = .always
        let environment = ProcessInfo.processInfo.environment
        let requested = Self.requestedIDs(environment["AMUX_SNAPSHOT_ONLY"])
        let available = Set(ComponentCatalog.examples.map(\.id))
        let unknown = requested.subtracting(available)
        XCTAssertTrue(
            unknown.isEmpty,
            "Unknown component snapshot IDs: \(unknown.sorted().joined(separator: ", ")). "
                + "Available IDs: \(available.sorted().joined(separator: ", "))"
        )
        guard unknown.isEmpty else { return }

        let examples = ComponentCatalog.examples.filter { requested.isEmpty || requested.contains($0.id) }
        XCTAssertFalse(examples.isEmpty, "The component catalog contains no selected examples")

        let recording = environment["AMUX_RECORD_SNAPSHOTS"] == "1"
        let perturbing = environment["AMUX_SNAPSHOT_PERTURB"] == "1"
        // Recording settles every example, so no baseline is ever an early
        // picture; asked for, an ordinary run does the same and fails any
        // example that changed without being marked to settle.
        let settlingAll = recording || environment["AMUX_SNAPSHOT_SETTLE_ALL"] == "1"
        let review = environment["AMUX_SNAPSHOT_REVIEW"].flatMap {
            $0.isEmpty ? nil : URL(fileURLWithPath: $0, isDirectory: true)
        }
        print(
            "AMUX_SNAPSHOT_CONFIGURATION selected="
                + (requested.isEmpty ? "all" : requested.sorted().joined(separator: ","))
                + " record=\(recording ? 1 : 0) perturb=\(perturbing ? 1 : 0)"
                + " review=\(review == nil ? 0 : 1)"
                + " settle-all=\(settlingAll ? 1 : 0)"
                + " host=\(environment["AMUX_COMPONENT_SNAPSHOTS"] == "1" ? 1 : 0)"
        )
        let suiteStarted = ProcessInfo.processInfo.systemUptime
        if let raw = environment["AMUX_SNAPSHOT_RUNNER_STARTED"], let runnerStarted = Double(raw) {
            print(String(format: "AMUX_SNAPSHOT_TIMING startup=%.3fs", suiteStarted - runnerStarted))
        }

        for example in examples {
            for appearance in appearances {
                let started = ProcessInfo.processInfo.systemUptime
                if let review {
                    await capture(example: example, appearance: appearance, into: review)
                } else {
                    await snapshot(
                        example: example,
                        appearance: appearance,
                        recording: recording,
                        perturbing: perturbing,
                        settlingAll: settlingAll
                    )
                }
                print(String(
                    format: "AMUX_SNAPSHOT_TIMING component=%@.%@ seconds=%.3f",
                    example.id,
                    appearance.name,
                    ProcessInfo.processInfo.systemUptime - started
                ))
            }
        }
        print(String(
            format: "AMUX_SNAPSHOT_TIMING batch=%d seconds=%.3f",
            examples.count * appearances.count,
            ProcessInfo.processInfo.systemUptime - suiteStarted
        ))
    }

    /// The example as the comparison sees it: every frosted surface drawn
    /// flat, as Reduce Transparency draws it. Liquid Glass and material are
    /// finished by the render server, out of the app's sight and on its own
    /// schedule (glass eases toward the luma behind it after reports that
    /// arrive when the render server sends them, and larger glass completes
    /// in a second pass), so a photograph of glass is not a fixed picture an
    /// exact comparison can hold. Glass is looked at by eye instead, in the
    /// review captures.
    private func snapshot(
        example: ComponentExample,
        appearance: Appearance,
        recording: Bool,
        perturbing: Bool,
        settlingAll: Bool
    ) async {
        let name = "\(example.id).\(appearance.name)"
        // An example at an accessibility text size is also read, as
        // VoiceOver reads it, for text drawn over text and controls off the
        // canvas; the tree it is read from exists only while something reads.
        let reading = example.dynamicTypeSize.isAccessibilitySize
        if reading { AccessibilityReading.on() }
        defer { if reading { AccessibilityReading.off() } }
        await showing(example, appearance, reducesTransparency: true, perturbing: perturbing) {
            controller, window, deadline in
            let traits = Self.traits(appearance)
            var picture: UIImage?
            if !example.settlesLate && !settlingAll {
                picture = await steadyPhotograph(of: controller.view, traits: traits)
                if picture == nil {
                    print("AMUX_SNAPSHOT_TIMING component=\(name) changed-unmarked")
                }
            }
            if picture == nil {
                guard let (settledPicture, changes) = await settledPhotograph(
                    of: controller.view, traits: traits, deadline: deadline, name: name)
                else {
                    XCTFail(
                        "Timed out waiting for \(name) to settle: its photographs were still changing "
                            + "5 s after it showed (plus its quiet window)"
                    )
                    return
                }
                if settlingAll && !example.settlesLate, let first = changes.first {
                    XCTFail(
                        "\(name) changed \(first) s after it reported ready, so an ordinary run would "
                            + "photograph it before it settles: mark it settlesLate in ComponentCatalog"
                    )
                    return
                }
                picture = settledPicture
            }
            guard let settled = picture else { return }
            if reading { await Self.assertReadable(name, in: window) }
            let frosted = Self.frostedLayers(in: window.layer)
            guard frosted == 0 else {
                XCTFail(
                    "\(name) holds \(frosted) Liquid Glass or material layer(s) when photographed: "
                        + "a surface drew glass or material without going through the "
                        + "reduce-transparency flag, and the comparison holds only flat pictures"
                )
                return
            }
            var strategy: Snapshotting<UIViewController, UIImage> = .image(
                drawHierarchyInKeyWindow: true,
                precision: 1,
                perceptualPrecision: 1,
                size: example.canvas,
                traits: Self.traits(appearance)
            )
            strategy.diffing = RoundingImageDiff.allowingRasterNoise(strategy.diffing, named: name)
            strategy.snapshot = { _ in Async(value: settled) }
            let failure = verifySnapshot(
                of: controller,
                as: strategy,
                named: name,
                record: recording ? .all : .never,
                file: #filePath,
                testName: "components"
            )
            if recording, failure?.contains("Record mode is on") == true {
                return
            }
            if let failure {
                XCTFail(failure)
            }
        }
    }

    /// A picture for a person to look at, never compared: the example with
    /// its glass and material as a phone draws them, written to `directory`
    /// as `<id>.<appearance>.png`. Only an example that wears a frosted
    /// surface is drawn, and the rule is what its window holds once it is
    /// ready: a layer of glass or material (see ``frostedLayers(in:)``). The
    /// picture is taken a fixed time later, longer than
    /// the render server has been seen to take to finish glass.
    private func capture(example: ComponentExample, appearance: Appearance, into directory: URL) async {
        let name = "\(example.id).\(appearance.name)"
        await showing(example, appearance, reducesTransparency: false, perturbing: false) {
            controller, window, _ in
            guard Self.frostedLayers(in: window.layer) > 0 else {
                print("AMUX_SNAPSHOT_REVIEW component=\(name) frosted=0")
                return
            }
            try? await Task.sleep(for: Self.glassFinishes)
            let image = photograph(controller.view, traits: Self.traits(appearance))
            let file = directory.appendingPathComponent("\(name).png")
            do {
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
                try XCTUnwrap(image.pngData()).write(to: file)
                print("AMUX_SNAPSHOT_REVIEW component=\(name) frosted=1")
            } catch {
                XCTFail("Could not write the review capture \(file.path): \(error)")
            }
        }
    }

    /// The longest the render server has been seen to take to finish glass
    /// after it first draws is under 2 s.
    private static let glassFinishes: Duration = .milliseconds(2500)

    /// Shows the example in a window of its own, on screen, waits until it
    /// reports ready, and hands `body` its controller, its window and the
    /// deadline for what follows.
    private func showing(
        _ example: ComponentExample,
        _ appearance: Appearance,
        reducesTransparency: Bool,
        perturbing: Bool,
        _ body: (UIViewController, UIWindow, TimeInterval) async -> Void
    ) async {
        let calendar: Calendar = {
            var value = Calendar(identifier: .gregorian)
            value.locale = Locale(identifier: "en_US_POSIX")
            value.timeZone = TimeZone(secondsFromGMT: 0)!
            return value
        }()
        let content = ComponentExampleView(
            example: example, appearance: appearance.colorScheme,
            reducesTransparency: reducesTransparency)
            .environment(\.locale, Locale(identifier: "en_US_POSIX"))
            .environment(\.calendar, calendar)
            .environment(\.timeZone, TimeZone(secondsFromGMT: 0)!)
            .environment(\.layoutDirection, .leftToRight)
            .transaction { $0.animation = nil }
        let rendered = AnyView(
            content.overlay(alignment: Alignment.leading) {
                if perturbing {
                    Rectangle().fill(Color(uiColor: .systemPink)).frame(width: 4)
                }
            }
        )
        let readiness = SnapshotReadiness()
        let controller = UIHostingController(rootView: ComponentReadinessReportingView(
            content: rendered,
            didChange: { readiness.elements = $0 }
        ))
        controller.overrideUserInterfaceStyle = appearance.interfaceStyle
        // The text size as the phone's own setting reaches a screen: a trait.
        // The example's environment value stops where UIKit hosts a piece of
        // it again, the system bar's items among them, which read the size
        // from their traits.
        controller.traitOverrides.preferredContentSizeCategory = UIContentSizeCategory(example.dynamicTypeSize)
        // The component is drawn without the window's safe area (the status
        // bar) and photographed where it stands, on screen, as a person sees
        // it; SnapshotTesting's own answer moves the view far off screen just
        // before drawing.
        controller.safeAreaRegions = []
        guard let windowScene = UIApplication.shared.connectedScenes
            .compactMap({ $0 as? UIWindowScene })
            .first
        else {
            XCTFail("The app-hosted snapshot test has no window scene")
            return
        }
        let previousKeyWindow = windowScene.windows.first(where: \.isKeyWindow)
        let window = UIWindow(windowScene: windowScene)
        window.frame = CGRect(origin: .zero, size: example.canvas)
        window.rootViewController = controller
        window.makeKeyAndVisible()
        defer {
            window.isHidden = true
            window.rootViewController = nil
            previousKeyWindow?.makeKey()
        }
        // UIKit containers inside (a navigation stack) read UIKit's safe area,
        // not SwiftUI's regions, so the window's inset is cancelled there too.
        controller.additionalSafeAreaInsets = UIEdgeInsets(
            top: -window.safeAreaInsets.top, left: -window.safeAreaInsets.left,
            bottom: -window.safeAreaInsets.bottom, right: -window.safeAreaInsets.right)
        controller.view.frame = window.bounds
        controller.view.layoutIfNeeded()
        let deadline = ProcessInfo.processInfo.systemUptime + 5
        let becameReady = await waitUntilReady(
            identifier: example.readinessIdentifier,
            value: example.readinessValue,
            readiness: readiness,
            controller: controller,
            deadline: deadline
        )
        guard becameReady else {
            XCTFail(
                "Timed out waiting for \(example.id) to report "
                    + "\(example.readinessIdentifier ?? "<identifier>")="
                    + "\(example.readinessValue ?? "<value>"); observed "
                    + readiness.description
            )
            return
        }
        controller.view.setNeedsLayout()
        controller.view.layoutIfNeeded()
        await body(controller, window, deadline)
    }

    private func waitUntilReady(
        identifier: String?,
        value: String?,
        readiness: SnapshotReadiness,
        controller: UIViewController,
        deadline: TimeInterval
    ) async -> Bool {
        guard let identifier, let value else { return true }
        repeat {
            controller.view.layoutIfNeeded()
            if readiness.contains(identifier: identifier, value: value) {
                return true
            }
            try? await Task.sleep(for: .milliseconds(10))
        } while ProcessInfo.processInfo.systemUptime < deadline
        return readiness.contains(identifier: identifier, value: value)
    }

    /// The picture of an example marked to settle late: the first
    /// photograph that stays unchanged for ``quiet``. Drawn flat, most
    /// examples are final at their first photograph. A few take one more
    /// change after they report ready, content that lands a pass later: an
    /// attachment chip replacing "Attaching", a focused field's caret
    /// appearing. On a CI runner that change has come up to 2.8 s after
    /// readiness, each one less than ``quiet`` after the one before.
    ///
    /// Waiting a quiet second for every example cost most of the batch, so
    /// only those marked `settlesLate` wait; a run asked to settle all of
    /// them (`--settle-all`, and every recording) is what proves no other
    /// example changes after it reports ready.
    ///
    /// The deadline bounds how long the example may take to become ready
    /// and stop changing; the quiet window that proves it stopped is added
    /// to it. When the photographs changed, counted from readiness, is
    /// printed for every example that changed at all.
    private func settledPhotograph(
        of view: UIView,
        traits: UITraitCollection,
        deadline: TimeInterval,
        name: String
    ) async -> (UIImage, [String])? {
        let started = ProcessInfo.processInfo.systemUptime
        var settled = photograph(view, traits: traits)
        var since = started
        var changes: [String] = []
        repeat {
            await DisplayFrame.pass()
            let current = photograph(view, traits: traits)
            let now = ProcessInfo.processInfo.systemUptime
            if Self.pixels(of: current) != Self.pixels(of: settled) {
                settled = current
                since = now
                changes.append(String(format: "%.3f", now - started))
            } else if now - since >= Self.quiet {
                if !changes.isEmpty {
                    print("AMUX_SNAPSHOT_TIMING component=\(name) changes=\(changes.joined(separator: ","))")
                }
                return (settled, changes)
            }
        } while ProcessInfo.processInfo.systemUptime < deadline + Self.quiet
        return nil
    }

    /// The picture of an example that is final once it reports ready: two
    /// photographs a whole display frame apart that agree. Nil when they
    /// differ, which an example not marked to settle should never do; the
    /// caller then settles it the slow way rather than compare a picture
    /// taken mid-change.
    private func steadyPhotograph(of view: UIView, traits: UITraitCollection) async -> UIImage? {
        let first = photograph(view, traits: traits)
        await DisplayFrame.pass()
        let second = photograph(view, traits: traits)
        return Self.pixels(of: first) == Self.pixels(of: second) ? second : nil
    }

    /// Over twice the longest wait seen for a flat example's late change,
    /// so a slower machine (a CI runner has taken over one and a half times
    /// as long as this Mac to draw) still shows it inside the window.
    private static let quiet: TimeInterval = 1

    /// How many layers under `layer` are a frosted surface's glass or
    /// material: a layer carrying Liquid Glass's filter, or a backdrop layer
    /// (what samples the content behind a material) that SwiftUI draws
    /// itself. A backdrop a UIKit view owns is left out: a list's scroll edge
    /// effect samples what is behind it too, and is no frosted surface.
    private static func frostedLayers(in layer: CALayer) -> Int {
        // A hidden layer is not in the picture: the tab bar keeps its glass
        // under a pushed page.
        guard !layer.isHidden, layer.opacity > 0 else { return 0 }
        let glass = (layer.filters ?? []).contains {
            ($0 as? NSObject)?.value(forKey: "name") as? String == "glassBackground"
        }
        let material = NSStringFromClass(type(of: layer)).contains("Backdrop") && !(layer.delegate is UIView)
        return (glass || material ? 1 : 0) + (layer.sublayers ?? []).reduce(0) { $0 + frostedLayers(in: $1) }
    }

    private static func traits(_ appearance: Appearance) -> UITraitCollection {
        UITraitCollection(traitsFrom: [
            UITraitCollection(displayScale: 3),
            UITraitCollection(displayGamut: .SRGB),
            UITraitCollection(layoutDirection: .leftToRight),
            UITraitCollection(userInterfaceStyle: appearance.interfaceStyle),
            UITraitCollection(accessibilityContrast: .normal),
        ])
    }

    /// Draws the view as SnapshotTesting's key-window strategy does.
    private func photograph(_ view: UIView, traits: UITraitCollection) -> UIImage {
        view.layoutIfNeeded()
        Self.holdCarets(in: view)
        return UIGraphicsImageRenderer(bounds: view.bounds, format: .init(for: traits)).image { _ in
            view.drawHierarchy(in: view.bounds, afterScreenUpdates: true)
        }
    }

    /// A focused field's caret blinks for as long as it is shown, so a screen
    /// with one would never settle, or would settle on either phase. It is
    /// held lit instead.
    private static func holdCarets(in view: UIView) {
        for case let display as UITextSelectionDisplayInteraction in view.interactions {
            display.cursorView.isBlinking = false
        }
        view.subviews.forEach(holdCarets)
    }

    /// Both photographs come from the same renderer format, so their bitmaps
    /// share a layout and equal bytes mean equal pixels. (PNG encodings of
    /// equal pixels are not always byte-identical.)
    private static func pixels(of image: UIImage) -> Data? {
        image.cgImage?.dataProvider?.data as Data?
    }

    /// At an accessibility text size, no piece of text is drawn over
    /// another and every control is whole on the canvas, or where its scroll
    /// view can bring it: what an example that wraps, stacks or scrolls
    /// instead of overprinting shows.
    private static func assertReadable(_ name: String, in window: UIWindow) async {
        var drawn: [ReadElement] = []
        for _ in 0..<50 {
            drawn = ReadElement.all(in: window)
            if !drawn.isEmpty { break }
            try? await Task.sleep(for: .milliseconds(20))
        }
        XCTAssertFalse(drawn.isEmpty, "\(name): nothing could be read from the accessibility tree")
        // A control's touch target reaches past what it draws, by up to the
        // slack that makes it a thumb's size; only past that is it off.
        for control in drawn where control.control {
            let canvas = control.reach.insetBy(dx: -12, dy: -12)
            XCTAssertTrue(
                canvas.contains(control.frame),
                "\(name): the control \"\(control.label)\" is not whole on the canvas: \(control.frame)")
        }
        let texts = drawn.filter { !$0.control && !$0.frame.isEmpty }
        print("AMUX_SNAPSHOT_READABLE component=\(name) texts=\(texts.count) controls=\(drawn.count - texts.count)")
        for (index, text) in texts.enumerated() {
            for other in texts.dropFirst(index + 1) {
                let shared = text.frame.intersection(other.frame)
                guard !shared.isNull, shared.width > 1, shared.height > 1 else { continue }
                XCTFail("\(name): \"\(text.label)\" is drawn over \"\(other.label)\" (\(text.frame) and \(other.frame))")
            }
        }
    }

    private static func requestedIDs(_ raw: String?) -> Set<String> {
        Set((raw ?? "").split(separator: ",").map(String.init).filter { !$0.isEmpty })
    }
}

private typealias Appearance = (name: String, colorScheme: ColorScheme, interfaceStyle: UIUserInterfaceStyle)

@MainActor
private final class SnapshotReadiness {
    var elements: [(identifier: String, value: String?)] = []

    func contains(identifier: String, value: String) -> Bool {
        elements.contains { $0.identifier == identifier && $0.value == value }
    }

    var description: String {
        elements.map { "\($0.identifier)=\($0.value ?? "<nil>")" }.joined(separator: ", ")
    }
}

/// Waits for a whole display frame: the first display-link callback only marks
/// the next frame boundary, the second comes one full frame after it.
@MainActor
private final class DisplayFrame: NSObject {
    private var continuation: CheckedContinuation<Void, Never>?
    private var ticks = 0

    static func pass() async {
        let frame = DisplayFrame()
        await withCheckedContinuation { continuation in
            frame.continuation = continuation
            CADisplayLink(target: frame, selector: #selector(tick(_:))).add(to: .main, forMode: .common)
        }
    }

    @objc private func tick(_ link: CADisplayLink) {
        ticks += 1
        guard ticks == 2 else { return }
        link.invalidate()
        continuation?.resume()
        continuation = nil
    }
}

/// One element VoiceOver would read, where it is drawn, in window points.
@MainActor
private struct ReadElement {
    let label: String
    let frame: CGRect
    let control: Bool
    /// Where the element can be brought into view: the canvas, or, inside a
    /// scroll view, everything it can scroll to as well. A row below the fold
    /// or a chip past the end of a sideways strip is whole there.
    let reach: CGRect

    static func all(in window: UIWindow) -> [ReadElement] {
        var found: [ReadElement] = []
        var seen = Set<ObjectIdentifier>()
        func visit(_ node: NSObject, reach: CGRect) {
            guard seen.insert(ObjectIdentifier(node)).inserted else { return }
            if let view = node as? UIView, view.isHidden || view.alpha == 0 { return }
            var reach = reach
            if let scroller = node as? UIScrollView {
                // A lazy stack reports a content size from the rows it has
                // laid out, and places the rest at estimates past it, so
                // whatever lies below the fold of a vertical scroller counts
                // as within reach.
                var size = scroller.contentSize
                if size.height > scroller.bounds.height { size.height = 1_000_000 }
                let content = CGRect(origin: .zero, size: size)
                    .inset(by: UIEdgeInsets(
                        top: -scroller.adjustedContentInset.top, left: -scroller.adjustedContentInset.left,
                        bottom: -scroller.adjustedContentInset.bottom, right: -scroller.adjustedContentInset.right))
                reach = reach.union(scroller.convert(content, to: window))
            }
            if node.isAccessibilityElement {
                let frame = window.convert(node.accessibilityFrame, from: nil)
                let traits = node.accessibilityTraits
                let control = traits.contains(.button) || traits.contains(.adjustable)
                    || node is UIControl || node is UITextField || node is UITextView
                if frame.width > 0, frame.height > 0 {
                    found.append(ReadElement(
                        label: node.accessibilityLabel ?? "", frame: frame, control: control, reach: reach))
                }
            }
            if let listed = node.accessibilityElements as? [NSObject] {
                listed.forEach { visit($0, reach: reach) }
            } else {
                let count = node.accessibilityElementCount()
                if count != NSNotFound, count > 0 {
                    for index in 0..<count {
                        if let child = node.accessibilityElement(at: index) as? NSObject { visit(child, reach: reach) }
                    }
                }
            }
            if let view = node as? UIView { view.subviews.forEach { visit($0, reach: reach) } }
        }
        visit(window, reach: window.bounds)
        return found
    }
}

/// The automation reading of the accessibility tree a UI test driver turns
/// on. A SwiftUI screen builds its tree only while something reads it.
@MainActor
private enum AccessibilityReading {
    private static let set: (@convention(c) (Int32) -> Void)? = {
        guard let handle = dlopen("/usr/lib/libAccessibility.dylib", RTLD_NOW),
              let symbol = dlsym(handle, "_AXSSetAutomationEnabled")
        else { return nil }
        return unsafeBitCast(symbol, to: (@convention(c) (Int32) -> Void).self)
    }()

    static func on() { set?(1) }
    static func off() { set?(0) }
}
