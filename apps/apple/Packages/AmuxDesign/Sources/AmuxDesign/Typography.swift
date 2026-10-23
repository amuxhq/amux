import SwiftUI

extension Design {
    /// Type roles, so a screen asks for meaning rather than a point size.
    public enum Role: String, Sendable, CaseIterable {
        case screenTitle
        case sectionTitle
        case identifier         // an agent or host name — something you could type
        case identifierUnread
        case body
        case bodyEmphasis
        case detail             // supporting prose, one step down
        case caption            // timestamps, counts, metadata
        case mono               // commands, paths, diff bodies
        case monoSmall
    }

    /// A role resolved to type: the face it is set in, its size at the default
    /// content size, the weight asked for up front — a weight modifier applied
    /// afterwards does not reach a bundled variable face — and the text style
    /// its size scales with under Dynamic Type.
    public struct FontSpec: Sendable, Equatable {
        public let family: String
        public let size: CGFloat
        public let weight: Font.Weight
        public let relativeTo: Font.TextStyle
        public let tracking: CGFloat
    }

    public func spec(_ role: Role) -> FontSpec {
        let body = type.bodySize
        return switch role {
        case .screenTitle:
            FontSpec(family: faces.display, size: type.titleSize,
                     weight: type.titleWeight, relativeTo: .title, tracking: type.tightTracking)
        case .sectionTitle:
            FontSpec(family: faces.body, size: 13,
                     weight: .semibold, relativeTo: .footnote, tracking: 0)
        case .identifier:
            identifierSpec(weight: type.identifierIsMono ? .medium : .semibold)
        case .identifierUnread:
            identifierSpec(weight: type.identifierIsMono ? .bold : .heavy)
        case .body:
            FontSpec(family: faces.body, size: body,
                     weight: .regular, relativeTo: .callout, tracking: 0)
        case .bodyEmphasis:
            FontSpec(family: faces.body, size: body,
                     weight: .semibold, relativeTo: .callout, tracking: 0)
        case .detail:
            FontSpec(family: faces.body, size: body - 1,
                     weight: .regular, relativeTo: .subheadline, tracking: 0)
        case .caption:
            FontSpec(family: faces.body, size: 13,
                     weight: .regular, relativeTo: .footnote, tracking: 0)
        case .mono:
            FontSpec(family: faces.mono, size: body - 2,
                     weight: .regular, relativeTo: .footnote, tracking: 0)
        case .monoSmall:
            FontSpec(family: faces.mono, size: body - 3.5,
                     weight: .regular, relativeTo: .caption, tracking: 0)
        }
    }

    private func identifierSpec(weight: Font.Weight) -> FontSpec {
        type.identifierIsMono
            ? FontSpec(family: faces.mono, size: type.bodySize - 0.5,
                       weight: weight, relativeTo: .subheadline, tracking: 0)
            : FontSpec(family: faces.body, size: type.bodySize + 1,
                       weight: weight, relativeTo: .subheadline, tracking: 0)
    }

    /// Every role scales with the reader's content size; nothing is set at a
    /// fixed point size, because a phone's type size is the reader's choice.
    public func font(_ role: Role) -> Font {
        let spec = spec(role)
        BundledFonts.register()
        return .custom(spec.family, size: spec.size, relativeTo: spec.relativeTo)
            .weight(spec.weight)
    }

    public func tracking(_ role: Role) -> CGFloat { spec(role).tracking }
}

extension View {
    public func designFont(_ role: Design.Role, _ design: Design) -> some View {
        font(design.font(role)).tracking(design.tracking(role))
    }
}
