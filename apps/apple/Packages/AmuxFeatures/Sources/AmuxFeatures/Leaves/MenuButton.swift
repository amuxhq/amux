import AmuxDesign
import SwiftUI
import UIKit

/// One row of a ``MenuButton``'s menu.
struct MenuItem {
    let title: String
    var systemImage: String?
    /// A second line under the title.
    var subtitle: String?
    var destructive = false
    /// The current one of a set of choices, ticked.
    var chosen = false
    /// Listed but not choosable, such as a host that cannot be reached.
    var disabled = false
    let action: () -> Void
}

/// A control that opens a menu: its label drawn by SwiftUI, the menu presented
/// by a UIKit button of the app's own laid over it.
///
/// SwiftUI's `Menu` presents through a UIKit button it puts over its label, and
/// that button is what VoiceOver and the UI tests reach — without the name or
/// the identifier given to the menu, so it is read out as an unnamed button.
/// A button the app owns carries both. The drawn label is hidden from
/// accessibility, so the control is read once.
struct MenuButton<Label: View>: View {
    @Environment(\.reducesMotion) private var reduceMotion
    let name: String
    let identifier: String
    let items: [MenuItem]
    /// What it is set to, for a driver to read.
    var value: String? = nil
    /// Runs as the menu opens, before any row is chosen.
    var opened: () -> Void = {}
    @ViewBuilder let label: () -> Label

    @State private var pressed = false

    var body: some View {
        given
            .accessibilityHidden(true)
            .overlay {
                MenuTrigger(
                    name: name, identifier: identifier, items: items, opened: opened,
                    pressed: $pressed)
            }
            .reported(identifier, label: name, value: value)
    }

    /// Gives under the thumb the way every other discrete control does.
    @ViewBuilder private var given: some View {
        if pressed {
            if reduceMotion {
                label().opacity(0.55)
            } else {
                label().scaleEffect(0.96)
            }
        } else {
            label()
        }
    }
}

private struct MenuTrigger: UIViewRepresentable {
    let name: String
    let identifier: String
    let items: [MenuItem]
    let opened: () -> Void
    @Binding var pressed: Bool

    final class Coordinator {
        var opened: () -> Void = {}
        var pressed: (Bool) -> Void = { _ in }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    /// A button whose menu opens for an assistive technology's activation as
    /// it does for a tap. UIKit's own answer to an activation is to decline
    /// it and leave the client to fake a touch, which not every client does.
    final class Presenting: UIButton {
        override func accessibilityActivate() -> Bool {
            performPrimaryAction()
            return true
        }
    }

    func makeUIView(context: Context) -> Presenting {
        let button = Presenting(type: .custom)
        button.showsMenuAsPrimaryAction = true
        button.isAccessibilityElement = true
        button.accessibilityTraits = .button
        let coordinator = context.coordinator
        button.addAction(UIAction { _ in coordinator.pressed(true) }, for: .touchDown)
        for released: UIControl.Event in [.touchUpInside, .touchUpOutside, .touchCancel, .touchDragExit] {
            button.addAction(UIAction { _ in coordinator.pressed(false) }, for: released)
        }
        button.addAction(
            UIAction { _ in
                coordinator.pressed(false)
                coordinator.opened()
            },
            for: .menuActionTriggered)
        return button
    }

    func updateUIView(_ button: Presenting, context: Context) {
        let pressed = $pressed
        context.coordinator.opened = opened
        context.coordinator.pressed = { pressed.wrappedValue = $0 }
        button.accessibilityLabel = name
        button.accessibilityIdentifier = identifier
        button.menu = UIMenu(children: items.map { item in
            let action = UIAction(
                title: item.title, image: item.systemImage.flatMap { UIImage(systemName: $0) },
                attributes: UIMenuElement.Attributes(item.destructive ? [.destructive] : [])
                    .union(item.disabled ? [.disabled] : []),
                state: item.chosen ? .on : .off
            ) { _ in item.action() }
            action.subtitle = item.subtitle
            return action
        })
    }
}
