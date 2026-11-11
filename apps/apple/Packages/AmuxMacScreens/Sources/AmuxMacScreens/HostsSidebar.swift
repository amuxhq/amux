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

struct HostRow: View {
    let host: HostView
    let selected: Bool
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: HostWords.symbol(host))
                .font(.system(size: 14))
                .foregroundStyle(.secondary)
                .frame(width: 18)
            VStack(alignment: .leading, spacing: 2) {
                Text(PlaceNames.host(host.name)).font(.system(size: 13)).lineLimit(1)
                Text(HostWords.reach(host))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
            Circle()
                .fill(host.local || HostWords.online(host) ? Color.green : Color.secondary.opacity(0.4))
                .frame(width: 7, height: 7)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(selected ? Palette.selection : (hovering ? Palette.hover : .clear)))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .accessibilityElement(children: .combine)
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

    static func symbol(_ host: HostView) -> String {
        switch host.platform {
        case .ios: "iphone"
        case .linux, .windows: "server.rack"
        case .macos, .unspecified, nil: "desktopcomputer"
        }
    }
}
