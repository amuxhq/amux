import SwiftUI
import UIKit

/// A hold on the review's lines that then follows the finger, the way text
/// is selected: the list scrolls under a finger that moves first, and once
/// the hold is up the finger draws instead.
///
/// SwiftUI's own hold handed over to a drag does select, but on the scroll
/// view's content it keeps every swipe that starts on a line from scrolling
/// the page, whether attached as the content's gesture or beside the scroll
/// view's. UIKit's long press fails as soon as the finger moves, which
/// leaves the touch to the scroll view, and after it is recognized it goes
/// on reporting where the finger is until it lifts.
struct DiffSelection: UIGestureRecognizerRepresentable {
    /// How long the finger is held still before it draws.
    let hold: Double
    /// The coordinate space the finger's place is given in.
    let space: String
    /// The hold is up, with the finger here.
    let began: (CGPoint) -> Void
    /// The finger moved to here while drawing.
    let moved: (CGPoint) -> Void
    /// The finger lifted, or the system took the touch.
    let ended: () -> Void

    func makeUIGestureRecognizer(context: Context) -> UILongPressGestureRecognizer {
        let recognizer = UILongPressGestureRecognizer()
        recognizer.minimumPressDuration = hold
        recognizer.delegate = context.coordinator
        return recognizer
    }

    func makeCoordinator(converter: CoordinateSpaceConverter) -> Beside {
        Beside()
    }

    /// Keeps the hold from taking the touch from anything but the scroll
    /// view: a file's head is held for its menu, and a hold there that
    /// finds no line must not stop the menu opening.
    final class Beside: NSObject, UIGestureRecognizerDelegate {
        func gestureRecognizer(
            _ gestureRecognizer: UIGestureRecognizer,
            shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer
        ) -> Bool {
            !(other is UIPanGestureRecognizer)
        }
    }

    func handleUIGestureRecognizerAction(_ recognizer: UILongPressGestureRecognizer, context: Context) {
        switch recognizer.state {
        case .began: began(context.converter.location(in: .named(space)))
        case .changed: moved(context.converter.location(in: .named(space)))
        case .ended, .cancelled, .failed: ended()
        default: break
        }
    }
}
