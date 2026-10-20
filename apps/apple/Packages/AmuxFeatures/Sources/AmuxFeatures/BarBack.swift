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

/// What a pushed page shows while the one thing on it is under way: the
/// breathing mark and what is happening, in ink, where the button was. A
/// button greyed out to say "wait" is a button that cannot be read.
struct WaitingLine: View {
    @Environment(\.design) private var design
    let title: String

    var body: some View {
        HStack(spacing: 10) {
            StateMark(state: .working)
            Text(title)
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
        }
        .frame(maxWidth: .infinity, minHeight: 44)
        .accessibilityElement(children: .combine)
    }
}

extension View {
    /// A pushed page's bar: the back circle on its own, with the page's
    /// heading in its content rather than beside it.
    func backBar(_ identifier: String, back: @escaping @MainActor () -> Void) -> some View {
        modifier(BackBar(identifier: identifier, back: back))
    }
}

private struct BackBar: ViewModifier {
    @Environment(\.reducesTransparency) private var reducesTransparency
    let identifier: String
    let back: @MainActor () -> Void

    func body(content: Content) -> some View {
        content
            .navigationBarBackButtonHidden(true)
            .toolbarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    BarBack(identifier: identifier) { back() }
                }
                .sharedBackgroundVisibility(reducesTransparency ? .hidden : .automatic)
            }
    }
}
