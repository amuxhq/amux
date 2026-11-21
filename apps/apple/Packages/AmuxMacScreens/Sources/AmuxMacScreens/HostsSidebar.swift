import AmuxValues
import AmuxWords
import SwiftUI

/// The machines the profile trusts: this Mac first, then the rest, those
/// reachable now ahead of those that are not.
struct HostsSidebar: View {
    let content: MainContent
    @Binding var place: Place

    var body: some View {
        VStack(spacing: 0) {
            WindowBar(leading: 16, rule: false) {
                Text("Devices").font(.ui(.prose, weight: .medium))
                Spacer()
            }
            list
        }
    }

    private var list: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 1) {
                // This Mac's row says so itself.
                if let mac = content.thisMac {
                    item(mac).padding(.top, 6)
                }
                if !content.hosts.isEmpty {
                    SectionTitle(text: "Paired", first: content.thisMac == nil)
                    ForEach(content.hosts, id: \.hostId) { host in item(host) }
                }
            }
            .padding(.horizontal, 8)
            .padding(.bottom, 8)
        }
        .overlay {
            if content.thisMac == nil, content.hosts.isEmpty {
                Text("No devices yet").foregroundStyle(Palette.textSecondary)
            }
        }
    }

    private func item(_ host: HostView) -> some View {
        HostRow(host: host, selected: place.host == host.hostId)
            .onTapGesture { place.host = host.hostId }
    }
}

/// A machine as the agent rows are drawn: its kind's icon in the column
/// the agents' marks take, its name, and how this Mac reaches it. One it
/// cannot reach reads quieter.
struct HostRow: View {
    let host: HostView
    let selected: Bool
    @State private var hovering = false

    var body: some View {
        let reachable = host.local || HostWords.online(host)
        HStack(spacing: 10) {
            HostTile(host: host, size: 28)
            VStack(alignment: .leading, spacing: 2) {
                Text(PlaceNames.host(host.name))
                    .font(.ui(.prose, weight: .medium))
                    .foregroundStyle(reachable ? Palette.text : Palette.text.opacity(0.75))
                    .lineLimit(1)
                    .frame(height: 20)
                Text(HostWords.reach(host))
                    .font(.ui(.meta))
                    .foregroundStyle(Palette.textSecondary)
                    .lineLimit(1)
                    .frame(height: 16)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background {
            let shape = RoundedRectangle(cornerRadius: Radius.row, style: .continuous)
            shape.fill(selected ? Palette.selection : (hovering ? Palette.hover : .clear))
                .overlay(shape.strokeBorder(selected ? Palette.selectionRing : .clear))
        }
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .accessibilityElement(children: .combine)
    }
}

/// A machine's kind on a plain tile, where a repository would have its
/// letters.
struct HostTile: View {
    let host: HostView
    var size: CGFloat = 30

    var body: some View {
        Icon(HostWords.symbol(host), size: size * 0.5)
            .foregroundStyle(Palette.textSecondary)
            .frame(width: size, height: size)
            .background(RoundedRectangle(cornerRadius: size * 0.3, style: .continuous).fill(Palette.well))
            .overlay(RoundedRectangle(cornerRadius: size * 0.3, style: .continuous).strokeBorder(Palette.line))
            .opacity(host.local || HostWords.online(host) ? 1 : 0.6)
    }
}

/// How the Mac words a host's reach and kind.
enum HostWords {
    static func online(_ host: HostView) -> Bool {
        if case .online = host.reach { return true }
        return false
    }

    static func reach(_ host: HostView) -> String {
        if host.local { return "This Mac" }
        switch host.reach {
        case .online(let via):
            return switch via {
            case .direct: "Connected · Direct"
            case .relay: "Connected · Relayed"
            case .ssh: "Connected over SSH"
            case .unspecified: "Connected"
            }
        case .away: return "Online · needs Pro"
        case .offline(.revoked): return "Unpaired · pair again to reconnect"
        case .offline(.signedOut): return "Offline · sign in to reach it"
        case .offline: return "Offline"
        }
    }

    static func symbol(_ host: HostView) -> String { lucide(host) }

    /// The host's kind as one of the app's icons.
    static func lucide(_ host: HostView) -> String {
        switch host.platform {
        case .ios, .android: "smartphone"
        case .linux, .windows: "server"
        case .macos, .unspecified, nil: "laptop"
        }
    }
}
