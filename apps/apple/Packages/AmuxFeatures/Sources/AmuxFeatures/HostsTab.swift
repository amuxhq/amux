import AmuxCore
import AmuxDesign
import SwiftUI

/// What happened on the Hosts tab. The screen navigates nowhere and reaches
/// nothing; it says what the person did and the shell decides where that leads.
public enum HostsAction: Equatable, Sendable {
    case open(HostId)
    /// Start pairing with a machine this phone has found. Pairing is always
    /// with one machine: a code typed for no machine has nowhere to go.
    case pair(HostId)
    case newAgent
    /// Stop trusting a machine, once the person has confirmed it: what it
    /// ends is the access this phone granted, not a preference.
    case revoke(HostId)
    /// Take the person to this app's own page in the system's settings, which
    /// is the only place a refused local network can be granted again. The app
    /// cannot ask a second time.
    case openSystemSettings
}

/// The machines agents run on.
///
/// One list, the machines something can be started on first. Each row says
/// what the machine is and how this phone is reaching it, because that is the
/// explanation for latency a person would otherwise blame on the app: a relay
/// hop feels different from a link on the same desk, and there is nothing the
/// app can do about it except say so. A machine that cannot be reached says
/// so on its row, and its page says why and what that means for its agents.
///
/// Machines this phone has found and not paired with are cards above the
/// list, as on the agent list: what there is to do with one is pair it.
/// Signing in adds the relay's machines to what can be found; it is not what
/// makes finding possible.
public struct HostsTab: View {
    @Environment(\.design) private var design
    private let model: HostsStore
    private let actions: @MainActor (HostsAction) -> Void

    public init(model: HostsStore, actions: @escaping @MainActor (HostsAction) -> Void) {
        self.model = model
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            if model.hosts.isEmpty && model.discovered.isEmpty {
                empty
            } else {
                list
            }
        }
        .navigationTitle("Hosts")
        .navigationSubtitle(subtitle)
        .toolbar {
            ToolbarItem(placement: .largeSubtitle) {
                LargeSubtitle(text: subtitle, identifier: "hosts.subtitle")
            }
        }
        // A screen is a container of the things on it, not a name for all of
        // them. Without this the system spreads this identifier over every
        // element underneath — the title, the buttons, the rows — so
        // everything on the screen answers to the screen's own name, for
        // VoiceOver and for anything driving the app alike.
        .accessibilityElement(children: .contain)
        .identified("hosts", value: subtitle)
    }

    /// "2 reachable · 1 away · 2 offline", and only the parts that are true.
    ///
    /// Reachable counts the machines something can actually be started on.
    /// An away machine is counted apart rather than folded into either: it is
    /// not reachable, and calling it offline would claim it is not there.
    private var subtitle: String {
        guard !model.hosts.isEmpty else { return "" }
        let reachable = model.hosts(.onThisNetwork).count + model.hosts(.throughTheRelay).count
        let away = model.hosts(.away).count
        let lost = model.hosts(.offline).count
        var parts: [String] = []
        if reachable > 0 || (away == 0 && lost == 0) { parts.append(String(localized: "\(reachable) reachable")) }
        if away > 0 { parts.append(String(localized: "\(away) away")) }
        if lost > 0 { parts.append(String(localized: "\(lost) offline")) }
        return parts.joined(separator: " · ")
    }

    // MARK: - The list

    private var list: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                // First, because it is the reason the list under it is short:
                // nothing found on a network nobody let this app look at is
                // not a fact about the network.
                if model.localNetwork == .denied {
                    refusedNetwork.padding(.horizontal, design.metrics.gutter)
                }
                if !callouts.isEmpty { CalloutStrip(callouts: callouts, act: actions) }
                if !model.hosts.isEmpty {
                    RowGroup(items: ordered) { host in row(host) }
                        .padding(.horizontal, design.metrics.gutter)
                }
            }
            .padding(.top, 6)
            .padding(.bottom, 120)
        }
        .scrollIndicators(.hidden)
    }

    /// Reachable first, nearest route first, then away, then offline; by
    /// name within each.
    private var ordered: [HostView] {
        model.hosts.sorted {
            (Self.rank($0.group), $0.name.lowercased()) < (Self.rank($1.group), $1.name.lowercased())
        }
    }

    private static func rank(_ group: HostGroup) -> Int {
        HostGroup.all.firstIndex(of: group) ?? HostGroup.all.count
    }

    /// The machines found and not paired, as cards with the one thing there
    /// is to do with one.
    private var callouts: [Callout<HostsAction>] {
        model.discovered.compactMap { host in
            guard let id = host.id else { return nil }
            let nearby = host.group == .onThisNetwork
            return Callout(
                id: "pair.\(host.name)", glyph: "plus.circle",
                title: nearby
                    ? String(localized: "\(host.name) is nearby")
                    : String(localized: "\(host.name) can be paired"),
                detail: String(localized: "Pair it to see its agents here"),
                button: String(localized: "Pair"), act: .pair(id),
                identifier: "hosts.pair.\(key(host))")
        }
    }

    private func row(_ host: HostView) -> some View {
        let reachable = host.online
        return Button {
            if let id = host.id { actions(.open(id)) }
        } label: {
            HStack(spacing: 11) {
                Image(systemName: glyph(host))
                    .font(.system(size: 15, weight: .medium))
                    .foregroundStyle(reachable ? design.inkMuted.color : design.inkFaint.color)
                    .frame(width: 22)
                VStack(alignment: .leading, spacing: 2) {
                    Text(host.name)
                        .designFont(.identifier, design)
                        .foregroundStyle(reachable ? design.ink.color : design.inkMuted.color)
                    Text(status(host))
                        .designFont(.detail, design)
                        .foregroundStyle(design.inkMuted.color)
                }
                Spacer(minLength: 6)
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .frame(minHeight: 44)
        }
        .buttonStyle(.amuxPush)
        .accessibilityLabel(spoken(host))
        .identified(
            "hosts.row.\(key(host))", label: spoken(host), value: host.group.name)
    }

    /// What a driver names a host's row by.
    private func key(_ host: HostView) -> String {
        host.id?.description ?? host.name
    }

    /// The machine drawn as the kind of machine it said it was. A host that
    /// has not said gets the plain computer rather than a guess, because the
    /// glyph is the first thing read and a wrong one is a wrong answer.
    private func glyph(_ host: HostView) -> String {
        switch host.platform {
        case "Mac Studio": "desktopcomputer"
        case "Mac mini": "macmini"
        case "MacBook Air": "laptopcomputer"
        case "Linux": "server.rack"
        case "Windows": "pc"
        default: "desktopcomputer"
        }
    }

    /// The second line: what the machine is, and how this phone stands with
    /// it — "direct" for a link with nothing in between, "via relay" for one
    /// that crosses it, or why it cannot be reached. An offline machine says
    /// how long it has been gone where this phone watched it go.
    func status(_ host: HostView) -> String {
        var parts: [String] = []
        if let platform = host.platform { parts.append(platform) }
        switch host.group {
        case .onThisNetwork: parts.append(String(localized: "direct"))
        case .throughTheRelay: parts.append(String(localized: "via relay"))
        case .away: parts.append(String(localized: "away on the free plan"))
        case .offline:
            if host.reach == .offline(.revoked) {
                parts.append(String(localized: "no longer trusts this phone"))
            } else if let id = host.id, model.foundButUnreachable.contains(id) {
                parts.append(String(localized: "found, not answering"))
            // Said because it settles which of two very different things is
            // wrong: a machine that never signed in cannot be reached from
            // anywhere but its own network whatever anybody buys.
            } else if host.reach == .offline(.signedOut) {
                parts.append(String(localized: "offline, this phone is signed out"))
            } else if let id = host.id, let gone = model.wentOffline(id) {
                parts.append(String(localized: "offline for \(since(gone))"))
            } else {
                parts.append(String(localized: "offline"))
            }
        }
        return parts.joined(separator: " · ")
    }

    /// How long it has been gone, in the same units every other age in the
    /// app is written in. An age rather than a wall-clock time because the
    /// machine did not report when it went — this phone noticed, and "for 3d"
    /// stays true where "08:12" would read as this morning.
    private func since(_ moment: Date) -> String {
        Elapsed.spelled(max(0, model.now.timeIntervalSince(moment)))
    }

    /// What a row says to somebody who cannot see it, in the order the row
    /// says it: which machine, what it is, and how it stands.
    private func spoken(_ host: HostView) -> String {
        var parts = [host.name]
        if let platform = host.platform { parts.append(platform) }
        switch host.group {
        case .onThisNetwork: parts.append(String(localized: "reachable on this network"))
        case .throughTheRelay: parts.append(String(localized: "reachable through the relay"))
        case .away:
            parts.append(String(localized: "away"))
            parts.append(String(localized: "the free plan does not relay between hosts"))
        case .offline:
            if host.reach == .offline(.revoked) {
                parts.append(String(localized: "it no longer trusts this phone, pair again to reach it"))
            } else if let id = host.id, model.foundButUnreachable.contains(id) {
                parts.append(String(localized: "found on this network and not answering"))
            } else if host.reach == .offline(.signedOut) {
                parts.append(String(localized: "offline while this phone is signed out"))
            } else if let id = host.id, let gone = model.wentOffline(id) {
                parts.append(String(localized: "offline for \(since(gone))"))
            } else {
                parts.append(String(localized: "offline"))
            }
            parts.append(String(localized: "its agents’ state is unknown"))
        }
        return parts.joined(separator: ", ")
    }

    private var refusedNetwork: some View {
        LocalNetworkRefused(heading: model.hosts.isEmpty, identifier: "hosts.localNetwork") {
            actions(.openSystemSettings)
        }
    }

    /// Nothing paired and nothing found, drawn as the agent list's first run
    /// is. It is not an error and it is not a failure of this phone: amux is
    /// free on the network it is on, and what has not happened yet is a host
    /// running there.
    ///
    /// When the network was refused, that is the headline instead: "no hosts
    /// yet" would be a claim this app is in no position to make. Pairing from
    /// a code shown on the computer still works either way.
    private var empty: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                if model.localNetwork == .denied {
                    refusedNetwork
                } else {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("No hosts yet")
                            .designFont(.screenTitle, design)
                            .foregroundStyle(design.ink.color)
                            .identified("hosts.empty", value: "No hosts yet")
                        Explain("A computer running amux on this network appears here, ready to pair.")
                    }
                }
                PairingHint()
                    .identified("hosts.empty.howToPair")
            }
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 30)
            .padding(.bottom, 120)
        }
        .scrollIndicators(.hidden)
    }
}
