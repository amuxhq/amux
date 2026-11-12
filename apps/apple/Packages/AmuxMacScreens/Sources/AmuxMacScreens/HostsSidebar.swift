import AmuxValues
import AmuxWords
import SwiftUI

/// The machines the profile trusts: this Mac first, then the rest, those
/// reachable now ahead of those that are not.
struct HostsSidebar: View {
    let content: MainContent
    @Binding var place: Place

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 1) {
                if let mac = content.thisMac {
                    SectionTitle(text: "This Mac")
                    item(mac)
                }
                if !content.hosts.isEmpty {
                    SectionTitle(text: "Paired")
                    ForEach(content.hosts, id: \.hostId) { host in item(host) }
                }
            }
            .padding(.horizontal, 8)
            .padding(.bottom, 8)
        }
        .overlay {
            if content.thisMac == nil, content.hosts.isEmpty {
                Text("No hosts yet").foregroundStyle(.secondary)
            }
        }
    }

    private func item(_ host: HostView) -> some View {
        HostRow(host: host, selected: place.host == host.hostId)
            .onTapGesture { place.host = host.hostId }
    }
}

/// A machine as the agent rows are drawn: its kind on a tile, its name, and
/// how this Mac reaches it; a badge on the tile when it cannot be reached.
struct HostRow: View {
    let host: HostView
    let selected: Bool
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 10) {
            HostTile(host: host, size: 30)
                .overlay(alignment: .bottomTrailing) {
                    if !host.local, !HostWords.online(host) {
                        Icon("cloud-off", size: 10)
                            .foregroundStyle(.secondary)
                            .padding(1.5)
                            .background(Circle().fill(Palette.sidebar))
                            .offset(x: 4, y: 4)
                    }
                }
            VStack(alignment: .leading, spacing: 2) {
                Text(PlaceNames.host(host.name))
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(host.local || HostWords.online(host) ? .primary : .secondary)
                    .lineLimit(1)
                Text(HostWords.reach(host))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8)
        .frame(height: 48)
        .background(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(selected ? Palette.selection : (hovering ? Palette.hover : .clear)))
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
            .foregroundStyle(.secondary)
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
            case .direct: "Connected on this network"
            case .relay: "Connected through the relay"
            case .ssh: "Connected over SSH"
            case .unspecified: "Connected"
            }
        case .away: return "Away · the relay sees it"
        case .offline(.revoked): return "No longer trusts this Mac"
        case .offline(.signedOut): return "Offline · sign in to reach it"
        case .offline: return "Offline"
        }
    }

    static func symbol(_ host: HostView) -> String { lucide(host) }

    /// The host's kind as one of the app's icons.
    static func lucide(_ host: HostView) -> String {
        switch host.platform {
        case .ios: "smartphone"
        case .linux, .windows: "server"
        case .macos, .unspecified, nil: "laptop"
        }
    }
}
