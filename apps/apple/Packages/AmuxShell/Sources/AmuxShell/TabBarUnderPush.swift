import SwiftUI
import UIKit

extension View {
    /// Hides the tab bar under a page pushed onto a tab's stack, the way
    /// UIKit does it: the bar leaves with the page sliding in and comes back
    /// with it sliding out, following a back swipe's finger. SwiftUI's own
    /// `.toolbar(.hidden, for: .tabBar)` brought the bar back only once the
    /// pop had finished, so it appeared a beat after the list it belongs to.
    func hidesTabBarWhenPushed() -> some View {
        background(TabBarUnderPush().frame(width: 0, height: 0))
    }
}

/// Marks the controller its page is pushed as. SwiftUI puts this probe's
/// controller inside the one hosting the page, so it climbs to the child of
/// the navigation controller and sets the flag UIKit reads at each push and
/// pop.
private struct TabBarUnderPush: UIViewControllerRepresentable {
    func makeUIViewController(context: Context) -> Probe { Probe() }
    func updateUIViewController(_ probe: Probe, context: Context) {}

    final class Probe: UIViewController {
        override func willMove(toParent parent: UIViewController?) {
            super.willMove(toParent: parent)
            mark(from: parent)
        }

        override func didMove(toParent parent: UIViewController?) {
            super.didMove(toParent: parent)
            mark(from: parent)
        }

        override func viewWillAppear(_ animated: Bool) {
            super.viewWillAppear(animated)
            mark(from: parent)
        }

        private func mark(from start: UIViewController?) {
            var controller = start
            while let current = controller {
                if current.parent is UINavigationController {
                    current.hidesBottomBarWhenPushed = true
                    return
                }
                controller = current.parent
            }
        }
    }
}
