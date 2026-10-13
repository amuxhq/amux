import AmuxCore
import AmuxDesign
import SwiftUI

/// One line of what a destructive act does: a check for what stays, a cross
/// in the colour of what a change takes away for what goes.
struct Consequence: View {
    @Environment(\.design) private var design
    let glyph: String
    let kept: Bool
    let text: String

    init(_ glyph: String, kept: Bool, _ text: String) {
        self.glyph = glyph
        self.kept = kept
        self.text = text
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 9) {
            Image(systemName: glyph)
                .font(.system(size: 11, weight: .bold))
                .foregroundStyle(kept ? design.inkMuted.color : design.removed.color)
                .frame(width: 14)
            Text(text)
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
    }
}

/// The accent dot a list row carries beside its age while its agent is
/// waiting on you.
///
/// Small and on the trailing edge, where a conversation list marks what is
/// unread, rather than a disc in front of the name: a disc there needed a slot
/// held open on every row that had nothing to show, and a list of calm agents
/// became a column of gaps.
public struct NeedsYouDot: View {
    @Environment(\.design) private var design
    @Environment(\.hidesNeedsYouDot) private var hidden

    public init() {}

    public var body: some View {
        Circle()
            .fill(design.accent.color)
            .frame(width: 8, height: 8)
            .opacity(hidden ? 0 : 1)
            .accessibilityHidden(true)
    }
}

private struct HidesNeedsYouDotKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    /// Whether the needs-you dot is drawn clear. Only a driving build sets
    /// it, to show that a whole-screen golden notices one small mark gone;
    /// the dot is too small to be an element the geometry names.
    public var hidesNeedsYouDot: Bool {
        get { self[HidesNeedsYouDotKey.self] }
        set { self[HidesNeedsYouDotKey.self] = newValue }
    }
}

/// A group of rows on one surface, hairline-separated.
public struct RowGroup<Item: Identifiable, Content: View>: View {
    @Environment(\.design) private var design
    private let items: [Item]
    private let prominence: Design.Prominence
    private let row: (Item) -> Content

    public init(
        items: [Item],
        prominence: Design.Prominence = .plain,
        @ViewBuilder row: @escaping (Item) -> Content
    ) {
        self.items = items
        self.prominence = prominence
        self.row = row
    }

    public var body: some View {
        Surface(prominence: prominence) {
            LazyVStack(spacing: 0) {
                ForEach(Array(items.enumerated()), id: \.element.id) { index, item in
                    row(item)
                    if index < items.count - 1 {
                        Rectangle()
                            .fill(design.hairline.color)
                            .frame(height: design.metrics.hairline)
                            .padding(
                                .leading,
                                prominence == .subject
                                    ? 13 : design.surfaces.separation == .rule ? 0 : 46)
                    }
                }
            }
        }
    }
}

/// A settings-style row: a label, its current value, and somewhere to go.
///
/// Interaction stays outside this view so the same production presentation
/// can be used for a button or for a fact that has nowhere deeper to open.
public struct FieldRow: View {
    @Environment(\.design) private var design
    @Environment(\.dynamicTypeSize) private var typeSize
    private let label: String
    private let value: String?
    private let mono: Bool
    private let glyph: String?
    private let chevron: Bool
    private let tint: Color?

    public init(
        label: String,
        value: String? = nil,
        mono: Bool = false,
        glyph: String? = nil,
        chevron: Bool = true,
        tint: Color? = nil
    ) {
        self.label = label
        self.value = value
        self.mono = mono
        self.glyph = glyph
        self.chevron = chevron
        self.tint = tint
    }

    public var body: some View {
        HStack(spacing: 10) {
            if let glyph {
                Image(systemName: glyph)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 20)
            }
            // At an accessibility text size a label and its value side by
            // side leave neither room: the value goes under the label and
            // wraps whole rather than being cut to a few characters.
            if typeSize.isAccessibilitySize {
                VStack(alignment: .leading, spacing: 2) {
                    labelText
                    if let value {
                        Text(value)
                            .designFont(mono ? .mono : .body, design)
                            .foregroundStyle(design.inkMuted.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                labelText
                Spacer(minLength: 10)
                if let value {
                    Text(value)
                        .designFont(mono ? .mono : .body, design)
                        .foregroundStyle(design.inkMuted.color)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
            }
            if chevron {
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 13)
        .frame(minHeight: 44)
        .contentShape(Rectangle())
    }

    private var labelText: some View {
        Text(label)
            .designFont(.body, design)
            .foregroundStyle(tint ?? design.ink.color)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// A section header. Quiet, uppercase, and never coloured — a heading is
/// structure, not attention.
public struct SectionHead: View {
    @Environment(\.design) private var design
    private let title: String
    private let trailing: String?

    public init(title: String, trailing: String? = nil) {
        self.title = title
        self.trailing = trailing
    }

    public var body: some View {
        HStack {
            Text(title.uppercased())
                .designFont(.sectionTitle, design)
                .foregroundStyle(design.inkFaint.color)
            Spacer()
            if let trailing {
                Text(trailing)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkFaint.color)
            }
        }
        .padding(.leading, 2)
        .accessibilityElement(children: .combine)
    }
}

/// A button. Four weights, and the outline exists so a refusal can sit beside
/// an approval at the same size without looking like the same offer.
public struct ActionLabel: View {
    @Environment(\.design) private var design
    private let title: String
    private let kind: Kind
    private let fill: Bool

    /// `primary` is ink, not accent. Three waiting agents on one screen means
    /// three primary buttons, and filling those with the accent floods a
    /// screen whose whole rule is that colour means attention. `destructive`
    /// is the one press that cannot be undone, in the colour of what a change
    /// takes away.
    public enum Kind: Sendable { case primary, destructive, quiet, outline, plain }

    public init(_ title: String, kind: Kind = .primary, fill: Bool = false) {
        self.title = title
        self.kind = kind
        self.fill = fill
    }

    public var body: some View {
        Text(title)
            .designFont(.bodyEmphasis, design)
            .foregroundStyle(foreground)
            .lineLimit(1)
            .padding(.horizontal, 16)
            .padding(.vertical, 11)
            .frame(maxWidth: fill ? .infinity : nil, minHeight: 44)
            .background {
                let shape = RoundedRectangle(
                    cornerRadius: design.metrics.controlRadius, style: .continuous)
                switch kind {
                case .primary: shape.fill(design.ink.color)
                case .destructive: shape.fill(design.removed.color)
                case .quiet: shape.fill(design.sunken.color)
                case .outline: shape.strokeBorder(design.hairline.color, lineWidth: 1)
                case .plain: shape.fill(.clear)
                }
            }
    }

    private var foreground: Color {
        switch kind {
        case .primary: design.ground.color
        case .destructive: Color.white
        case .quiet, .outline: design.ink.color
        case .plain: design.accent.color
        }
    }
}

/// A round glass button — the shape iOS uses for a bare action in a bar.
public struct GlassIcon: View {
    @Environment(\.design) private var design
    private let glyph: String
    private let prominent: Bool
    private let size: CGFloat

    public init(glyph: String, prominent: Bool = false, size: CGFloat = 34) {
        self.glyph = glyph
        self.prominent = prominent
        self.size = size
    }

    public var body: some View {
        Image(systemName: glyph)
            .font(.system(size: size * 0.44, weight: .semibold))
            .foregroundStyle(prominent ? design.onAccent.color : design.ink.color)
            .frame(width: size, height: size)
            .background {
                if prominent {
                    Circle().fill(design.accent.color)
                } else {
                    Color.clear.frosted(Circle(), as: .glass)
                }
            }
            .contentShape(Circle())
    }
}

/// The way back from a pushed screen.
///
/// The selected presentation uses the same compact tinted label everywhere.
/// The action and accessibility name remain explicit because production
/// screens navigate real state rather than depicting a static destination.
public struct BackLink: View {
    @Environment(\.design) private var design
    private let title: String
    private let identifier: String
    private let spokenLabel: String
    private let action: @MainActor () -> Void

    public init(
        _ title: String,
        identifier: String,
        accessibilityLabel: String? = nil,
        action: @escaping @MainActor () -> Void
    ) {
        self.title = title
        self.identifier = identifier
        spokenLabel = accessibilityLabel ?? "Back to \(title)"
        self.action = action
    }

    public var body: some View {
        Button(action: action) {
            HStack(spacing: 3) {
                Image(systemName: "chevron.left")
                    .font(.system(size: 15, weight: .semibold))
                Text(title)
                    .designFont(.body, design)
            }
            .foregroundStyle(design.accent.color)
            .thumbTarget(x: 1, y: 13)
        }
        .buttonStyle(.amuxControl)
        .accessibilityLabel(spokenLabel)
        .identified(identifier, label: spokenLabel)
        .reclaimingThumbTarget(x: 1, y: 13)
    }
}

/// A screen's primary action on glass above the home indicator.
///
/// Kept separate from the full-height screen so scrolling content can run
/// behind it and every flow gets the same reachable geometry.
public struct BottomAction<Content: View>: View {
    @Environment(\.design) private var design
    private let content: Content

    public init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    public var body: some View {
        VStack(spacing: 0) { content }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .frosted(RoundedRectangle(
                cornerRadius: design.metrics.floatRadius,
                style: .continuous))
            .padding(.horizontal, 12)
            .padding(.bottom, 10)
    }
}

/// Supporting prose, one step down from the thing it explains.
public struct Explain: View {
    @Environment(\.design) private var design
    private let text: String

    public init(_ text: String) {
        self.text = text
    }

    public var body: some View {
        Text(text)
            .designFont(.detail, design)
            .foregroundStyle(design.inkMuted.color)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// How a key is written where a person has to read it.
///
/// A fingerprint is sixty-four hex characters and the only thing anybody does
/// with one is compare it against the same key written somewhere else, so how
/// it is set is the whole of whether that comparison is possible.
public enum Fingerprint {
    /// In fours.
    ///
    /// An unbroken run of sixty-four is where an eye loses its place; in fours
    /// the comparison is short hops. The characters and their order are
    /// untouched, so what is on screen is still the fingerprint.
    public static func grouped(_ fingerprint: String) -> String {
        stride(from: 0, to: fingerprint.count, by: 4).map { start in
            String(Array(fingerprint)[start..<min(start + 4, fingerprint.count)])
        }.joined(separator: " ")
    }

    /// The first four characters and the last four, with the middle said to be
    /// missing rather than merely absent.
    ///
    /// For a row that names a key rather than asks about one. Four and four is
    /// what somebody can hold in their head while glancing between two
    /// screens, and it is not a comparison — anywhere a key is actually being
    /// decided about, the whole of it is shown.
    public static func short(_ fingerprint: String) -> String {
        guard fingerprint.count > 11 else { return fingerprint }
        return "\(fingerprint.prefix(4))…\(fingerprint.suffix(4))"
    }
}

/// One choice of several, chosen or not, in ink: choosing is not something
/// waiting on you, so it does not take the accent.
struct Radio: View {
    @Environment(\.design) private var design
    let chosen: Bool

    var body: some View {
        let mark = design.ink.color
        return ZStack {
            Circle()
                .strokeBorder(
                    chosen ? mark : design.hairline.color, lineWidth: chosen ? 2 : 1)
                .frame(width: 15, height: 15)
            if chosen {
                Circle().fill(mark).frame(width: 7, height: 7)
            }
        }
        .frame(width: 18, height: 18)
    }
}

extension View {
    /// A Toggle as one named switch to VoiceOver. On iOS 26 the UIKit switch a
    /// Toggle draws is an accessibility element of its own, with no name,
    /// beside the Toggle's named one; this stands a single switch in for both.
    func oneSwitch(_ label: String, isOn: Binding<Bool>) -> some View {
        accessibilityRepresentation { Toggle(label, isOn: isOn) }
    }
}
