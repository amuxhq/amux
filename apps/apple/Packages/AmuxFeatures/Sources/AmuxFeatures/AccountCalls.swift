import AmuxCore
import AmuxDesign
import SwiftUI

/// The two things an account is ever offered for, written once.
///
/// They are not interchangeable and they are never both on screen. Signing in
/// is what makes your machines findable when you are not on their network; a
/// subscription is what carries agents through the relay to them. Neither is
/// what amux is for — a phone on the same network as its machines needs
/// neither — so both are worded as the one thing they add rather than as
/// something missing, and both appear only where something on screen is
/// actually out of reach without them.
///
/// Written as views rather than as strings because the wording and the button
/// belong together: every place that says this also has to offer the way out
/// of it, and two places that drifted apart would end up selling different
/// things.

/// The words of the subscribe offer, apart from the view that draws them.
///
/// Two things say them: the view below, and the conversation's foot state,
/// which is an ordinary value with no screen behind it. A `View` and its static
/// members are main-actor isolated, so the sentences live out here rather than
/// becoming a second copy on the other side of the actor.
public enum SubscribeCopy {
    /// The offer in the words of what it buys. Not "subscription required":
    /// that names a state of the account rather than a thing a person wanted
    /// to do, and what they wanted to do is reach this machine from here.
    public static let headline = "Reach your agents from anywhere"

    /// Why the machine somebody is looking at cannot be used, and what is true
    /// about the ones that can.
    public static func detail(host: String?) -> String {
        guard let host else {
            return """
                The relay can see this host and cannot carry agents to it on this account. \
                A subscription opens the tunnel. Hosts on this network never need one.
                """
        }
        return """
            The relay can see \(host) and cannot carry its agents to this phone. A \
            subscription opens the tunnel. Hosts on this network never need one.
            """
    }
}

/// Reaching machines the relay can see and this account may not tunnel to.
///
/// It stands where the composer would be in an away machine's conversation,
/// and where the keypad's caption would be when a code authenticates against a
/// machine only the relay has seen. Both are the same moment: somebody is
/// trying to use a machine, and the one thing in the way is the relay.
public struct SubscribeCallToAction: View {
    @Environment(\.design) private var design
    private let host: String?
    private let identifier: String
    private let subscribe: @MainActor () -> Void

    public init(
        host: String?, identifier: String = "subscribe",
        subscribe: @escaping @MainActor () -> Void
    ) {
        self.host = host
        self.identifier = identifier
        self.subscribe = subscribe
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                Text(SubscribeCopy.headline)
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                    .fixedSize(horizontal: false, vertical: true)
                Text(detail)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Button(action: subscribe) {
                ActionLabel("Subscribe", kind: .primary, fill: true)
            }
            .buttonStyle(.amuxControl)
            .accessibilityLabel("Subscribe")
            .identified("\(identifier).buy", label: "Subscribe")
        }
        .accessibilityElement(children: .contain)
        .identified(identifier, label: SubscribeCopy.headline, value: detail)
    }

    private var detail: String { SubscribeCopy.detail(host: host) }
}
