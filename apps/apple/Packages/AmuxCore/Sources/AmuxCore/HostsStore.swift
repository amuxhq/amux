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
    /// The account this profile is bound to, as last read.
    public private(set) var account: AccountView?

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

extension PairedPeer: Identifiable {
    public var id: [UInt8] { hostId }
    /// The machine, as the bridge names it.
    public var host: HostId? { HostId(bytes: hostId) }
}
