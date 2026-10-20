import AmuxDesign
import SwiftUI

/// The way back as a pushed page draws it in the system's bar: a chevron in
/// a circle of its own, as Messages and Slack draw it. A page that uses it
/// hides the system's back button, whose title would otherwise sit beside it.
public struct BarBack: View {
    @Environment(\.design) private var design
    let identifier: String
    let action: () -> Void

    public init(identifier: String, action: @escaping () -> Void) {
        self.identifier = identifier
        self.action = action
    }

    public var body: some View {
        Button(action: action) {
            Image(systemName: "chevron.left")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(design.ink.color)
                .frame(width: 36, height: 36)
                .barItemSurface()
        }
        .accessibilityLabel("Back")
        .identified(identifier, label: "Back")
    }
}
