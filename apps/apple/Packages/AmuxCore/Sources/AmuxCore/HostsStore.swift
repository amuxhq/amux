import AmuxValues
import Foundation
import Observation

/// The machines this phone knows: the ones it trusts, the ones it has found
/// on this network and not paired, and this phone itself.
@MainActor
@Observable
public final class HostsStore {
    /// Trusted machines, online first, then by name.
    public private(set) var hosts: [HostView] = []
    /// Found on this network and never paired.
    public private(set) var discovered: [HostView] = []
    /// This phone, as the runtime lists it.
    public private(set) var local: HostView?
    /// This phone's identity and the machines it trusts, by fingerprint.
    public private(set) var roster: Roster?
    public private(set) var localNetwork: LocalNetworkPermission = .unknown
    /// Whether the person has been told why amux looks at this network.
    /// iOS asks for local network access the moment the phone first looks,
    /// so until this is said the phone does not look.
    public private(set) var networkExplained = true
    /// The account this profile is bound to, as the profile list carries it.
    public private(set) var account: AccountView?
    /// The machines whose pairing window the person already put away: the
    /// phone does not raise the same window again while it stays open.
    public private(set) var declinedWindows: Set<HostId> = []

    /// The machines last read away, kept until each is reached or no longer
    /// listed: putting the app away drops every link, and coming back reads
    /// a machine unreached until the first dials end, which is no news about
    /// the plan.
    public private(set) var heldAway: Set<HostId> = []

    @ObservationIgnored private var departures: [HostId: Date] = [:]
    @ObservationIgnored private let clock: @MainActor () -> Date

    public init(clock: @escaping @MainActor () -> Date = { Date() }) {
        self.clock = clock
    }

    public var now: Date { clock() }

    /// What the runtime lists now.
    public func show(_ views: [HostView]) {
        local = views.first(where: \.local)
        let trusted = views.filter { $0.trusted && !$0.local }
        let known = Dictionary(
            hosts.compactMap { host in host.id.map { ($0, host.online) } },
            uniquingKeysWith: { _, last in last })
        for host in trusted {
            guard let id = host.id else { continue }
            if host.online {
                departures[id] = nil
            } else if known[id] == true {
                // Only a departure this phone watched has a time.
                departures[id] = clock()
            }
        }
        let listed = Set(trusted.compactMap(\.id))
        departures = departures.filter { listed.contains($0.key) }
        hosts = trusted.sorted {
            ($0.online ? 0 : 1, $0.name.lowercased()) < ($1.online ? 0 : 1, $1.name.lowercased())
        }
        discovered = views.filter { $0.candidate && !$0.local }
            .sorted { $0.name.lowercased() < $1.name.lowercased() }
        let unreached = Set(trusted.filter { !$0.online }.compactMap(\.id))
        heldAway = heldAway.intersection(unreached)
            .union(trusted.filter { $0.group == .away }.compactMap(\.id))
        // A window that closed is forgotten, so the next one the machine
        // opens is offered again.
        let open = Set(discovered.filter(\.windowOpen).compactMap(\.id))
        declinedWindows.formIntersection(open)
    }

    /// The first machine, by name, the relay would reach if this account
    /// paid for it, as last read.
    public var awayHost: HostView? {
        hosts.filter { host in host.id.map(heldAway.contains) ?? false }
            .min { $0.name.lowercased() < $1.name.lowercased() }
    }

    /// The machine found nearby with a pairing window open that the phone
    /// offers to pair with by itself, if there is one the person has not
    /// put away.
    public var windowToOffer: HostView? {
        discovered.first { host in
            guard host.windowOpen, let id = host.id else { return false }
            return !declinedWindows.contains(id)
        }
    }

    /// The person put away the pairing a machine's open window raised, or
    /// one they opened for it: the phone does not raise it again until that
    /// window closes and another opens.
    public func declineWindow(_ id: HostId) {
        guard discovered.contains(where: { $0.id == id && $0.windowOpen }) else { return }
        declinedWindows.insert(id)
    }

    public func explainedNetwork(_ explained: Bool) {
        networkExplained = explained
    }

    public func show(_ roster: Roster) {
        self.roster = roster
    }

    public func show(_ account: AccountView?) {
        self.account = account
    }

    public var online: [HostView] { hosts.filter(\.online) }
    public var offline: [HostView] { hosts.filter { !$0.online } }

    public func hosts(_ group: HostGroup) -> [HostView] {
        hosts.filter { $0.group == group }
    }

    public func candidates(_ group: HostGroup) -> [HostView] {
        discovered.filter { $0.group == group }
    }

    /// Trusted machines discovery can see that no route reaches.
    public var foundButUnreachable: Set<HostId> {
        let seen = Set(hosts.filter { !$0.addrs.isEmpty }.compactMap(\.id))
        return Set(hosts.filter { $0.group == .offline && seen.contains($0.id!) }.compactMap(\.id))
    }

    public func wentOffline(_ id: HostId) -> Date? { departures[id] }

    public var devices: [PairedPeer] { roster?.peers ?? [] }

    /// Whether the roster is older than the machines: unread, or missing a
    /// machine this phone has since come to trust, whose page would otherwise
    /// have no key to show.
    public var rosterBehind: Bool {
        guard let roster else { return true }
        let known = Set(roster.peers.map(\.hostId))
        return hosts.contains { $0.trusted && !known.contains($0.hostId) }
    }

    public func sawLocalNetwork(_ permission: LocalNetworkPermission) {
        localNetwork = permission
    }

    public func host(_ id: HostId) -> HostView? { hosts.first { $0.id == id } }

    /// A trusted host, or one found on this network.
    public func known(_ id: HostId) -> HostView? {
        hosts.first { $0.id == id } ?? discovered.first { $0.id == id }
    }
}

extension HostView {
    /// Found on this network with a pairing window open: a code typed now
    /// can work. Only a machine's own advertisement says so, so one reached
    /// through the relay or a paired machine never reads open.
    public var windowOpen: Bool { candidate && pairingOpen && !addrs.isEmpty }

    /// Found on this network with no pairing window open: a code typed now
    /// can only fail, so the person is told to open amux there instead.
    public var windowClosed: Bool { candidate && !pairingOpen && !addrs.isEmpty }
}

extension PairedPeer: Identifiable {
    public var id: [UInt8] { hostId }
    /// The machine, as the bridge names it.
    public var host: HostId? { HostId(bytes: hostId) }
}
