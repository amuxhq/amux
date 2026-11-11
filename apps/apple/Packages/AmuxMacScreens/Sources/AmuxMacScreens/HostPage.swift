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
    let revoke: @MainActor ([UInt8]) -> Void
    @State private var confirming = false

    public init(
        host: HostView, agents: [FleetRow], now: Date = Date(),
        openAgent: @escaping @MainActor (AgentKey) -> Void = { _ in },
        revoke: @escaping @MainActor ([UInt8]) -> Void = { _ in }
    ) {
        self.host = host
        self.agents = agents
        self.now = now
        self.openAgent = openAgent
        self.revoke = revoke
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 10) {
                        Image(systemName: HostWords.symbol(host))
                            .font(.system(size: 22))
                            .foregroundStyle(.secondary)
                        Text(PlaceNames.host(host.name)).font(.system(size: 22, weight: .semibold))
                    }
                    Text(HostWords.reach(host)).foregroundStyle(.secondary)
                    if let meaning = HostWords.meaning(host) {
                        Text(meaning).font(.system(size: 12.5)).foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                facts
                section("Agents") {
                    if agents.isEmpty {
                        Text("No agents on it").foregroundStyle(.secondary).padding(12)
                    } else {
                        VStack(spacing: 1) {
                            ForEach(agents) { row in
                                FleetRowView(row: row, now: now, selected: false, toggleFamily: {})
                                    .onTapGesture { openAgent(row.card.agent) }
                            }
                        }
                        .padding(4)
                    }
                }
                if !host.local {
                    Button("Stop Trusting \(PlaceNames.host(host.name))…", role: .destructive) { confirming = true }
                        .confirmationDialog(
                            "Stop trusting \(PlaceNames.host(host.name))?", isPresented: $confirming
                        ) {
                            Button("Stop Trusting", role: .destructive) { revoke(host.hostId) }
                        } message: {
                            Text("This Mac stops reaching it and its agents leave the list. Only pairing again brings it back.")
                        }
                }
            }
            .font(.system(size: 13))
            .frame(maxWidth: 620, alignment: .leading)
            .padding(.horizontal, 32)
            .padding(.vertical, 24)
            .frame(maxWidth: .infinity)
        }
    }

    private var facts: some View {
        section(nil) {
            Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 10) {
                if let platform = HostWords.platform(host) { fact("Platform", platform) }
                if let version = host.version { fact("Version", version, mono: true) }
                ForEach(Array(host.providers.enumerated()), id: \.offset) { _, signIn in
                    if signIn.provider != .unspecified {
                        fact(signIn.provider == .codex ? "Codex" : "Claude", signIn.signedIn ? "Signed in" : "Not signed in")
                    }
                }
                if !HostWords.online(host), !host.local, host.reach != .offline(.revoked),
                    let error = host.lastDialError, !error.isEmpty
                {
                    fact("Last try", "Could not connect")
                }
                if !host.addrs.isEmpty { fact("Addresses", host.addrs.joined(separator: "\n"), mono: true) }
            }
            .padding(14)
        }
    }

    private func fact(_ label: String, _ value: String, mono: Bool = false) -> some View {
        GridRow(alignment: .firstTextBaseline) {
            Text(label).foregroundStyle(.secondary)
            Text(value)
                .font(mono ? .system(size: 12, design: .monospaced) : .system(size: 13))
                .textSelection(.enabled)
        }
    }

    private func section(_ title: String?, @ViewBuilder _ content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            if let title {
                Text(title).font(.system(size: 12, weight: .medium)).foregroundStyle(.secondary)
            }
            content()
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Palette.hover))
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
