import AmuxValues
import Foundation

/// Where the hosts screen lists a host: by the route it is reached over,
/// else whether the relay sees it.
public enum HostGroup: Sendable, Equatable {
    case onThisNetwork
    case throughTheRelay
    /// Seen by the relay, and not reachable from here.
    case away
    case offline

    public var name: String {
        switch self {
        case .onThisNetwork: "on-this-network"
        case .throughTheRelay: "through-the-relay"
        case .away: "away"
        case .offline: "offline"
        }
    }

    public static let all: [HostGroup] = [.onThisNetwork, .throughTheRelay, .away, .offline]
}

extension Reach {
    /// A link stands to the host now.
    public var online: Bool {
        if case .online = self { return true }
        return false
    }
}

extension Platform {
    /// The system's name for itself, as a person reads it; nil when the
    /// host never said.
    public var name: String? {
        switch self {
        case .macos: "macOS"
        case .linux: "Linux"
        case .windows: "Windows"
        case .ios: "iOS"
        case .unspecified: nil
        }
    }
}

extension HostView: Identifiable {
    public var id: HostId? { HostId(bytes: hostId) }

    public var online: Bool { reach.online }

    /// Where the hosts screen lists this host, from how the runtime says
    /// this phone reaches it.
    ///
    /// A candidate is found on this network by definition. A trusted host
    /// online is grouped by the route its link runs over. Away is kept for
    /// a host the relay refused to connect for the account's free plan; one
    /// that no longer trusts this phone, or that only a signed-out phone
    /// cannot reach, is offline here.
    public var group: HostGroup {
        if candidate { return .onThisNetwork }
        switch reach {
        case .online(let via):
            return via == .direct ? .onThisNetwork : .throughTheRelay
        case .away:
            return .away
        case .offline:
            return .offline
        }
    }
}
