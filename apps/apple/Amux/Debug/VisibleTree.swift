import Foundation
import UIKit

/// What is on screen, read from the accessibility tree.
///
/// The tree, and not the view hierarchy: a SwiftUI screen draws most of itself
/// into a handful of views, and its identifiers, labels and values live on
/// accessibility elements hanging off them. Reading the same tree a journey
/// drives and VoiceOver speaks means a door query cannot claim something is
/// reachable that a person could not reach.
enum VisibleTree {
    @MainActor
    static func elements(of window: UIWindow) -> [VisibleElement] {
        var found: [VisibleElement] = []
        var seen = Set<ObjectIdentifier>()
        walk(window, in: window, into: &found, seen: &seen)
        return found
    }

    /// The first element carrying an identifier, depth-first — the same order
    /// a query reports, so what a driver taps is what it just read.
    @MainActor
    static func find(_ identifier: String, in window: UIWindow) -> NSObject? {
        var found: NSObject?
        var seen = Set<ObjectIdentifier>()
        search(window, matching: identifier, into: &found, seen: &seen)
        return found
    }

    /// The smallest accessibility element on show whose frame holds
    /// `point`, in window coordinates. SwiftUI does not show an element's
    /// identifier to the process that drew it, so a control a screen
    /// declared is found again by where it was drawn.
    ///
    /// Only what a touch at that point reaches counts: the views on its way
    /// down to the view it lands on, and what that view draws. Retained
    /// screens that are not on show (a hidden tab, the page under a pushed
    /// one) still hold elements at the same point, and acting on one of
    /// those opens something the person never touched. Of what is on show,
    /// an element saying the declared label is preferred over a smaller one
    /// that does not.
    @MainActor
    static func element(at point: CGPoint, in window: UIWindow, saying label: String? = nil) -> NSObject? {
        guard let hit = window.hitTest(point, with: nil) else { return nil }
        var path = Set<ObjectIdentifier>()
        var step: UIView? = hit
        while let view = step {
            path.insert(ObjectIdentifier(view))
            step = view.superview
        }
        var best: (NSObject, CGFloat)?
        var said: (NSObject, CGFloat)?
        var seen = Set<ObjectIdentifier>()
        func visit(_ node: NSObject) {
            guard seen.insert(ObjectIdentifier(node)).inserted else { return }
            if let view = node as? UIView, !path.contains(ObjectIdentifier(view)),
               !view.isDescendant(of: hit) {
                return
            }
            if node.isAccessibilityElement {
                let frame = window.convert(node.accessibilityFrame, from: nil)
                let area = frame.width * frame.height
                if frame.contains(point), area > 0 {
                    if best.map({ area < $0.1 }) ?? true { best = (node, area) }
                    if let label, !label.isEmpty,
                       node.accessibilityLabel?.contains(label) == true,
                       said.map({ area < $0.1 }) ?? true {
                        said = (node, area)
                    }
                }
            }
            for child in children(of: node) { visit(child) }
        }
        visit(window)
        return said?.0 ?? best?.0
    }

    /// The one accessibility element on show saying exactly `label` that a
    /// touch at its middle reaches. A bar's button is rebuilt off screen when
    /// the bar changes, and the frame it last reported can be where it was
    /// then rather than where it is drawn; what it says still finds it.
    @MainActor
    static func element(saying label: String, in window: UIWindow) -> NSObject? {
        var saying: [NSObject] = []
        var seen = Set<ObjectIdentifier>()
        func visit(_ node: NSObject) {
            guard seen.insert(ObjectIdentifier(node)).inserted else { return }
            if let view = node as? UIView, view.isHidden || view.alpha == 0 { return }
            if node.isAccessibilityElement, node.accessibilityLabel == label { saying.append(node) }
            for child in children(of: node) { visit(child) }
        }
        visit(window)
        let reached = saying.filter { node in
            let frame = window.convert(node.accessibilityFrame, from: nil)
            return element(at: CGPoint(x: frame.midX, y: frame.midY), in: window, saying: label) === node
        }
        return reached.count == 1 ? reached.first : nil
    }

    /// The list a swipe would move: of the lists that scroll up and down and
    /// that a touch at their middle reaches, the one drawn last, which is the
    /// card or sheet on top. With none, the list under the middle of the
    /// window, whether or not it has anywhere to go.
    @MainActor
    static func list(in window: UIWindow) -> UIScrollView? {
        var found: UIScrollView?
        func visit(_ view: UIView) {
            if let list = view as? UIScrollView, !(list is UITextView), !list.isHidden,
               scrollsVertically(list), reached(list, in: window) {
                found = list
            }
            for subview in view.subviews { visit(subview) }
        }
        visit(window)
        if let found { return found }
        var view = window.hitTest(CGPoint(x: window.bounds.midX, y: window.bounds.midY), with: nil)
        while let candidate = view, !(candidate is UIScrollView) || candidate is UITextView {
            view = candidate.superview
        }
        return view as? UIScrollView
    }

    /// The innermost list on show whose content holds `point`, in window
    /// coordinates, whether that part of it is in sight or scrolled out of
    /// it.
    @MainActor
    static func list(holding point: CGPoint, in window: UIWindow) -> UIScrollView? {
        var found: UIScrollView?
        func visit(_ view: UIView) {
            if let list = view as? UIScrollView, !(list is UITextView), !list.isHidden,
               scrollsVertically(list), reached(list, in: window),
               list.convert(CGRect(origin: .zero, size: list.contentSize), to: window).contains(point) {
                found = list
            }
            for subview in view.subviews { visit(subview) }
        }
        visit(window)
        return found
    }

    /// The innermost scrolling view on show, in either direction, whose
    /// content holds `point`: a strip of cards scrolls sideways.
    @MainActor
    static func scroller(holding point: CGPoint, in window: UIWindow) -> UIScrollView? {
        var found: UIScrollView?
        func visit(_ view: UIView) {
            if let list = view as? UIScrollView, !(list is UITextView), !list.isHidden,
               reached(list, in: window),
               list.convert(CGRect(origin: .zero, size: list.contentSize), to: window).contains(point) {
                found = list
            }
            for subview in view.subviews { visit(subview) }
        }
        visit(window)
        return found
    }

    @MainActor
    private static func scrollsVertically(_ list: UIScrollView) -> Bool {
        let inset = list.adjustedContentInset
        return list.contentSize.height + inset.top + inset.bottom > list.bounds.height + 1
    }

    @MainActor
    private static func reached(_ list: UIScrollView, in window: UIWindow) -> Bool {
        let shown = list.convert(list.bounds, to: window).intersection(window.bounds)
        guard !shown.isNull, shown.width > 0, shown.height > 0 else { return false }
        return window.hitTest(CGPoint(x: shown.midX, y: shown.midY), with: nil)?
            .isDescendant(of: list) == true
    }

    @MainActor
    private static func walk(
        _ node: NSObject, in window: UIWindow,
        into found: inout [VisibleElement], seen: inout Set<ObjectIdentifier>
    ) {
        guard seen.insert(ObjectIdentifier(node)).inserted else { return }
        // A hidden view is not on show, and neither is anything it holds:
        // a list measures its rows in a hidden hosting view that would
        // otherwise report the last row it measured, at the list's origin.
        if let view = node as? UIView, view.isHidden || view.alpha == 0 { return }
        if let element = describe(node, in: window) { found.append(element) }
        for child in children(of: node) {
            walk(child, in: window, into: &found, seen: &seen)
        }
    }

    @MainActor
    private static func search(
        _ node: NSObject, matching identifier: String,
        into found: inout NSObject?, seen: inout Set<ObjectIdentifier>
    ) {
        guard found == nil, seen.insert(ObjectIdentifier(node)).inserted else { return }
        if let view = node as? UIView, view.isHidden || view.alpha == 0 { return }
        if (node as? any UIAccessibilityIdentification)?.accessibilityIdentifier == identifier {
            found = node
            return
        }
        for child in children(of: node) {
            search(child, matching: identifier, into: &found, seen: &seen)
        }
    }

    /// An element's accessibility children first, then the subviews it draws
    /// into. Both, because a hosting view has accessibility children and real
    /// subviews, and a control the app builds in UIKit has only subviews.
    @MainActor
    private static func children(of node: NSObject) -> [NSObject] {
        var children: [NSObject] = []
        if let listed = node.accessibilityElements as? [NSObject] {
            children.append(contentsOf: listed)
        } else {
            let count = node.accessibilityElementCount()
            if count != NSNotFound && count > 0 {
                for index in 0..<count {
                    if let child = node.accessibilityElement(at: index) as? NSObject {
                        children.append(child)
                    }
                }
            }
        }
        if let view = node as? UIView {
            children.append(contentsOf: subviews(of: view))
        }
        return children
    }

    /// A view's subviews in drawing order, except a list's cells, which go
    /// top to bottom: a list keeps its cells in the order it happened to
    /// make or reuse them, which changes with how the rows arrived, while
    /// VoiceOver reads them as they stand.
    @MainActor
    private static func subviews(of view: UIView) -> [UIView] {
        guard view is UICollectionView else { return view.subviews }
        var subviews = view.subviews
        let slots = subviews.indices.filter { subviews[$0] is UICollectionViewCell }
        let cells = slots.map { subviews[$0] }.sorted {
            ($0.frame.minY, $0.frame.minX) < ($1.frame.minY, $1.frame.minX)
        }
        for (slot, cell) in zip(slots, cells) { subviews[slot] = cell }
        return subviews
    }

    @MainActor
    private static func describe(_ node: NSObject, in window: UIWindow) -> VisibleElement? {
        let identifier = (node as? any UIAccessibilityIdentification)?.accessibilityIdentifier
        let label = node.accessibilityLabel
        // A view that names nothing and says nothing is scaffolding; reporting
        // it would bury the elements a journey actually asserts on.
        guard let identifier, !identifier.isEmpty else {
            guard let label, !label.isEmpty, node.isAccessibilityElement else { return nil }
            return element(node, identifier: "", label: label, in: window)
        }
        return element(node, identifier: identifier, label: label, in: window)
    }

    @MainActor
    private static func element(
        _ node: NSObject, identifier: String, label: String?, in window: UIWindow
    ) -> VisibleElement {
        let frame = if let view = node as? UIView {
            view.convert(view.bounds, to: window)
        } else {
            window.convert(node.accessibilityFrame, from: nil)
        }
        let enabled = if let control = node as? UIControl {
            control.isEnabled
        } else {
            !node.accessibilityTraits.contains(.notEnabled)
        }
        return VisibleElement(
            identifier: identifier,
            label: label.flatMap { $0.isEmpty ? nil : $0 },
            value: node.accessibilityValue.flatMap { $0.isEmpty ? nil : $0 },
            frame: VisibleFrame(
                x: frame.origin.x, y: frame.origin.y,
                width: frame.width, height: frame.height),
            enabled: enabled)
    }
}
