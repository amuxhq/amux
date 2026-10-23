import SwiftUI

/// The app's visual language, as values.
///
/// One rule outranks the rest: colour means attention. An agent with nothing
/// to say is drawn achromatically, so the eye lands on the one that does
/// without anyone hunting for a badge — which is why there is a single
/// chromatic accent and every other token is a neutral grey.
public struct Design: Sendable, Equatable {
    public let name: String

    // Ground and surfaces: plain neutral greys with no tint, so the one
    // colour on the screen is the accent.
    public let ground: Ramp
    public let raised: Ramp
    public let sunken: Ramp
    public let hairline: Ramp

    // Ink. Neither pole is absolute: pure black on white is heavy and pure
    // white on black glares, so both back off a step from their ground.
    public let ink: Ramp
    public let inkMuted: Ramp
    public let inkFaint: Ramp

    /// The one chromatic voice, spent only on something that is waiting.
    public let accent: Ramp
    public let onAccent: Ramp

    /// What a change added and what it took away.
    ///
    /// The only colours in the design that are not the accent or a neutral
    /// grey, and the exception is deliberate: green and red are a
    /// convention about what changed, while the accent is this app's one word
    /// for "something is waiting for you". A diff is not that, so it must not
    /// borrow the word. Both are muted well below a signal green and red so
    /// that a page of them still reads as text.
    public let added: Ramp
    public let removed: Ramp

    public let faces: Faces
    public let metrics: Metrics
    public let type: Typography
    public let surfaces: Surfaces

    public init(
        name: String, ground: Ramp, raised: Ramp, sunken: Ramp, hairline: Ramp, ink: Ramp,
        inkMuted: Ramp, inkFaint: Ramp, accent: Ramp, onAccent: Ramp, added: Ramp,
        removed: Ramp, faces: Faces, metrics: Metrics, type: Typography, surfaces: Surfaces
    ) {
        self.name = name
        self.ground = ground
        self.raised = raised
        self.sunken = sunken
        self.hairline = hairline
        self.ink = ink
        self.inkMuted = inkMuted
        self.inkFaint = inkFaint
        self.accent = accent
        self.onAccent = onAccent
        self.added = added
        self.removed = removed
        self.faces = faces
        self.metrics = metrics
        self.type = type
        self.surfaces = surfaces
    }

    public struct Metrics: Sendable, Equatable {
        public let cardRadius: CGFloat
        public let controlRadius: CGFloat
        /// The radius of anything that floats free of an edge; a detached
        /// surface is rounder than one that meets a border.
        public let floatRadius: CGFloat
        public let rowPadding: CGFloat
        public let gutter: CGFloat
        public let feedGap: CGFloat
        public let hairline: CGFloat

        public init(
            cardRadius: CGFloat, controlRadius: CGFloat, floatRadius: CGFloat,
            rowPadding: CGFloat, gutter: CGFloat,
            feedGap: CGFloat, hairline: CGFloat
        ) {
            self.cardRadius = cardRadius
            self.controlRadius = controlRadius
            self.floatRadius = floatRadius
            self.rowPadding = rowPadding
            self.gutter = gutter
            self.feedGap = feedGap
            self.hairline = hairline
        }
    }

    /// How a raised thing separates itself from what is behind it.
    public enum Separation: String, Sendable {
        /// A rule between rows, and no container at all.
        case rule
        /// A filled surface with a hairline border.
        case card
        /// Glass, and nothing else — no border, no shadow.
        case glass
    }

    public struct Surfaces: Sendable, Equatable {
        public let separation: Separation
        /// Whether the ground is flat or graduated.
        public let graduated: Bool
    }

    /// Where a surface may be glass. The agents are what the app is about and
    /// glass makes them read as objects; a host row is configuration, and
    /// configuration wants to be quiet.
    public enum Prominence: Sendable { case subject, plain }

    public struct Typography: Sendable, Equatable {
        /// Agent and host names in the mono face. Off: a name is what a person
        /// reads a list by, and mono makes a chat list read as a terminal.
        public let identifierIsMono: Bool
        public let titleWeight: Font.Weight
        /// How big a screen's own name is.
        public let titleSize: CGFloat
        public let bodySize: CGFloat
        public let tightTracking: CGFloat

        public init(
            identifierIsMono: Bool, titleWeight: Font.Weight, titleSize: CGFloat,
            bodySize: CGFloat, tightTracking: CGFloat
        ) {
            self.identifierIsMono = identifierIsMono
            self.titleWeight = titleWeight
            self.titleSize = titleSize
            self.bodySize = bodySize
            self.tightTracking = tightTracking
        }
    }

    /// The app's one skin: type-led, rules instead of containers, and mono
    /// kept for code: commands, paths and diffs.
    public static let app = Design(
        name: "Studio",
        // White, and the system's true black: the clean ground of the chat
        // apps people already know, which the system's glass bars are drawn
        // to sit on.
        ground: Ramp(0xFFFFFF, 0x000000),
        raised: Ramp(0xFFFFFF, 0x1C1C1E),
        // Light recesses by going darker; dark cannot, because its ground is
        // already black, so on dark a well is a step up.
        sunken: Ramp(0xF2F2F4, 0x161618),
        hairline: Ramp(0xDCDCE0, 0x38383A),
        ink: Ramp(0x1D1C1F, 0xF5F5F7),
        inkMuted: Ramp(0x5E5E66, 0x98989F),
        inkFaint: Ramp(0x8A8A93, 0x6E6E73),
        // Petrol: cool enough to stay calm, far enough round the wheel from
        // the indigo every AI product wears to read as instrument, not brand.
        accent: Ramp(0x0E5F6E, 0x4FBACD),
        onAccent: Ramp(0xFFFFFF, 0x06080B),
        // Read off the approved drawings rather than picked afresh, so the
        // arithmetic on a chip and the body of a diff are the same green and
        // the same red the design was signed off in.
        added: Ramp(0x29854F, 0x70C78C),
        removed: Ramp(0xA8333D, 0xE68080),
        faces: .instrument,
        metrics: Metrics(
            cardRadius: 16, controlRadius: 12, floatRadius: 26,
            rowPadding: 12, gutter: 16, feedGap: 16,
            hairline: 0.5),
        type: Typography(
            identifierIsMono: false, titleWeight: .semibold,
            titleSize: 26, bodySize: 16, tightTracking: -0.4),
        surfaces: Surfaces(separation: .rule, graduated: false))

    /// Every colour token in the order a catalogue or a pinned table reads
    /// them, so no token can be added without showing up in both.
    public var colours: [(name: String, ramp: Ramp)] {
        [
            ("ground", ground), ("raised", raised), ("sunken", sunken),
            ("hairline", hairline), ("ink", ink), ("inkMuted", inkMuted),
            ("inkFaint", inkFaint), ("accent", accent), ("onAccent", onAccent),
            ("added", added), ("removed", removed),
        ]
    }

    /// The accent as a plain `Color` for `.tint`, which cannot resolve a Ramp.
    public var accentColor: Color { accent.color }
}

private struct DesignKey: EnvironmentKey {
    static let defaultValue = Design.app
}

extension EnvironmentValues {
    public var design: Design {
        get { self[DesignKey.self] }
        set { self[DesignKey.self] = newValue }
    }
}

/// Whether the screen is being photographed for a baseline rather than looked
/// at by a person.
///
/// A photograph of a screen has to be of the screen and not of the moment it
/// was taken in, so anything that runs on a timer of its own draws its resting
/// state while this is set. The text caret is the one that matters: it blinks
/// about once a second, on nobody's schedule but its own, and a picture caught
/// mid-blink differs from the one before it by a bar of accent that means
/// nothing.
private struct PhotographedKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    public var photographed: Bool {
        get { self[PhotographedKey.self] }
        set { self[PhotographedKey.self] = newValue }
    }
}

/// What the reader has asked the system to do less of.
///
/// SwiftUI publishes both of these already, but publishes them read-only: they
/// are the device's settings and nothing inside an app may write them. So the
/// app reads its own copy, planted from the device's answer by
/// `readingAssistiveSettings` at the root and overridable above it. A screen
/// being photographed with one of them on is the only thing that ever
/// overrides them, and without that there would be no way to see what this app
/// looks like for a reader who has turned them on.
private struct ReducesMotionKey: EnvironmentKey {
    static let defaultValue = false
}

private struct ReducesTransparencyKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    public var reducesMotion: Bool {
        get { self[ReducesMotionKey.self] }
        set { self[ReducesMotionKey.self] = newValue }
    }

    public var reducesTransparency: Bool {
        get { self[ReducesTransparencyKey.self] }
        set { self[ReducesTransparencyKey.self] = newValue }
    }
}

private struct AssistiveSettings: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var motion
    @Environment(\.accessibilityReduceTransparency) private var transparency

    func body(content: Content) -> some View {
        content
            .environment(\.reducesMotion, motion)
            .environment(\.reducesTransparency, transparency)
    }
}

extension View {
    /// Puts the device's own reduce-motion and reduce-transparency settings
    /// where the app reads them. Applied once, at the root of everything drawn.
    public func readingAssistiveSettings() -> some View {
        modifier(AssistiveSettings())
    }
}
