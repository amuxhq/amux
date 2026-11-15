import AmuxValues
import AmuxWords
import SwiftUI

/// One machine, opened from the hosts sidebar: how this Mac reaches it,
/// what it is, the agents it runs, and for a paired machine, stopping
/// trusting it.
public struct HostPage: View {
    let host: HostView
    /// Its agents, as the fleet lists them.
    let agents: [FleetRow]
    let now: Date
    let openAgent: @MainActor (AgentKey) -> Void
    let toggleFamily: @MainActor (AgentKey) -> Void
    let revoke: @MainActor ([UInt8]) -> Void
    @State private var confirming = false

    public init(
        host: HostView, agents: [FleetRow], now: Date = Date(),
        openAgent: @escaping @MainActor (AgentKey) -> Void = { _ in },
        toggleFamily: @escaping @MainActor (AgentKey) -> Void = { _ in },
        revoke: @escaping @MainActor ([UInt8]) -> Void = { _ in }
    ) {
        self.host = host
        self.agents = agents
        self.now = now
        self.openAgent = openAgent
        self.toggleFamily = toggleFamily
        self.revoke = revoke
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 26) {
                HStack(spacing: 14) {
                    HostTile(host: host, size: 52)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(PlaceNames.host(host.name)).font(.ui(.heading, weight: .semibold))
                        Text(HostWords.reach(host)).foregroundStyle(.secondary)
                    }
                }
                if let meaning = HostWords.meaning(host) {
                    Text(meaning).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
                group("Machine") {
                    if let platform = HostWords.platform(host) { fact("Platform", platform) }
                    if let version = host.version { fact("amux", version) }
                    if !HostWords.online(host), !host.local, host.reach != .offline(.revoked),
                        let error = host.lastDialError, !error.isEmpty
                    {
                        fact("Last try", "Could not connect")
                    }
                }
                let providers = host.providers.filter { $0.provider != .unspecified }
                if !providers.isEmpty {
                    group("Providers") {
                        ForEach(Array(providers.enumerated()), id: \.offset) { _, signIn in
                            fact(signIn.provider == .codex ? "Codex" : "Claude", signIn.signedIn ? "Signed in" : "Not signed in",
                                 quiet: !signIn.signedIn)
                        }
                    }
                }
                if !host.addrs.isEmpty {
                    group("Network") {
                        ForEach(host.addrs, id: \.self) { address in
                            Text(address)
                                .font(.mono(.callout))
                                .textSelection(.enabled)
                                .padding(.horizontal, 14)
                                .frame(maxWidth: .infinity, minHeight: 36, alignment: .leading)
                        }
                    }
                }
                group(agents.isEmpty ? "Agents" : "Agents · \(agents.count)") {
                    if agents.isEmpty {
                        Text("No agents on it").foregroundStyle(.secondary)
                            .padding(.horizontal, 14)
                            .frame(maxWidth: .infinity, minHeight: 40, alignment: .leading)
                    } else {
                        ForEach(agents) { row in
                            FleetRowView(row: row, now: now, selected: false, local: host.local, ground: Palette.well) {
                                toggleFamily(row.card.agent)
                            }
                            .padding(.horizontal, 4)
                            .onTapGesture { openAgent(row.card.agent) }
                        }
                        .padding(.vertical, 2)
                    }
                }
                if !host.local {
                    Button("Stop Trusting \(PlaceNames.host(host.name))…") { confirming = true }
                        .buttonStyle(PillButtonStyle(destructive: true))
                        .confirmationDialog(
                            "Stop trusting \(PlaceNames.host(host.name))?", isPresented: $confirming
                        ) {
                            Button("Stop Trusting", role: .destructive) { revoke(host.hostId) }
                        } message: {
                            Text("This Mac stops reaching it and its agents leave the list. Only pairing again brings it back.")
                        }
                }
            }
            .font(.ui(.body))
            .frame(maxWidth: 620, alignment: .leading)
            .padding(.horizontal, Metrics.gutter)
            .padding(.vertical, 28)
            .frame(maxWidth: .infinity)
        }
    }

    private func fact(_ label: String, _ value: String, quiet: Bool = false) -> some View {
        HStack {
            Text(label)
            Spacer(minLength: 12)
            Text(value).foregroundStyle(quiet ? .tertiary : .secondary).textSelection(.enabled)
        }
        .padding(.horizontal, 14)
        .frame(minHeight: 36)
    }

    /// A titled group of lines on one surface, split by hairlines, as a
    /// desktop settings pane groups them.
    private func group(_ title: String, @ViewBuilder _ content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.ui(.meta, weight: .medium)).foregroundStyle(.secondary).padding(.leading, 2)
            Separated { content() }
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).fill(Palette.well))
                .overlay(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).strokeBorder(Palette.line))
        }
    }
}

/// Lines with a hairline between each.
private struct Separated<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        VStack(spacing: 0) {
            Group(subviews: content) { lines in
                ForEach(Array(lines.enumerated()), id: \.offset) { index, line in
                    if index > 0 {
                        Rectangle().fill(Palette.line).frame(height: 1).padding(.leading, 14)
                    }
                    line
                }
            }
        }
    }
}

extension HostWords {
    static func platform(_ host: HostView) -> String? {
        switch host.platform {
        case .macos: "macOS"
        case .linux: "Linux"
        case .windows: "Windows"
        case .ios: "iOS"
        case .unspecified, nil: nil
        }
    }

    /// What being away or offline means for its agents.
    static func meaning(_ host: HostView) -> String? {
        switch host.reach {
        case .online: nil
        case .away:
            "The relay can see it. The free plan does not relay between hosts, so reaching it needs a subscription."
        case .offline(.revoked): "It no longer trusts this Mac. Pair again to reach it."
        case .offline: host.local ? nil : "Its agents report their state as unknown until it is back."
        }
    }
}
