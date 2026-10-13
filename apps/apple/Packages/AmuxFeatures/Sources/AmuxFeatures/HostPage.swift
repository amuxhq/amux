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
/// it, what it is, which providers it runs, and its key in full.
///
/// The row already says where the machine is; this page is for the facts a
/// row has no room for, and for what there is to do about a machine here:
/// pair again with one that no longer trusts this phone, and stop trusting
/// it. Revoking asks first, because it cannot be undone from this phone:
/// only pairing again brings the machine back.
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
            VStack(alignment: .leading, spacing: 0) {
                BackLink("Hosts", identifier: "host.back") { actions(.back) }
                    .padding(.horizontal, design.metrics.gutter)
                    .padding(.vertical, 10)
                ScrollView {
                    if let view = model.host(host) {
                        content(view)
                    } else {
                        // Revoked here or elsewhere: the machine has left the list
                        // this page was opened from.
                        Explain(String(localized: "This host is no longer paired with this phone."))
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.horizontal, design.metrics.gutter)
                            .identified("host.gone")
                    }
                }
                .scrollIndicators(.hidden)
            }
        }
        .accessibilityElement(children: .contain)
        .identified("host", value: model.host(host)?.name ?? "")
    }

    private func content(_ view: HostView) -> some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 2) {
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
            }
            RowGroup(items: facts(view)) { fact in
                VStack(alignment: .leading, spacing: 2) {
                    Text(fact.label)
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                    Text(fact.value)
                        .designFont(fact.mono ? .monoSmall : .body, design)
                        .foregroundStyle(design.ink.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .accessibilityElement(children: .combine)
                .identified("host.fact.\(fact.id)", label: fact.label, value: fact.value)
            }
            // A machine that dropped this phone is still on its list; pairing
            // again is the way back, and the first thing to offer.
            if view.reach == .away(.revoked), let id = view.id {
                Button { actions(.pairAgain(id)) } label: {
                    ActionLabel(String(localized: "Pair Again"), kind: .primary, fill: true)
                }
                .buttonStyle(.amuxControl)
                .accessibilityLabel(String(localized: "Pair Again with \(view.name)"))
                .identified("host.pairAgain", label: String(localized: "Pair Again"))
            }
            if confirming {
                confirm(view)
            } else {
                Button { confirming = true } label: {
                    ActionLabel(String(localized: "Revoke"), kind: .destructive, fill: true)
                }
                .buttonStyle(.amuxControl)
                .accessibilityLabel(String(localized: "Revoke \(view.name)"))
                .identified("host.revoke", label: String(localized: "Revoke \(view.name)"))
            }
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.top, 4)
        .padding(.bottom, 40)
    }

    /// Revoking, with what it ends said first.
    private func confirm(_ view: HostView) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(String(localized: "Revoke \(view.name)?"))
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
            Consequence("xmark", kept: false, String(localized: "This phone stops reaching it, and its agents leave this phone’s list."))
            Consequence("xmark", kept: false, String(localized: "Only pairing again brings it back."))
            ButtonPair {
                choiceButton(String(localized: "Cancel"), kind: .outline, id: "host.revoke.cancel") {
                    confirming = false
                }
                choiceButton(String(localized: "Revoke"), kind: .destructive, id: "host.revoke.confirm") {
                    confirming = false
                    actions(.revoke(host))
                }
            }
        }
        .padding(16)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .identified("host.revoke.card", label: String(localized: "Revoke \(view.name)?"))
    }

    private struct Fact: Identifiable {
        let id: String
        let label: String
        let value: String
        var mono = false
    }

    private func facts(_ view: HostView) -> [Fact] {
        var facts: [Fact] = []
        if let platform = view.platform {
            facts.append(Fact(id: "platform", label: String(localized: "Platform"), value: platform))
        }
        if let version = view.version {
            facts.append(Fact(id: "version", label: String(localized: "Version"), value: version, mono: true))
        }
        let runs = view.providers.filter { $0.provider != .unspecified }
        if !runs.isEmpty {
            facts.append(Fact(
                id: "providers", label: String(localized: "Runs"),
                value: runs.map(Self.provider).joined(separator: "\n")))
        }
        if !view.addrs.isEmpty {
            facts.append(Fact(
                id: "addresses", label: String(localized: "Addresses"),
                value: view.addrs.joined(separator: "\n"), mono: true))
        }
        // The dial's own error names an address and a status code, which say
        // nothing a person can act on. A machine that dropped this phone has
        // its reason said above it; any other failed dial is said in words.
        if !view.online, view.reach != .away(.revoked), let error = view.lastDialError, !error.isEmpty {
            facts.append(Fact(
                id: "last-error", label: String(localized: "Last try"),
                value: String(localized: "Could not connect")))
        }
        if let peer = model.devices.first(where: { $0.hostId == view.hostId }) {
            facts.append(Fact(
                id: "fingerprint", label: String(localized: "Fingerprint"),
                value: Fingerprint.grouped(peer.fingerprint), mono: true))
        }
        return facts
    }

    private static func provider(_ signIn: ProviderSignIn) -> String {
        let name = signIn.provider == .codex ? "Codex" : "Claude"
        return signIn.signedIn
            ? String(localized: "\(name) · signed in") : String(localized: "\(name) · not signed in")
    }

    /// How this phone stands with the machine, in the words the rows use.
    static func reach(_ view: HostView) -> String {
        switch view.group {
        case .onThisNetwork: return String(localized: "Reachable on this network, direct")
        case .throughTheRelay: return String(localized: "Reachable through the relay")
        // The daemon lists a host away only when the relay alone sees it and
        // the account is on the free plan, which relays nothing between hosts.
        case .away: return String(localized: "Away · the free plan does not relay between hosts")
        case .offline:
            switch view.reach {
            case .away(.revoked): return String(localized: "Offline · it no longer trusts this phone")
            case .away(.signedOut): return String(localized: "Offline while this phone is signed out")
            default: return String(localized: "Offline")
            }
        }
    }
}
