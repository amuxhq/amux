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
/// Each row says what the machine is and how this phone is reaching it,
/// because that is the explanation for latency a person would otherwise blame
/// on the app: a relay hop feels different from a link on the same desk, and
/// there is nothing the app can do about it except say so.
///
/// The groups are where a machine is, not how good it is. On this network,
/// through the relay, away and offline are four different things you can do
/// with a machine rather than four shades of one; a machine you cannot use is
/// not a worse version of one you can — nothing you start on it will run — and
/// the agents already on it are in a state nobody can report, which is said
/// once under the group rather than implied on every row.
///
/// Machines this phone has found and not paired with sit in the same groups,
/// because where a machine is has the same answer whether or not any trust has
/// been written. Signing in adds the relay's machines to what can be found; it
/// is not what makes finding possible.
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
            VStack(alignment: .leading, spacing: 0) {
                header
                list
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            // Read over the machines rather than instead of them. The keys
            // being read are the keys to the machines on the list behind, and
            // a page that replaced them would ask somebody to remember which
            // machine they came to revoke.
            if model.readingDevices {
                DevicesSheet(model: model, actions: actions)
                    .transition(.move(edge: .bottom))
            }
        }
        .moving(value: model.readingDevices)
        // A screen is a container of the things on it, not a name for all of
        // them. Without this the system spreads this identifier over every
        // element underneath — the title, the buttons, the rows — so
        // everything on the screen answers to the screen's own name, for
        // VoiceOver and for anything driving the app alike.
        .accessibilityElement(children: .contain)
        .identified("hosts", value: subtitle)
    }

    // MARK: - Header

    private var header: some View {
        HStack(alignment: .center, spacing: 10) {
            VStack(alignment: .leading, spacing: 1) {
                Text("Hosts")
                    .designFont(.screenTitle, design)
                    .foregroundStyle(design.ink.color)
                    .identified("hosts.title", value: "Hosts")
                Text(subtitle)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkMuted.color)
                    .identified("hosts.subtitle", value: subtitle)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            // No add button. What this screen lists is machines, and a machine
            // is added by pairing with that machine: one found here carries
            // its own Pair button, and one that is not found prints a code to
            // scan. A plus that opened a keypad for no machine led nowhere.
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.vertical, 10)
    }

    /// "3 reachable · 1 away · 2 offline", and only the parts that are true.
    ///
    /// Reachable counts the two groups something can actually be started on.
    /// An away machine is counted apart rather than folded into either: it is
    /// not reachable, and calling it offline would claim it is not there.
    private var subtitle: String {
        let reachable = model.hosts(.onThisNetwork).count + model.hosts(.throughTheRelay).count
        let away = model.hosts(.away).count
        let lost = model.hosts(.offline).count
        var parts: [String] = []
        if reachable > 0 || (away == 0 && lost == 0) { parts.append("\(reachable) reachable") }
        if away > 0 { parts.append("\(away) away") }
        if lost > 0 { parts.append("\(lost) offline") }
        return parts.joined(separator: " · ")
    }

    // MARK: - The list

    /// The machines, in the four groups that decide what can be done with
    /// them, each holding both the machines this phone has paired with and the
    /// offers it has found on the same route.
    private var list: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                // First, because it is the reason the groups under it are
                // short: nothing found on a network nobody let this app look
                // at is not a fact about the network.
                if model.localNetwork == .denied { refusedNetwork }
                ForEach(HostGroup.all, id: \.name) { reach in
                    group(reach)
                }
                if model.hosts.isEmpty && model.discovered.isEmpty { empty }
                if let roster = model.roster { thisPhone(roster) }
            }
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 6)
            .padding(.bottom, 120)
        }
        .scrollIndicators(.hidden)
    }

    /// One group: what this phone is paired with on that route, then what it
    /// has found there and has not paired with.
    ///
    /// The offers are under the same head rather than in a section of their
    /// own, because where a machine is is the question the head answers and it
    /// has the same answer for both. What separates them is what each row can
    /// do — an offer carries Pair and nothing else — and that is said on the
    /// row, where it is true.
    @ViewBuilder
    private func group(_ reach: HostGroup) -> some View {
        let paired = model.hosts(reach)
        let offers = model.candidates(reach)
        if !paired.isEmpty || !offers.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                SectionHead(title: title(reach))
                if !paired.isEmpty {
                    RowGroup(items: paired) { host in row(host, reach) }
                }
                if !offers.isEmpty {
                    RowGroup(items: offers) { host in offer(host) }
                }
                if let caption = caption(reach, offers: !offers.isEmpty) {
                    Explain(caption)
                        .identified("hosts.caption.\(reach.name)", value: caption)
                }
            }
        }
    }

    private func title(_ reach: HostGroup) -> String {
        switch reach {
        case .onThisNetwork: "On this network"
        case .throughTheRelay: "Through the relay"
        case .away: "Away"
        case .offline: "Offline"
        }
    }

    /// The one thing worth saying under a group, and nothing where there is
    /// nothing.
    func caption(_ reach: HostGroup, offers: Bool) -> String? {
        switch reach {
        case .onThisNetwork:
            return offers ? "Run amux pair on one of these and enter the code it prints." : nil
        case .throughTheRelay:
            return nil
        // Stated, not sold. What is true is that the relay can see these and
        // this account cannot open a tunnel to one; the offer to pay for that
        // belongs where somebody is trying to use one, not on a list.
        case .away:
            return "The relay can see these. Reaching them from anywhere needs a subscription."
        // Two known facts that together point away from the machine: its
        // advertisement is on this network and no link to it stands. Said only
        // where both are true of something on the list.
        case .offline:
            return model.foundButUnreachable.isEmpty
                ? "Agents on an offline host report their state as unknown."
                : """
                  Agents on an offline host report their state as unknown. A host found \
                  here that will not answer may be on a network that blocks amux.
                  """
        }
    }

    /// A machine on this phone's route that it has not paired with.
    ///
    /// The same group as the paired machines, because it is in the same place;
    /// a different row, because nothing of its is readable and nothing can be
    /// started on it. What it is, is an offer, so it carries the one thing
    /// there is to do with one.
    private func offer(_ host: HostView) -> some View {
        HStack(spacing: 11) {
            Image(systemName: glyph(host))
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(design.inkFaint.color)
                .frame(width: 22)
            VStack(alignment: .leading, spacing: 2) {
                Text(host.name)
                    .designFont(.identifier, design)
                    .foregroundStyle(design.ink.color)
                Text(offered(host))
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
            }
            Spacer(minLength: 6)
            Button { if let id = host.id { actions(.pair(id)) } } label: {
                ActionLabel("Pair", kind: .outline)
            }
            .buttonStyle(.amuxRow)
            .accessibilityLabel("Pair with \(host.name)")
            .identified("hosts.pair.\(key(host))", label: "Pair with \(host.name)")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: 44)
        .accessibilityElement(children: .contain)
        .identified("hosts.offer.\(key(host))", label: spokenOffer(host), value: "not paired")
    }

    /// "Linux · found", and only the half that is known. "Found" rather than
    /// "not paired": the group has already said where it is, and what this row
    /// reports is that this phone saw it, which is the thing that makes it
    /// worth offering.
    private func offered(_ host: HostView) -> String {
        var parts: [String] = []
        if let platform = host.platform { parts.append(platform) }
        parts.append(host.group == .onThisNetwork ? "found" : "not paired")
        return parts.joined(separator: " · ")
    }

    private func spokenOffer(_ host: HostView) -> String {
        var parts = [host.name]
        if let platform = host.platform { parts.append(platform) }
        parts.append("found, not paired")
        return parts.joined(separator: ", ")
    }

    // MARK: - This phone

    /// What this phone is, and how many machines hold a key to it.
    ///
    /// Last on the screen and not among the machines, because it is not one of
    /// them: the rows above are places agents run, and this is the identity
    /// this device presents to all of them. The count is a way in rather than
    /// the answer — deciding to revoke means reading a fingerprint, and a
    /// fingerprint on every row would bury the machines.
    private func thisPhone(_ roster: Roster) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "This Phone")
            RowGroup(items: facts(roster)) { fact in
                if fact.opensDevices {
                    Button { model.readDevices() } label: { FactRow(fact: fact) }
                        .buttonStyle(.amuxRow)
                } else {
                    FactRow(fact: fact)
                }
            }
            Explain("Revoking a device ends its access immediately.")
                .identified("hosts.caption.devices")
        }
    }

    private func facts(_ roster: Roster) -> [Fact] {
        [
            Fact(
                label: "Identity", value: identity(roster.identity), mono: true,
                opensDevices: false),
            Fact(
                label: "Paired Devices", value: "\(roster.peers.count)", mono: false,
                opensDevices: true),
        ]
    }

    /// "iPhone · 4bb0…94e0": what this phone calls itself and enough of its
    /// key to recognise, on a row that has one line for both.
    ///
    /// Elided rather than truncated by the layout, so it elides the same way
    /// at every width and type size. The whole fingerprint is one tap away, in
    /// the same place the machines' are, because ends alone are not what
    /// anybody should compare a key by.
    private func identity(_ identity: Identity) -> String {
        "\(identity.name) · \(Fingerprint.short(identity.fingerprint))"
    }

    /// Why there is nothing on this network, when the reason is that nobody
    /// let this app look.
    ///
    /// Said here rather than left as an empty list, because the two are
    /// indistinguishable on screen and only one of them is fixable: iOS asks
    /// once and an app that was refused can never ask again, so the only way
    /// back is the system's own settings.
    private var refusedNetwork: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("amux cannot see this network")
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            Explain("""
                Nobody let amux look at the network this phone is on, so no host on it \
                can be found. Turn on Local Network for amux in Settings.
                """)
            Button { actions(.openSystemSettings) } label: {
                ActionLabel("Open Settings", kind: .outline)
            }
            .buttonStyle(.plain)
            .identified("hosts.localNetwork.settings", label: "Open Settings")
        }
        .accessibilityElement(children: .contain)
        .identified("hosts.localNetwork.refused", value: "amux cannot see this network")
    }

    /// Nothing paired and nothing found. It is not an error and it is not a
    /// failure of this phone: amux is free on the network it is on, and what
    /// has not happened yet is a host running there.
    ///
    /// Separate from the refused network above, which is on screen at the same
    /// time when both are true: one says there is nothing here, the other says
    /// why this app cannot tell.
    private var empty: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("No hosts yet")
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            Explain("Run amux on a computer on this network and it appears here.")
            PairingHint()
                .identified("hosts.empty.howToPair")
        }
        .identified("hosts.empty", value: "No hosts yet")
    }

    private func row(_ host: HostView, _ reach: HostGroup) -> some View {
        Button {
            if let id = host.id { actions(.open(id)) }
        } label: {
            HStack(spacing: 11) {
                Image(systemName: glyph(host))
                    .font(.system(size: 15, weight: .medium))
                    .foregroundStyle(
                        reach == .offline ? design.inkFaint.color : design.inkMuted.color)
                    .frame(width: 22)
                VStack(alignment: .leading, spacing: 2) {
                    Text(host.name)
                        .designFont(.identifier, design)
                        .foregroundStyle(design.ink.color)
                    Text(status(host, reach))
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkFaint.color)
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
        .accessibilityLabel(spoken(host, reach))
        .identified(
            "hosts.row.\(key(host))", label: spoken(host, reach), value: reach.name)
    }

    /// The machine drawn as the kind of machine it said it was. A host that
    /// has not said gets the plain computer rather than a guess, because the
    /// glyph is the first thing read and a wrong one is a wrong answer.
    /// What a driver names a host's row by.
    private func key(_ host: HostView) -> String {
        host.id?.description ?? host.name
    }

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

    /// The second line: what the machine is, and the one word for how this
    /// phone stands with it.
    ///
    /// The group above has already said where it is, so the word here is the
    /// route itself — "direct" for a link with nothing in between, "via relay"
    /// for one that crosses it — rather than a repeat of the heading. An
    /// offline machine says how long it has been gone where this phone
    /// watched it go, and says that it was found here when its advertisement
    /// is on this network and no link to it will stand.
    func status(_ host: HostView, _ reach: HostGroup) -> String {
        var parts: [String] = []
        if let platform = host.platform { parts.append(platform) }
        switch reach {
        case .onThisNetwork: parts.append("direct")
        case .throughTheRelay: parts.append("via relay")
        case .away: parts.append("away")
        case .offline:
            if host.reach == .away(.revoked) {
                parts.append("no longer trusts this phone")
            } else if let id = host.id, model.foundButUnreachable.contains(id) {
                parts.append("found, not answering")
            // Said because it settles which of two very different things is
            // wrong. A machine that never signed in cannot be reached from
            // anywhere but its own network whatever anybody buys, so the one
            // thing to do about it is on the machine — and offering a
            // subscription for it would be selling a fix that is not one.
            } else if host.reach == .away(.signedOut) {
                parts.append("offline, this phone is signed out")
            } else if let id = host.id, let gone = model.wentOffline(id) {
                parts.append("offline for \(since(gone))")
            } else {
                parts.append("offline")
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
    private func spoken(_ host: HostView, _ reach: HostGroup) -> String {
        var parts = [host.name]
        if let platform = host.platform { parts.append(platform) }
        switch reach {
        case .onThisNetwork: parts.append("reachable on this network")
        case .throughTheRelay: parts.append("reachable through the relay")
        case .away:
            parts.append("away")
            parts.append("seen by the relay and not reachable from here")
        case .offline:
            if host.reach == .away(.revoked) {
                parts.append("it no longer trusts this phone, pair again to reach it")
            } else if let id = host.id, model.foundButUnreachable.contains(id) {
                parts.append("found on this network and not answering")
            } else if host.reach == .away(.signedOut) {
                parts.append("offline while this phone is signed out")
            } else if let id = host.id, let gone = model.wentOffline(id) {
                parts.append("offline for \(since(gone))")
            } else {
                parts.append("offline")
            }
            parts.append("its agents’ state is unknown")
        }
        return parts.joined(separator: ", ")
    }
}

/// One stated fact about this phone: a label on the left and the answer on the
/// right, with a chevron only where there is somewhere to go.
private struct Fact: Identifiable, Equatable {
    var label: String
    var value: String
    /// Identities and fingerprints are compared character by character against
    /// something printed elsewhere, so they are set in the mono face the rest
    /// of the app spells identifiers in.
    var mono: Bool
    var opensDevices: Bool

    var id: String { label }
}

private struct FactRow: View {
    let fact: Fact

    var body: some View {
        FieldRow(
            label: fact.label,
            value: fact.value,
            mono: fact.mono,
            chevron: fact.opensDevices)
        .accessibilityElement(children: .combine)
        .identified(
            "hosts.fact.\(fact.label.lowercased().replacingOccurrences(of: " ", with: "-"))",
            label: "\(fact.label), \(fact.value)", value: fact.value)
    }
}

/// The machines this phone holds a key to, and the one thing there is to do
/// about one.
///
/// A fingerprint per row, because that is the whole reason to open this: the
/// count on the screen behind answers "how many", and the only question left
/// is which key belongs to what, which is answered by reading the fingerprint
/// against the one the machine itself prints.
private struct DevicesSheet: View {
    @Environment(\.design) private var design
    let model: HostsStore
    let actions: @MainActor (HostsAction) -> Void
    /// The machine whose revoke is being confirmed: one tap must not end a
    /// pairing that only pairing again can bring back.
    @State private var confirming: [UInt8]?

    var body: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 0)
            panel
        }
        .background(alignment: .top) {
            // A tap outside closes it. Nothing has been withdrawn by opening
            // the list, so leaving costs nothing and does not ask.
            Color.black.opacity(Glass.scrim)
                .ignoresSafeArea()
                .onTapGesture { model.stopReadingDevices() }
        }
    }

    private var panel: some View {
        VStack(alignment: .leading, spacing: 12) {
            Capsule()
                .fill(design.hairline.color)
                .frame(width: 40, height: 5)
                .frame(maxWidth: .infinity)
            HStack(alignment: .firstTextBaseline) {
                Text("This Phone")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                Spacer(minLength: 8)
                Button { model.stopReadingDevices() } label: {
                    ActionLabel("Done", kind: .plain)
                }
                .buttonStyle(.amuxControl)
                .identified("hosts.devices.done", label: "Done")
            }
            // Long enough to overflow on a phone paired with many machines,
            // and a whole fingerprint is what makes it long. It scrolls only
            // when it has to, so the ordinary case is a panel the size of what
            // is in it rather than one that always reaches for the screen.
            ViewThatFits(in: .vertical) {
                keys
                ScrollView { keys }.scrollIndicators(.hidden)
            }
            Explain("Revoking a device ends its access immediately.")
        }
        .padding(16)
        .frame(maxWidth: .infinity)
        .background {
            UnevenRoundedRectangle(
                topLeadingRadius: design.metrics.cardRadius,
                topTrailingRadius: design.metrics.cardRadius, style: .continuous)
                .fill(design.raised.color)
                .ignoresSafeArea(edges: .bottom)
        }
        .accessibilityElement(children: .contain)
        .identified("hosts.devices.sheet", value: "\(model.devices.count)")
    }

    /// This phone's own key and every machine's, whole.
    private var keys: some View {
        VStack(alignment: .leading, spacing: 12) {
            // This phone's own key, whole. It is the one a machine shows while
            // it waits to be told to trust this device, so the place somebody
            // comes to compare keys is the place it has to be readable.
            if let identity = model.roster?.identity {
                Surface(prominence: .subject) {
                    VStack(alignment: .leading, spacing: 3) {
                        Text(identity.name)
                            .designFont(.identifier, design)
                            .foregroundStyle(design.ink.color)
                        Text(Fingerprint.grouped(identity.fingerprint))
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.inkFaint.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .accessibilityElement(children: .combine)
                    .identified(
                        "hosts.devices.identity", label: identity.name,
                        value: identity.fingerprint)
                }
            }
            SectionHead(title: "Paired Devices")
            if model.devices.isEmpty {
                Explain("No host is paired with this phone.")
                    .identified("hosts.devices.none")
            } else {
                RowGroup(items: model.devices) { device in
                    row(device)
                }
            }
        }
    }

    private func row(_ device: PairedPeer) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            deviceLine(device)
            if confirming == device.id { confirm(device) }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: 44)
        .accessibilityElement(children: .contain)
        .identified(
            "hosts.device.\(device.host?.description ?? device.name)", label: "\(device.name), \(device.fingerprint)",
            value: device.fingerprint)
    }

    /// Revoking one machine, with what it ends said first.
    private func confirm(_ device: PairedPeer) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(String(localized: "Revoke \(device.name)? This phone stops reaching it, and only pairing again brings it back."))
                .designFont(.detail, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
            ButtonPair {
                choiceButton(String(localized: "Cancel"), kind: .outline, id: "hosts.revoke.cancel") {
                    confirming = nil
                }
                choiceButton(String(localized: "Revoke"), kind: .destructive, id: "hosts.revoke.confirm") {
                    confirming = nil
                    if let host = device.host { actions(.revoke(host)) }
                }
            }
        }
    }

    private func deviceLine(_ device: PairedPeer) -> some View {
        HStack(spacing: 11) {
            VStack(alignment: .leading, spacing: 2) {
                Text(device.name)
                    .designFont(.identifier, design)
                    .foregroundStyle(design.ink.color)
                Text(Fingerprint.grouped(device.fingerprint))
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 6)
            Button { confirming = device.id } label: {
                ActionLabel("Revoke", kind: .outline)
            }
            .buttonStyle(.amuxRow)
            .accessibilityLabel("Revoke \(device.name)")
            .identified("hosts.revoke.\(device.host?.description ?? device.name)", label: "Revoke \(device.name)")
        }
    }
}
