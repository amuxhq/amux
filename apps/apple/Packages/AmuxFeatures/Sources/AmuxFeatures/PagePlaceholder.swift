import AmuxDesign
import SwiftUI

/// A pushed page whose content is not here: still on its way, there being
/// nothing to show, or the host not answering. Drawn as the app's other
/// empty states are — a heading and a sentence at the top — with the
/// system bar's back circle, so the way out is where it is on the page it
/// stands in for.
public struct PagePlaceholder: View {
    public enum Showing: Equatable, Sendable {
        /// On its way: the breathing mark and what is happening.
        case waiting(String)
        /// Nothing there, and nothing wrong.
        case empty(title: String, detail: String)
        /// Not coming, with why in the host's or service's own words.
        case failed(title: String, reason: String)
    }

    @Environment(\.design) private var design
    private let showing: Showing
    private let identifier: String
    private let back: @MainActor () -> Void

    /// `identifier` names the page; its back circle is `<identifier>.back`
    /// and what it says is `<identifier>.<empty|failure>`.
    public init(_ showing: Showing, identifier: String, back: @escaping @MainActor () -> Void) {
        self.showing = showing
        self.identifier = identifier
        self.back = back
    }

    public var body: some View {
        ZStack(alignment: .topLeading) {
            Ground()
            content
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, design.metrics.gutter)
                .padding(.top, Space.l)
        }
        .backBar("\(identifier).back", back: back)
    }

    @ViewBuilder
    private var content: some View {
        switch showing {
        case .waiting(let what):
            HStack(spacing: 10) {
                StateMark(state: .working)
                Text(what)
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
            }
            .accessibilityElement(children: .combine)
            .identified("\(identifier).waiting", value: what)
        case .empty(let title, let detail):
            VStack(alignment: .leading, spacing: 8) {
                heading(title)
                Explain(detail)
            }
            .accessibilityElement(children: .combine)
            .identified("\(identifier).empty", value: detail)
        case .failed(let title, let reason):
            VStack(alignment: .leading, spacing: 8) {
                heading(title)
                Text(reason)
                    .designFont(.detail, design)
                    .foregroundStyle(design.removed.color)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .accessibilityElement(children: .combine)
            .identified("\(identifier).failure", value: reason)
        }
    }

    private func heading(_ title: String) -> some View {
        Text(title)
            .designFont(.screenTitle, design)
            .foregroundStyle(design.ink.color)
            .fixedSize(horizontal: false, vertical: true)
    }
}
