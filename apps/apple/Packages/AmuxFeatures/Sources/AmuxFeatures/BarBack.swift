import AmuxDesign
import SwiftUI

/// The way back as a pushed page draws it in the system's bar: a chevron in
/// a circle of its own, as Messages and Slack draw it. A page that uses it
/// hides the system's back button, whose title would otherwise sit beside it.
public struct BarBack: View {
    let identifier: String
    let action: () -> Void

    public init(identifier: String, action: @escaping () -> Void) {
        self.identifier = identifier
        self.action = action
    }

    public var body: some View {
        BarButton("chevron.left", label: String(localized: "Back"), identifier: identifier, action: action)
    }
}

/// A symbol in a circle of its own in the system's bar: back, cancel, done.
/// The system's role buttons draw their own glass whatever the app's
/// less-transparency setting says; this one stands on the bar's glass, or on
/// a flat circle when less transparency is asked for, as every other item in
/// the app's bars does.
public struct BarButton: View {
    @Environment(\.design) private var design
    let symbol: String
    let label: String
    let identifier: String
    let action: () -> Void

    public init(
        _ symbol: String, label: String, identifier: String,
        action: @escaping () -> Void
    ) {
        self.symbol = symbol
        self.label = label
        self.identifier = identifier
        self.action = action
    }

    public var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(design.ink.color)
                .frame(width: 36, height: 36)
                .barItemSurface()
        }
        .accessibilityLabel(label)
        .identified(identifier, label: label)
    }
}
