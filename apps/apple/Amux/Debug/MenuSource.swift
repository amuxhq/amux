import UIKit

/// The controls that present a menu, and the actions their menus hold.
enum MenuSource {
    /// The control whose menu offers an item titled `label`, and that item.
    @MainActor
    static func item(_ label: String, in view: UIView) -> (UIButton, UIAction)? {
        if let button = view as? UIButton, let menu = button.menu,
           let action = action(label, in: menu) {
            return (button, action)
        }
        for subview in view.subviews {
            if let found = item(label, in: subview) { return found }
        }
        return nil
    }

    private static func action(_ label: String, in menu: UIMenu) -> UIAction? {
        for child in menu.children {
            if let action = child as? UIAction, action.title == label { return action }
            if let nested = child as? UIMenu, let found = action(label, in: nested) { return found }
        }
        return nil
    }

    /// Runs an action as the menu would when its row is tapped.
    @MainActor
    static func run(_ action: UIAction, from button: UIButton) {
        let perform = NSSelectorFromString("performWithSender:target:")
        guard action.responds(to: perform) else { return }
        _ = action.perform(perform, with: button, with: nil)
    }
}

/// The system's dialogs: a confirmation dialog or an alert, which SwiftUI
/// presents as a `UIAlertController`.
enum DialogSource {
    /// The dialog on screen, the one presented last.
    @MainActor
    static func shown(in window: UIWindow) -> UIAlertController? {
        var top = window.rootViewController
        var found: UIAlertController?
        while let presented = top?.presentedViewController {
            if let alert = presented as? UIAlertController { found = alert }
            top = presented
        }
        return found
    }

    /// Presses the dialog's button titled `label`.
    ///
    /// A dialog's buttons do not answer an activation from inside the
    /// process, as a presented menu's rows do not, so the button's own
    /// handler is run, after the dialog is put away, which is the order a
    /// tap runs it in.
    @MainActor
    static func press(_ label: String, in alert: UIAlertController) -> Bool {
        typealias Handler = @convention(block) (UIAlertAction) -> Void
        guard let action = alert.actions.first(where: { $0.title == label }) else { return false }
        guard let block = action.value(forKey: "handler") else {
            alert.dismiss(animated: true)
            return true
        }
        let handler = unsafeBitCast(block as AnyObject, to: Handler.self)
        alert.dismiss(animated: true) { handler(action) }
        return true
    }
}
