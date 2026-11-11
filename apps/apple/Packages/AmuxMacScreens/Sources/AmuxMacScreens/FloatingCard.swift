import AmuxValues
import AmuxWords
import SwiftUI

/// Where the open agent works, at the chat's top right while the right-hand
/// pane is closed: its host and folder, its branch, its changes and its
/// family. Each line that has more opens the pane on it.
struct FloatingCard: View {
    nonisolated static let width: CGFloat = 240

    let row: FleetRow
    let branch: String?
    let changes: ChangeTotals?
    let open: @MainActor (InspectorTab) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Line(symbol: HostSymbol.of(row), text: PlaceNames.host(row.hostName), trailing: nil)
            Line(symbol: "folder", text: PlaceNames.directory(row.workingDirectory), trailing: nil)
            if let branch {
                Line(symbol: "arrow.triangle.branch", text: branch, trailing: nil)
            }
            Divider().padding(.vertical, 4)
            Button {
                open(.changes)
            } label: {
                Line(symbol: "plusminus", text: "Changes", trailing: AnyView(counts))
            }
            .buttonStyle(HoverButtonStyle())
            if row.card.members > 1 || row.card.descendants > 0 {
                Button {
                    open(.family)
                } label: {
                    Line(
                        symbol: "person.2", text: "Family",
                        trailing: AnyView(
                            Text("\(max(row.card.members, row.card.descendants + 1) - 1)")
                                .foregroundStyle(.secondary).monospacedDigit()))
                }
                .buttonStyle(HoverButtonStyle())
            }
        }
        .font(.system(size: 12.5))
        .padding(8)
        .frame(width: Self.width)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Palette.raised))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Palette.line))
        .shadow(color: .black.opacity(0.10), radius: 10, y: 3)
    }

    private var counts: some View { ChangeCounts(changes: changes) }
}

/// "+6 −3", or "none".
struct ChangeCounts: View {
    let changes: ChangeTotals?

    var body: some View {
        if let changes, changes.files > 0 {
            HStack(spacing: 4) {
                Text("+\(changes.added)").foregroundStyle(.green)
                Text("−\(changes.removed)").foregroundStyle(.red)
            }
            .monospacedDigit()
        } else {
            Text("none").foregroundStyle(.tertiary)
        }
    }
}

/// The card folded to its changes, where the window is too narrow for it
/// beside the chat.
struct FloatingPill: View {
    let changes: ChangeTotals?
    let open: @MainActor (InspectorTab) -> Void

    var body: some View {
        Button {
            open(.changes)
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "plusminus").font(.system(size: 11)).foregroundStyle(.secondary)
                ChangeCounts(changes: changes)
            }
            .font(.system(size: 12))
            .padding(.horizontal, 10)
            .frame(height: 28)
            .background(Capsule().fill(Palette.raised))
            .overlay(Capsule().strokeBorder(Palette.line))
            .shadow(color: .black.opacity(0.10), radius: 8, y: 2)
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .help("Changes")
    }
}

private struct Line: View {
    let symbol: String
    let text: String
    let trailing: AnyView?

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: symbol)
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
                .frame(width: 16)
            Text(text).lineLimit(1).truncationMode(.middle)
            Spacer(minLength: 4)
            if let trailing { trailing }
        }
        .padding(.horizontal, 6)
        .frame(height: 24)
        .contentShape(Rectangle())
    }
}

enum HostSymbol {
    static func of(_ row: FleetRow) -> String {
        switch row.hostReach {
        case .online: "desktopcomputer"
        case .away: "cloud"
        case .offline: "desktopcomputer.trianglebadge.exclamationmark"
        }
    }
}
