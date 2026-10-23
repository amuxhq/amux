import SwiftUI

extension View {
    /// Marks a surface that can be the top one on screen: the tabs, or a
    /// sheet raised over them.
    ///
    /// Two things stop at a sheet's edge, and each surface carries them past
    /// it. What is drawn over the app (the report's offer and the report)
    /// stays under a sheet, so it is drawn by whichever surface is on top.
    /// And what a sheet names never reaches a reader at the app's root,
    /// because SwiftUI hands a preference up only as far as the presentation
    /// it was set in, so each surface hands its own up to the driver.
    public func surface() -> some View {
        modifier(Surface())
    }
}

/// What each surface on screen has named, as the driver reads it.
///
/// A driver at the root sees only what no surface handed over; the rest is
/// here, one entry per surface in the order they appeared, which is the
/// order they are stacked.
@MainActor
public final class SurfaceElements {
    private var order: [UUID] = []
    private var named: [UUID: [IdentifiedElement]] = [:]

    public init() {}

    /// Every surface's elements, the bottom one first.
    public var stacked: [[IdentifiedElement]] {
        order.map { named[$0] ?? [] }
    }

    func enter(_ surface: UUID) {
        order.removeAll { $0 == surface }
        order.append(surface)
    }

    func leave(_ surface: UUID) {
        order.removeAll { $0 == surface }
        named[surface] = nil
    }

    func update(_ surface: UUID, _ elements: [IdentifiedElement]) {
        // A change that lands after the surface has gone would only hold
        // stale elements nobody reads.
        guard order.contains(surface) else { return }
        named[surface] = elements
    }
}

extension EnvironmentValues {
    @Entry public var surfaceElements: SurfaceElements? = nil
}

private struct Surface: ViewModifier {
    @Environment(\.reportLayers) private var reports
    @Environment(\.surfaceElements) private var elements
    @State private var surface = UUID()

    func body(content: Content) -> some View {
        content
            .overlay {
                if let reports, reports.isTop(surface) { reports.draw() }
            }
            .modifier(HandsUpElements(to: elements, from: surface))
            .onAppear {
                reports?.enter(surface)
                elements?.enter(surface)
            }
            .onDisappear {
                reports?.leave(surface)
                elements?.leave(surface)
            }
    }
}

/// Hands a surface's named elements to the driver and stops them there, so
/// a surface that is not a sheet is not read twice, once here and once at
/// the root.
private struct HandsUpElements: ViewModifier {
    let to: SurfaceElements?
    let from: UUID

    @ViewBuilder
    func body(content: Content) -> some View {
        if let to {
            content
                .onPreferenceChange(IdentifiedElements.self) { elements in
                    Task { @MainActor in to.update(from, elements) }
                }
                .transformPreference(IdentifiedElements.self) { $0 = [] }
        } else {
            content
        }
    }
}
