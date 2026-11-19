import SwiftUI

/// The window's type scale. Every text size the screens use is one of
/// these, so a change to how a kind of text reads is made once.
///
/// The system face takes whole points only; monospaced text takes the
/// half steps (`small`, `callout`) as well, since at the same size it
/// reads larger than the system face beside it.
struct TextSize: Sendable {
    let rawValue: CGFloat

    /// A count on a badge.
    static let badge = TextSize(rawValue: 10)
    /// A rule's words, a timestamp under a bubble, a shortcut hint.
    static let caption = TextSize(rawValue: 11)
    /// A command's output. Monospaced only.
    static let small = TextSize(rawValue: 11.5)
    /// Meta beside a row's words, section heads, notes under a row.
    static let meta = TextSize(rawValue: 12)
    /// Code in prose and in a step. Monospaced only.
    static let callout = TextSize(rawValue: 12.5)
    /// Rows, controls and the work in a turn.
    static let body = TextSize(rawValue: 13)
    /// The person's prompt: the agent's size, so neither reads louder.
    static let bubble = TextSize(rawValue: 14)
    /// The agent's prose.
    static let prose = TextSize(rawValue: 14)
    /// A card's title.
    static let title = TextSize(rawValue: 15)
    /// A second-level heading in prose.
    static let proseSubheading = TextSize(rawValue: 16)
    /// A first-level heading in prose.
    static let proseHeading = TextSize(rawValue: 18)
    /// A page's heading.
    static let heading = TextSize(rawValue: 22)
    /// Setting up's heading.
    static let display = TextSize(rawValue: 26)
    /// The one large mark on an empty page.
    static let hero = TextSize(rawValue: 46)
}

extension Font {
    /// The system face at a step of the scale.
    static func ui(_ size: TextSize, weight: Font.Weight = .regular) -> Font {
        .system(size: size.rawValue, weight: weight)
    }

    /// The monospaced face at a step of the scale.
    static func mono(_ size: TextSize, weight: Font.Weight = .regular) -> Font {
        .system(size: size.rawValue, weight: weight, design: .monospaced)
    }
}

/// Corner radii, from the smallest control to the composer.
enum Radius {
    /// A rail button.
    static let rail: CGFloat = 10
    /// A sidebar row, a small button's hover fill.
    static let row: CGFloat = 8
    /// A hover button's fill, a tab.
    static let control: CGFloat = 8
    /// Wells inside a card: output, code, a field; the rail's tiles.
    static let well: CGFloat = 8
    /// Cards in the feed and lists of hosts.
    static let card: CGFloat = 12
    /// What floats over the feed: the ask, the floating card.
    static let panel: CGFloat = 16
    /// Setting up's cards.
    static let sheet: CGFloat = 14
    /// The composer and the prompt's bubble.
    static let composer: CGFloat = 16
    /// A window on the frame. Concentric with the system's window corner
    /// (20 points on macOS 26) at the frame's gap, so the bottom corners
    /// run parallel to the window's own.
    static let window: CGFloat = 12
}

/// How far a surface stands off what is under it.
enum Elevation {
    /// Resting on the window: the composer, a pill, a chip.
    case raised
    /// Over the feed: the floating card.
    case floating

    var color: Color {
        switch self {
        case .raised: .black.opacity(0.06)
        case .floating: .black.opacity(0.1)
        }
    }

    var radius: CGFloat {
        switch self {
        case .raised: 8
        case .floating: 14
        }
    }

    var y: CGFloat {
        switch self {
        case .raised: 2
        case .floating: 6
        }
    }
}

extension View {
    func elevation(_ level: Elevation) -> some View {
        shadow(color: level.color, radius: level.radius, y: level.y)
    }
}
