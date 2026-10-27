import Foundation
import QuartzCore
import SwiftUI
import UIKit

/// The window a query or a capture is about.
enum DoorWindow {
    @MainActor
    static var current: UIWindow? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let windows = scenes.flatMap(\.windows)
        return windows.first(where: \.isKeyWindow) ?? windows.first
    }

    /// The scene's other visible windows, topmost first: what the system
    /// presents over the app, a menu among them, draws in a window of its
    /// own.
    @MainActor
    static var others: [UIWindow] {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let key = current
        return scenes.flatMap(\.windows)
            .filter { $0 !== key && !$0.isHidden && $0.alpha > 0 }
            .sorted { $0.windowLevel > $1.windowLevel }
    }

    /// Sets the text size as the phone's own setting reaches the app: as a
    /// trait of the scene, which every sheet, dialog and menu inherits. The
    /// root's environment does not reach a sheet, whose hosting reads the
    /// size from its traits again.
    @MainActor
    static func wear(_ size: DynamicTypeSize) {
        for scene in UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }) {
            scene.traitOverrides.preferredContentSizeCategory = UIContentSizeCategory(size)
        }
    }

    /// Whether the system is still moving something on screen: a sheet
    /// sliding, the keyboard rising, a page pushing. Those run as Core
    /// Animation animations the app's own drawing never shows, so a
    /// picture drawn by the app holds still while the display does not.
    @MainActor
    static var animating: Bool {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        return scenes.flatMap(\.windows).contains { !$0.isHidden && moving($0.layer) }
    }

    /// Only an animation that ends counts. Liquid Glass keeps animations
    /// of endless duration on its layers, tracking the views it sits on,
    /// for as long as it is on screen, and a working agent's motion repeats
    /// a finite period forever.
    @MainActor
    private static func moving(_ layer: CALayer) -> Bool {
        let ending = layer.animationKeys()?.contains { key in
            guard let animation = layer.animation(forKey: key) else { return false }
            return animation.duration.isFinite && animation.repeatCount.isFinite
                && animation.repeatDuration.isFinite
        } ?? false
        return ending || layer.sublayers?.contains(where: moving) ?? false
    }

    /// The first text input inside a view, which is what a SwiftUI text field
    /// actually is once it has been laid out.
    @MainActor
    static func textInput(in view: UIView) -> (any UIKeyInput & UIResponder)? {
        if let input = view as? (any UIKeyInput & UIResponder) { return input }
        for subview in view.subviews {
            if let found = textInput(in: subview) { return found }
        }
        return nil
    }

    /// Every text input in a view, in the order they are drawn.
    @MainActor
    static func allTextViews(in view: UIView) -> [UIView] {
        var found: [UIView] = []
        if view is any UIKeyInput { found.append(view) }
        for subview in view.subviews { found += allTextViews(in: subview) }
        return found
    }

    /// The search field on show, the one drawn last when a sheet's covers
    /// another's. A field under a hidden view is a screen that is not on
    /// show, such as another tab's.
    @MainActor
    static func searchField(in view: UIView) -> UISearchTextField? {
        guard !view.isHidden else { return nil }
        var found = view as? UISearchTextField
        for subview in view.subviews {
            if let deeper = searchField(in: subview) { found = deeper }
        }
        return found
    }

    /// The text input that already has the keyboard.
    ///
    /// A name declared on a SwiftUI screen reaches the accessibility tree as
    /// an element rather than as a view, so a field cannot always be found by
    /// the name the screen gave it. What has the keyboard can, and it is what
    /// a keystroke or a paste would reach anyway.
    @MainActor
    static func focused(in view: UIView) -> (any UIKeyInput & UIResponder)? {
        if view.isFirstResponder, let input = view as? (any UIKeyInput & UIResponder) {
            return input
        }
        for subview in view.subviews {
            if let found = focused(in: subview) { return found }
        }
        return nil
    }
}

/// Waiting for the display rather than for a duration: a fixed sleep is either
/// a flake on a slow machine or wasted seconds on a fast one.
enum DoorFrames {
    @MainActor
    static func next() async {
        await withCheckedContinuation { continuation in
            Waiter.wait(continuation)
        }
    }

    @MainActor
    private final class Waiter {
        private var link: CADisplayLink?
        private var continuation: CheckedContinuation<Void, Never>?
        private static var live: [Waiter] = []

        static func wait(_ continuation: CheckedContinuation<Void, Never>) {
            let waiter = Waiter()
            live.append(waiter)
            waiter.continuation = continuation
            let link = CADisplayLink(target: waiter, selector: #selector(fired))
            link.add(to: .main, forMode: .common)
            waiter.link = link
        }

        @objc private func fired() {
            link?.invalidate()
            link = nil
            let waiting = continuation
            continuation = nil
            Self.live.removeAll { $0 === self }
            waiting?.resume()
        }
    }
}

/// A PNG of the window drawn from inside the process.
///
/// This is how a report freezes what the person was looking at: a bug is
/// reported from a running app on a real phone, where nothing outside the
/// process can photograph the screen, so the app has to draw its own.
///
/// It is not how a golden is taken. Drawing the hierarchy asks every system
/// material on screen to resolve against this renderer rather than the one on
/// the display, and glass resolved that way is not stable from one pass to the
/// next; a golden is a photograph of the simulator's display taken from the
/// Mac instead. What that costs a report is a frame that may differ in its
/// glass from what the person saw, which is a fair trade for being able to
/// freeze the screen at all.
enum DoorCapture {
    /// Draws the window into an image.
    ///
    /// Rendering the hierarchy is itself what makes a system material resolve
    /// its backdrop for this renderer, and the first pass after an appearance
    /// change resolves it against the appearance before — a light screen comes
    /// back wearing the dark screen's plates. So a capture is this, repeated
    /// until two passes agree.
    @MainActor
    static func render(of window: UIWindow) -> UIImage? {
        let format = UIGraphicsImageRendererFormat()
        format.scale = window.traitCollection.displayScale
        format.opaque = true
        let renderer = UIGraphicsImageRenderer(bounds: window.bounds, format: format)
        var drew = false
        let image = renderer.image { _ in
            drew = window.drawHierarchy(in: window.bounds, afterScreenUpdates: true)
        }
        return drew ? image : nil
    }

    @MainActor
    static func write(_ image: UIImage, _ data: Data, to path: String) -> DoorReply {
        let url = URL(fileURLWithPath: path)
        do {
            try FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try data.write(to: url)
        } catch {
            return .error("capture failed: \(error.localizedDescription)")
        }
        let scale = image.scale
        return .captured(
            path: path,
            width: Int((image.size.width * scale).rounded()),
            height: Int((image.size.height * scale).rounded()),
            scale: Int(scale.rounded()))
    }
}
