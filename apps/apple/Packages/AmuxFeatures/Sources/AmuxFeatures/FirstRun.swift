import AmuxCore
import AmuxDesign
import SwiftUI

/// Why amux looks at the network this phone is on, said just before iOS
/// asks.
///
/// iOS asks for local network access the first time the app looks, once,
/// and a person who says no can only undo it in Settings. So the phone does
/// not look until the person has read why, here, and pressed the button
/// that starts it, which is what raises the system's question.
struct LocalNetworkPrimer: View {
    @Environment(\.design) private var design
    let identifier: String
    let look: @MainActor () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Explain(String(localized: "amux looks for your computers on this network, so iOS asks for local network access next."))
                .identified("\(identifier).primer")
            Button { look() } label: {
                ActionLabel("Look for Hosts", kind: .primary)
            }
            .buttonStyle(.amuxControl)
            .identified("\(identifier).look", label: "Look for Hosts")
        }
        .accessibilityElement(children: .contain)
    }
}

/// What amux sends, on the first screen, before it sends anything.
///
/// The phone's own installation holds every usage count until a client has
/// put this in front of somebody. Share Usage in Settings turns them off.
struct UsageNoticeLine: View {
    @Environment(\.design) private var design
    /// Told once the line is in view. The first-run screen scrolls, and on
    /// a small phone or at a large text size the line starts below the
    /// fold: drawn there it was not read, so it counts only once nearly all
    /// of it has been scrolled into view.
    let drawn: @MainActor () -> Void
    let details: @MainActor () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("amux sends usage counts, never content. Once you sign in, they’re linked to your account. Share Usage in Settings turns them off.")
                .designFont(.caption, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
                .identified("home.usageNotice")
            Button { details() } label: {
                Text("Details")
                    .designFont(.caption, design)
                    .fontWeight(.semibold)
                    .foregroundStyle(design.accent.color)
                    .thumbTarget(y: 14)
            }
            .buttonStyle(.amuxControl)
            .identified("home.usageNotice.details", label: "Details")
            .reclaimingThumbTarget(y: 14)
        }
        .onScrollVisibilityChange(threshold: 0.9) { visible in
            if visible { drawn() }
        }
    }
}

/// Where the usage counts are described: the same page the terminal's
/// notice names.
public enum UsageDetails {
    public static let page = URL(string: "https://github.com/amuxhq/amux/blob/main/docs/ANALYTICS.md")!
}
