import SwiftUI

/// The window's type scale. Every text size the screens use is one of
/// these, so a change to how a kind of text reads is made once.
///
/// Monospaced text takes the half steps (`small`, `callout`): at the same
/// size it reads larger than the system face beside it.
enum TextSize: CGFloat {
    /// A count on a badge.
    case badge = 10
    /// A rule's words, a timestamp under a bubble, a shortcut hint.
    case caption = 11
    /// A command's output.
    case small = 11.5
    /// Meta beside a row's words, section heads, notes under a row.
    case meta = 12
    /// Code in prose, the activity line, a chat's ending.
    case callout = 12.5
    /// Rows, controls and the work in a turn.
    case body = 13
    /// The person's prompt.
    case bubble = 13.5
    /// The agent's prose.
    case prose = 14
    /// A card's title.
    case title = 15
    /// A second-level heading in prose.
    case proseSubheading = 15.5
    /// A first-level heading in prose.
    case proseHeading = 18
    /// A page's heading.
    case heading = 22
    /// Setting up's heading.
    case display = 26
    /// The one large mark on an empty page.
    case hero = 46
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
    /// A sidebar row, a small button's hover fill.
    static let row: CGFloat = 6
    /// A hover button's fill.
    static let control: CGFloat = 7
    /// The rail's tiles.
    static let tile: CGFloat = 7.5
    /// Wells inside a card: output, code, a field.
    static let well: CGFloat = 8
    /// Cards in the feed and lists of hosts.
    static let card: CGFloat = 10
    /// What floats over the feed: the ask, the floating card.
    static let panel: CGFloat = 12
    /// Setting up's cards.
    static let sheet: CGFloat = 14
    /// The composer and the prompt's bubble.
    static let composer: CGFloat = 16
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
