import AmuxCore
import AmuxDesign
import SwiftUI

/// What the host page asks of whoever pushed it.
public enum HostPageAction: Equatable, Sendable {
    case back
    /// Stop trusting the machine, once the person has confirmed it.
    case revoke(HostId)
    /// Pair with a machine that no longer trusts this phone.
    case pairAgain(HostId)
}

/// One paired machine, opened from its row on Hosts: how this phone reaches
/// it, what it is, which agents it runs, and its key in full.
///
/// The row already says where the machine is; this page is for the facts a
/// row has no room for, for what being away or offline means for its agents,
/// and for what there is to do about a machine here: pair again with one that
/// no longer trusts this phone, and stop trusting it. Revoking is the last
/// row and asks first, because it cannot be undone from this phone: only
/// pairing again brings the machine back.
public struct HostPage: View {
    @Environment(\.design) private var design
    private let model: HostsStore
    private let host: HostId
    private let actions: @MainActor (HostPageAction) -> Void
    @State private var confirming: Bool

    public init(
        model: HostsStore, host: HostId, confirming: Bool = false,
        actions: @escaping @MainActor (HostPageAction) -> Void
    ) {
        self.model = model
        self.host = host
        self.actions = actions
        _confirming = State(initialValue: confirming)
    }

    public var body: some View {
        ZStack(alignment: .topLeading) {
            Ground()
            ScrollView {
                if let view = model.host(host) {
                    content(view)
                } else {
                    // Revoked here or elsewhere: the machine has left the list
                    // this page was opened from.
                    Explain(String(localized: "This host is no longer paired with this phone."))
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, design.metrics.gutter)
                        .padding(.top, 8)
                        .identified("host.gone")
                }
            }
            .scrollIndicators(.hidden)
        }
        .navigationBarBackButtonHidden(true)
        .toolbarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                BarBack(identifier: "host.back") { actions(.back) }
            }
        }
        .accessibilityElement(children: .contain)
        .identified("host", value: model.host(host)?.name ?? "")
    }


    private func content(_ view: HostView) -> some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 4) {
                Text(view.name)
                    .designFont(.screenTitle, design)
                    .foregroundStyle(design.ink.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .identified("host.name", value: view.name)
                Text(Self.reach(view))
                    .designFont(.detail, design)
                    .foregroundStyle(design.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .identified("host.reach", value: view.group.name)
                if let meaning = meaning(view) {
                    Explain(meaning)
                        .padding(.top, 4)
                        .identified("host.meaning", value: meaning)
                }
            }
            // A machine that dropped this phone is still on its list; pairing
            // again is the way back, and the first thing to offer.
            if view.reach == .offline(.revoked), let id = view.id {
                Button { actions(.pairAgain(id)) } label: {
                    ActionLabel(String(localized: "Pair Again"), kind: .primary, fill: true)
                }
                .buttonStyle(.amuxControl)
                .accessibilityLabel(String(localized: "Pair Again with \(view.name)"))
                .identified("host.pairAgain", label: String(localized: "Pair Again"))
            }
            Surface {
                VStack(spacing: 0) {
                    ForEach(Array(facts(view).enumerated()), id: \.element.id) { index, fact in
                        if index > 0 { rule }
                        factRow(fact)
                    }
                }
            }
            Surface {
                Button { confirming = true } label: {
                    FieldRow(
                        label: String(localized: "Revoke This Host"), chevron: false,
                        tint: design.removed.color)
                }
                .buttonStyle(.amuxRow)
                .accessibilityLabel(String(localized: "Revoke \(view.name)"))
                .identified("host.revoke", label: String(localized: "Revoke \(view.name)"))
                // On the row, not the page: the system draws the dialog as a
                // popover pointing at the view that holds it.
                .confirmationDialog(
                    String(localized: "Revoke \(view.name)?"), isPresented: $confirming,
                    titleVisibility: .visible
                ) {
                    Button(String(localized: "Revoke"), role: .destructive) { actions(.revoke(host)) }
                        .accessibilityIdentifier("host.revoke.confirm")
                } message: {
                    Text("This phone stops reaching it and its agents leave this phone’s list. Only pairing again brings it back.")
                }
            }
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.top, 8)
        .padding(.bottom, 40)
    }

    /// Under the row's words, not its edge, as Settings rules its rows.
    private var rule: some View {
        Rectangle()
            .fill(design.hairline.color)
            .frame(height: design.metrics.hairline)
            .padding(.leading, 14)
    }

    /// What being away or offline means for the machine's agents, said on
    /// the machine's own page rather than under every group of a list.
    func meaning(_ view: HostView) -> String? {
        switch view.group {
        case .onThisNetwork, .throughTheRelay: return nil
        // Stated, not sold: the relay can see it and this account cannot
        // open a tunnel to it. The offer belongs where somebody is trying to
        // use it.
        case .away:
            return String(localized: "The relay can see it. The free plan does not relay between hosts, so reaching it needs a subscription.")
        case .offline:
            if view.reach == .offline(.revoked) { return nil }
            if let id = view.id, model.foundButUnreachable.contains(id) {
                return String(localized: "Its agents report their state as unknown. It is on this network and will not answer, which a network that blocks amux would explain.")
            }
            return String(localized: "Its agents report their state as unknown until it is back.")
        }
    }

    /// A fact on one line beside its label, or under it when it runs to
    /// several, as an address list or a key does.
    @ViewBuilder
    private func factRow(_ fact: Fact) -> some View {
        Group {
            if fact.stacked {
                VStack(alignment: .leading, spacing: 4) {
                    Text(fact.label)
                        .designFont(.body, design)
                        .foregroundStyle(design.ink.color)
                    Text(fact.value)
                        .designFont(fact.mono ? .monoSmall : .body, design)
                        .foregroundStyle(design.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
            } else {
                FieldRow(label: fact.label, value: fact.value, mono: fact.mono, chevron: false)
            }
        }
        .accessibilityElement(children: .combine)
        .identified("host.fact.\(fact.id)", label: fact.label, value: fact.value)
    }

    private struct Fact: Identifiable {
        let id: String
        let label: String
        let value: String
        var mono = false
        var stacked = false
    }

    private func facts(_ view: HostView) -> [Fact] {
        var facts: [Fact] = []
        if let platform = view.platform?.name {
            facts.append(Fact(id: "platform", label: String(localized: "Platform"), value: platform))
        }
        if let version = view.version {
            facts.append(Fact(id: "version", label: String(localized: "Version"), value: version, mono: true))
        }
        for signIn in view.providers where signIn.provider != .unspecified {
            let name = signIn.provider == .codex ? "Codex" : "Claude"
            facts.append(Fact(
                id: "provider.\(name.lowercased())", label: name,
                value: signIn.signedIn ? String(localized: "Signed in") : String(localized: "Not signed in")))
        }
        // The dial's own error names an address and a status code, which say
        // nothing a person can act on. A machine that dropped this phone has
        // its reason said above it; any other failed dial is said in words.
        if !view.online, view.reach != .offline(.revoked), let error = view.lastDialError, !error.isEmpty {
            facts.append(Fact(
                id: "last-error", label: String(localized: "Last try"),
                value: String(localized: "Could not connect")))
        }
        if !view.addrs.isEmpty {
            facts.append(Fact(
                id: "addresses", label: String(localized: "Addresses"),
                value: view.addrs.joined(separator: "\n"), mono: true, stacked: true))
        }
        if let peer = model.devices.first(where: { $0.hostId == view.hostId }) {
            facts.append(Fact(
                id: "fingerprint", label: String(localized: "Fingerprint"),
                value: Fingerprint.grouped(peer.fingerprint), mono: true, stacked: true))
        }
        return facts
    }

    /// How this phone stands with the machine, in the words the rows use.
    static func reach(_ view: HostView) -> String {
        switch view.group {
        case .onThisNetwork: return String(localized: "Reachable on this network, direct")
        case .throughTheRelay: return String(localized: "Reachable through the relay")
        // The daemon lists a host away only when the relay refused to
        // connect to it because the free plan relays nothing between hosts.
        case .away: return String(localized: "Away on the free plan")
        case .offline:
            switch view.reach {
            case .offline(.revoked): return String(localized: "Offline · it no longer trusts this phone")
            case .offline(.signedOut): return String(localized: "Offline while this phone is signed out")
            default: return String(localized: "Offline")
            }
        }
    }
}
