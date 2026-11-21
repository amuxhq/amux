import AmuxValues
import AmuxWords
import AppKit
import SwiftUI

/// One machine, opened from the hosts sidebar: how this Mac reaches it,
/// what it is, the agents it runs, and for a paired machine, stopping
/// trusting it.
public struct HostPage: View {
    let host: HostView
    /// Its threads of work, as the sidebar lists them: a family's head
    /// stands for its members.
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
        self.agents = agents.filter { $0.depth == 0 }
        self.now = now
        self.openAgent = openAgent
        self.revoke = revoke
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 26) {
                // The window's bar names the host: the page opens on what
                // it is and how it is reached, in one line.
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 8) {
                        Icon(HostWords.symbol(host), size: 14)
                        Text(summary).textSelection(.enabled)
                    }
                    .foregroundStyle(Palette.textSecondary)
                    if let meaning = HostWords.meaning(host) {
                        Text(meaning).foregroundStyle(Palette.textSecondary).fixedSize(horizontal: false, vertical: true)
                    }
                }
                let providers = host.providers.filter { !$0.provider.isEmpty }
                if !providers.isEmpty {
                    group("Coding tools") {
                        ForEach(Array(providers.enumerated()), id: \.offset) { _, signIn in
                            fact(signIn.name, signIn.signedIn ? "Signed in" : "Not signed in",
                                 quiet: !signIn.signedIn)
                        }
                    }
                }
                if !host.addrs.isEmpty {
                    group(host.addrs.count == 1 ? "Address" : "Addresses") {
                        ForEach(host.addrs, id: \.self) { address in
                            AddressLine(address: address)
                        }
                    }
                }
                group(agents.isEmpty ? "Agents" : "Agents · \(agents.count)") {
                    if agents.isEmpty {
                        Text("No agents yet").foregroundStyle(Palette.textSecondary)
                            .padding(.horizontal, 14)
                            .frame(maxWidth: .infinity, minHeight: 40, alignment: .leading)
                    } else {
                        ForEach(agents) { row in
                            FleetRowView(row: row, now: now, selected: false, hostShown: true, ground: Palette.card)
                            .padding(.horizontal, 4)
                            .onTapGesture { openAgent(row.card.agent) }
                        }
                        .padding(.vertical, 2)
                    }
                }
                if !host.local {
                    Button("Unpair \(PlaceNames.host(host.name))…") { confirming = true }
                        .buttonStyle(PillButtonStyle(destructive: true))
                        .confirmationDialog(
                            "Unpair \(PlaceNames.host(host.name))?", isPresented: $confirming
                        ) {
                            Button("Unpair", role: .destructive) { revoke(host.hostId) }
                        } message: {
                            Text("This Mac won’t connect to it anymore, and its agents leave your list. To undo this, pair again.")
                        }
                }
            }
            .font(.ui(.body))
            .frame(maxWidth: 620, alignment: .leading)
            .padding(.horizontal, Metrics.gutter)
            .padding(.vertical, 24)
            .frame(maxWidth: .infinity)
        }
    }

    /// "Connected on this network · Linux · amux 0.9.0".
    private var summary: String {
        var reach = HostWords.reach(host)
        if !HostWords.online(host), !host.local, host.reach != .offline(.revoked),
            let error = host.lastDialError, !error.isEmpty
        {
            reach += " · last try failed"
        }
        let version = host.version.map { "amux \($0)" }
        return [reach, HostWords.platform(host), version].compactMap { $0 }.joined(separator: " · ")
    }

    private func fact(_ label: String, _ value: String, quiet: Bool = false) -> some View {
        HStack {
            Text(label)
            Spacer(minLength: 12)
            Text(value).foregroundStyle(quiet ? Palette.textTertiary : Palette.textSecondary).textSelection(.enabled)
        }
        .padding(.horizontal, 14)
        .frame(minHeight: 36)
    }

    /// A titled group of lines on one surface, split by hairlines, as a
    /// desktop settings pane groups them.
    private func group(_ title: String, @ViewBuilder _ content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.ui(.meta, weight: .medium)).foregroundStyle(Palette.textSecondary).padding(.leading, 2)
            Separated { content() }
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).fill(Palette.card))
                .overlay(RoundedRectangle(cornerRadius: Radius.card, style: .continuous).strokeBorder(Palette.line))
        }
    }
}

/// One of a host's addresses: what kind it is, the address, and a copy
/// button while the pointer is on the line.
private struct AddressLine: View {
    let address: String
    @State private var hovering = false
    @State private var copied = false

    var body: some View {
        // The copy button stands before the address while the pointer is
        // on the line, so the address ends where every other value does.
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(kind)
            Spacer(minLength: 12)
            if hovering || copied {
                Button(action: copy) {
                    Icon(copied ? "check" : "copy", size: 14)
                        .foregroundStyle(Palette.textSecondary)
                        .frame(width: 24, height: 24)
                        .contentShape(Rectangle())
                }
                .buttonStyle(HoverButtonStyle())
                .help(copied ? "Copied" : "Copy address")
                .accessibilityLabel("Copy \(address)")
                .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 4.5 }
            }
            Text(address)
                .font(.mono(.callout))
                .foregroundStyle(Palette.textSecondary)
                .textSelection(.enabled)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .padding(.horizontal, 14)
        .frame(minHeight: 36)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }

    /// IPv6 when it is bracketed or has more than one colon, IPv4 when it
    /// is dotted digits, a hostname otherwise.
    private var kind: String {
        if address.hasPrefix("[") || address.split(separator: ":").count > 2 { return "IPv6" }
        let dotted = (address.split(separator: ":").first ?? "").split(separator: ".")
        if dotted.count == 4, dotted.allSatisfy({ UInt8($0) != nil }) { return "IPv4" }
        return "Hostname"
    }

    private func copy() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(address, forType: .string)
        copied = true
        Task {
            try? await Task.sleep(for: .seconds(1.5))
            copied = false
        }
    }
}

/// Lines with a hairline between each.
struct Separated<Content: View>: View {
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
            "It’s online, but the free plan doesn’t connect computers to each other over the internet. Reaching it from here needs Pro."
        case .offline(.revoked): "It unpaired this Mac. Pair again to reach it."
        case .offline: host.local ? nil : "Its agents’ status updates when it’s back online."
        }
    }
}
