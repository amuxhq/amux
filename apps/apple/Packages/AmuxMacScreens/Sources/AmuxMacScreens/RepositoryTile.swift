import AmuxValues
import AmuxWords
import SwiftUI

/// A repository's tile: the first and last letter of its name on its tint,
/// the same for every checkout of it, on every client. An agent outside any
/// folder a tile can name gets a plain folder.
struct RepositoryTile: View {
    let repository: Repository?
    var size: CGFloat = 28
    /// Settled and finished agents' tiles go quiet with their rows.
    var quiet = false

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: size * 0.3, style: .continuous)
        Group {
            if let repository {
                Text(repository.mark)
                    .font(.system(size: size * 0.42, weight: .semibold, design: .rounded))
                    .foregroundStyle(Palette.tint(repository.tint))
                    .frame(width: size, height: size)
                    .background(shape.fill(Palette.tint(repository.tint).opacity(0.16)))
            } else {
                Icon("folder", size: size * 0.5)
                    .foregroundStyle(.secondary)
                    .frame(width: size, height: size)
                    .background(shape.fill(Palette.well))
            }
        }
        .overlay(shape.strokeBorder(Color.primary.opacity(0.06)))
        .saturation(quiet ? 0 : 1)
        .opacity(quiet ? 0.6 : 1)
        .accessibilityHidden(true)
    }
}

/// What an agent is doing, as a badge on its tile's corner: a blue arc while
/// it works, a coral dot when it needs the person, a red one when it failed,
/// a cloud when its host cannot be reached. Nothing when it is idle.
struct StatusBadge: View {
    let state: RowState
    var size: CGFloat = 11
    /// The surface behind the tile, and what lights the row (a selection or
    /// hover fill), which ring the badge so it reads as cut out of the tile.
    var ground: Color = Palette.sidebar
    var lit: Color = .clear

    var body: some View {
        switch state {
        case .working, .starting:
            Working(size: size - 2).padding(1.5).background(ring)
        case .needsYou:
            dot(Palette.you)
        case .exited(.failed):
            dot(Palette.failure)
        case .hostOffline, .hostAway:
            Icon("cloud-off", size: size - 1)
                .foregroundStyle(.secondary)
                .padding(1.5)
                .background(ring)
        default:
            EmptyView()
        }
    }

    private var ring: some View {
        ZStack {
            Circle().fill(ground)
            Circle().fill(lit)
        }
    }

    private func dot(_ color: Color) -> some View {
        Circle().fill(color).frame(width: size - 2, height: size - 2)
            .padding(2).background(ring)
    }
}

extension RowState {
    /// The badge has something to show.
    var badged: Bool {
        switch self {
        case .working, .starting, .needsYou, .exited(.failed), .hostOffline, .hostAway: true
        default: false
        }
    }
}
