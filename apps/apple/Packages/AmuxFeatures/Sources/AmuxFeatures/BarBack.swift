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

/// A tab's line under its large title, in the list's preview type and a
/// breath below the title: the bar's own subtitle sits tight against the
/// display face's short descenders.
///
/// The bar keeps the item whether or not there is anything to say. Adding or
/// removing it while the screen is up leaves the bar laid out for the old
/// height: a line appearing clips the top of the large title, one going away
/// drops the bar to its small title. An empty item still costs the bar's
/// 6 pt gap under the title, which is the price of a title that stays whole.
struct LargeSubtitle: View {
    @Environment(\.design) private var design
    @Environment(\.dynamicTypeSize) private var typeSize
    let text: String
    let identifier: String

    /// The breath below the title. The bar holds its items' text at
    /// xxLarge while the title above keeps growing, and past that the
    /// title's taller line reaches 4 points down, so at the bar's cap the
    /// line steps further away.
    private var gap: CGFloat { typeSize >= .xxLarge ? Space.m : Space.xs }

    var body: some View {
        if text.isEmpty {
            Color.clear.frame(height: 0)
        } else {
            Text(text)
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
                .padding(.top, gap)
                .frame(maxWidth: .infinity, alignment: .leading)
                .identified(identifier, value: text)
        }
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
