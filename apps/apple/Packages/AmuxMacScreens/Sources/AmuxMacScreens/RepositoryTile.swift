import AmuxValues
import AmuxWords
import SwiftUI

/// A repository's tile: the first and last letter of its name on its tint,
/// the same for every checkout of it, on every client. An agent outside any
/// folder a tile can name gets a plain folder, and a provider subagent,
/// which has no folder, its provider's bot.
struct RepositoryTile: View {
    let repository: Repository?
    var size: CGFloat = 28
    var native = false
    /// Settled and finished agents' tiles go quiet with their rows.
    var quiet = false

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: size * 0.25, style: .continuous)
        Group {
            if let repository {
                Text(repository.mark.uppercased())
                    .font(.system(size: max(6.5, size * 0.46), weight: .bold, design: .rounded))
                    .tracking(-0.2)
                    .foregroundStyle(Palette.tint(repository.tint))
                    // Rounded capitals centred by their line sit high in a
                    // small tile.
                    .offset(y: size <= 14 ? 0.5 : 0)
                    .frame(width: size, height: size)
                    .background(shape.fill(Palette.tint(repository.tint).opacity(0.18)))
            } else {
                Icon(native ? "bot" : "folder", size: size * 0.5, optical: false)
                    .foregroundStyle(Palette.textSecondary)
                    .frame(width: size, height: size)
                    .background(shape.fill(Palette.well))
            }
        }
        .saturation(quiet ? 0 : 1)
        .opacity(quiet ? 0.6 : 1)
        // Beside a name, set a little low as icons are, so it sits with
        // lowercase words rather than on their capitals' middle.
        .offset(y: size <= 16 ? 1 : 0)
        // A graphic, not words: in a row aligned on text it sits centred
        // on the words' capitals rather than offering its letters'
        // baseline as the row's.
        .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 5 }
        .alignmentGuide(.lastTextBaseline) { $0[VerticalAlignment.center] + 5 }
        .accessibilityHidden(true)
    }
}

/// A repository as a small rounded square of its tint: a place, so a
/// square, and the same tint on every client for every checkout of it.
/// Beside a name it says which repository without spending a tile's room.
struct RepositorySwatch: View {
    let repository: Repository?
    var size: CGFloat = 8

    var body: some View {
        RoundedRectangle(cornerRadius: size * 0.3, style: .continuous)
            .fill(repository.map { Palette.tint($0.tint) } ?? Palette.textQuaternary)
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}

/// What an agent is doing, in the slot that leads its row, so the states
/// down a list read as one column: a breathing blue dot while it works, a
/// solid orange one when it needs the person, a red alert when it failed,
/// a cloud when its host cannot be reached. Nothing when it is idle or
/// done: the absence of a mark is the calm state.
struct StatusMark: View {
    let state: RowState
    /// The slot's side, the same with or without a mark.
    var slot: CGFloat = 14

    var body: some View {
        Group {
            switch state {
            case .working:
                Working(size: slot)
            case .starting:
                Circle().fill(Palette.accent.opacity(0.5)).frame(width: 6, height: 6)
            case .needsYou:
                Circle().fill(Palette.you).frame(width: 7, height: 7)
            case .exited(.failed):
                Icon("circle-alert", size: 12).foregroundStyle(Palette.failure)
            case .hostOffline, .hostAway:
                Icon("cloud-off", size: 12).foregroundStyle(Palette.textTertiary)
            default:
                Color.clear
            }
        }
        .frame(width: slot, height: slot)
    }
}

