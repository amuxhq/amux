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
