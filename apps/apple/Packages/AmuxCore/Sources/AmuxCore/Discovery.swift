import AmuxValues
import Foundation
import Network
import dnssd

/// Whether the system is letting this app look at the network it is on.
///
/// A person who said no is not a person with no machines nearby, and the two
/// have to be told apart: one is explained and can be undone in Settings, the
/// other is a network with nothing on it.
public enum LocalNetworkPermission: Equatable, Sendable {
    /// Nothing has been asked yet, or the browser has not answered.
    case unknown
    case granted
    case denied
}

/// The one place in this app that looks at the local network.
///
/// On iOS only the system may browse, so the shared Rust library does not: it
/// is handed what this saw and treats it as the only machines this device can
/// see. The whole set goes over each time — a machine that has gone is a
/// machine missing from the set, not an event of its own.
///
/// Browsing runs only while somebody is looking at the phone. A browser left
/// running behind a locked screen keeps the radio awake to learn about
/// machines nobody is about to talk to.
@MainActor
public final class LocalDiscovery {
    /// What amux advertises itself as. The transport is QUIC, hence `_udp`.
    public nonisolated static let service = "_amux._udp"

    public typealias HandOver = @MainActor ([Found]) -> Void

    /// What the system has said about looking at this network.
    public private(set) var permission: LocalNetworkPermission = .unknown {
        didSet {
            guard permission != oldValue else { return }
            permissionChanged?(permission)
        }
    }

    /// Told when the answer changes, so a screen can explain a refusal.
    public var permissionChanged: (@MainActor (LocalNetworkPermission) -> Void)?

    /// Starts resolving one route to an address and reports the address, or
    /// nothing where the route gave none in time. Returns what abandons it.
    typealias Resolve = @MainActor (
        _ route: NWEndpoint, _ within: TimeInterval, _ settled: @escaping @MainActor (String?) -> Void
    ) -> @MainActor () -> Void
    /// Runs something on the main actor after a delay.
    typealias After = @MainActor (_ delay: TimeInterval, _ work: @escaping @MainActor () -> Void) -> Void

    /// How long one route may take to give an address. A healthy lookup on a
    /// local network settles in well under a second; one that has not by now
    /// is waiting on a record nobody will answer for.
    nonisolated static let resolveTimeout: TimeInterval = 3
    /// How long to wait before trying every route of an advertisement again
    /// once all of them have failed.
    nonisolated static let retryDelay: TimeInterval = 5

    private let handOver: HandOver
    /// The only machines this browser may report, or nothing where it reports
    /// every amux machine it resolves. Set by a driven debug launch, whose
    /// simulator browses the Mac's real network and would otherwise report
    /// whatever else is running on it.
    private let only: Set<HostId>?
    private let resolve: Resolve
    private let after: After
    private var browser: NWBrowser?
    /// Which browser's reports are taken: bumped as one starts and as one
    /// stops, so a report still on its way to the main actor from a
    /// browser that was stopped, or replaced, is ignored. Each report
    /// carries the value it was made under.
    private(set) var browse = 0
    /// Every advertisement being resolved or already resolved, by the endpoint
    /// the browser reports it under.
    private var tracked: [NWEndpoint: Tracked] = [:]
    /// The advertisements whose addresses are known, which is what is handed over.
    private var found: [NWEndpoint: Found] = [:]
    /// Bumped whenever an advertisement starts resolving afresh, so an answer
    /// or a retry belonging to an attempt that was replaced is ignored.
    private var attempts = 0

    public convenience init(only: Set<HostId>? = nil, handOver: @escaping HandOver) {
        self.init(only: only, resolve: Self.connecting, after: Self.mainQueue, handOver: handOver)
    }

    init(only: Set<HostId>?, resolve: @escaping Resolve, after: @escaping After,
         handOver: @escaping HandOver) {
        self.only = only
        self.resolve = resolve
        self.after = after
        self.handOver = handOver
    }

    /// The queue the browser starts and reports on. Starting a browser on
    /// the main thread was measured at about 75 ms of a cold launch's main
    /// thread; here it costs the launch nothing, and what it reports is
    /// read on the main actor.
    nonisolated private static let browsing = DispatchQueue(label: "sh.amux.discovery", qos: .utility)

    /// Begins browsing. Doing this twice is doing it once.
    public func start() {
        guard browser == nil else { return }
        browse += 1
        let browse = browse
        let parameters = NWParameters()
        parameters.includePeerToPeer = false
        let browser = NWBrowser(
            for: .bonjourWithTXTRecord(type: Self.service, domain: nil), using: parameters)
        browser.stateUpdateHandler = { state in
            Task { @MainActor in self.browserSaid(state, browse: browse) }
        }
        browser.browseResultsChangedHandler = { results, _ in
            let sightings = results.map { result in
                Sighting(
                    endpoint: result.endpoint,
                    record: Self.record(of: result.metadata),
                    routes: Self.routes(to: result.endpoint, on: result.interfaces))
            }
            Task { @MainActor in self.saw(sightings, browse: browse) }
        }
        self.browser = browser
        Self.browsing.async { browser.start(queue: Self.browsing) }
    }

    /// Stops browsing, forgetting what was seen without withdrawing it.
    ///
    /// Nothing is handed over here. The library is told separately that the
    /// app went away and closes its direct links itself; the machines it was
    /// told about are the ones it dials again the moment the app comes back,
    /// rather than an empty network until the browser has found them twice.
    /// A machine that really did leave meanwhile is missing from the first set
    /// the restarted browser hands over.
    public func stop() {
        guard browser != nil else { return }
        browser?.cancel()
        browser = nil
        browse += 1
        forget()
        // A refusal outlives the browser: the person has to change it in
        // Settings, and the card that says so must not vanish in the
        // meantime. Anything else is asked again by the next browser.
        if permission != .denied { permission = .unknown }
    }

    /// Takes what the browser says about this network. Once refused, only
    /// a browser that is ready takes the refusal back: the waits and
    /// cancellations that follow a refusal say nothing new about it.
    func browserSaid(_ state: NWBrowser.State) {
        browserSaid(state, browse: browse)
    }

    /// What a browser said of its state, taken only from the current one.
    func browserSaid(_ state: NWBrowser.State, browse: Int) {
        guard browse == self.browse else { return }
        let said = Self.permission(for: state)
        guard said != .unknown || permission != .denied else { return }
        permission = said
    }

    private func forget() {
        for entry in tracked.values { entry.abandon?() }
        tracked = [:]
        found = [:]
        attempts += 1
    }

    /// One advertisement as the browser reported it.
    struct Sighting {
        let endpoint: NWEndpoint
        let record: NWTXTRecord?
        /// The same advertisement scoped to each interface it was seen on, in
        /// the order they are tried.
        let routes: [NWEndpoint]
    }

    private struct Claim: Equatable {
        let host: HostId
        let name: String
        let version: UInt32
        let scope: String
    }

    private struct Tracked {
        let claim: Claim
        let routes: [NWEndpoint]
        let attempt: Int
        var next = 0
        var abandon: (@MainActor () -> Void)?
    }

    /// The set the browser can currently see.
    ///
    /// An advertisement is resolved when it first appears and again whenever
    /// what it claims or where it was seen changes, because a machine that
    /// came back under the same name may be listening somewhere else.
    func saw(_ sightings: [Sighting]) {
        saw(sightings, browse: browse)
    }

    /// What a browser said it saw, taken only from the current one.
    func saw(_ sightings: [Sighting], browse: Int) {
        guard browse == self.browse else { return }
        let visible = Set(sightings.map(\.endpoint))
        for endpoint in Array(tracked.keys) where !visible.contains(endpoint) {
            tracked.removeValue(forKey: endpoint)?.abandon?()
            found.removeValue(forKey: endpoint)
        }
        for sighting in sightings {
            guard let record = sighting.record,
                  let claimed = Self.claim(from: record),
                  only?.contains(claimed.host) ?? true,
                  let name = claimed.name ?? Self.serviceName(of: sighting.endpoint)
            else { continue }
            let claim = Claim(
                host: claimed.host, name: name, version: claimed.version, scope: claimed.scope)
            if let current = tracked[sighting.endpoint],
               current.claim == claim, current.routes == sighting.routes {
                continue
            }
            tracked[sighting.endpoint]?.abandon?()
            if found[sighting.endpoint]?.hostId != claim.host.bytes {
                found.removeValue(forKey: sighting.endpoint)
            }
            attempts += 1
            tracked[sighting.endpoint] = Tracked(
                claim: claim, routes: sighting.routes, attempt: attempts)
            tryNextRoute(of: sighting.endpoint)
        }
        handOver(Array(found.values))
    }

    /// Resolves the next route of an advertisement, and once every route has
    /// failed, starts over from the first after a pause.
    ///
    /// A lookup that never settles is not an error the system reports: the
    /// connection waits for a record indefinitely. Without a deadline one
    /// unanswerable route would hide the machine for as long as the browser
    /// runs.
    private func tryNextRoute(of endpoint: NWEndpoint) {
        guard var entry = tracked[endpoint] else { return }
        let attempt = entry.attempt
        guard entry.next < entry.routes.count else {
            entry.next = 0
            entry.abandon = nil
            tracked[endpoint] = entry
            after(Self.retryDelay) { [weak self] in
                guard let self, self.tracked[endpoint]?.attempt == attempt else { return }
                self.tryNextRoute(of: endpoint)
            }
            return
        }
        let route = entry.routes[entry.next]
        entry.next += 1
        tracked[endpoint] = entry
        let abandon = resolve(route, Self.resolveTimeout) { [weak self] address in
            guard let self, let entry = self.tracked[endpoint], entry.attempt == attempt else { return }
            guard let address else {
                self.tryNextRoute(of: endpoint)
                return
            }
            self.tracked[endpoint]?.abandon = nil
            self.found[endpoint] = Found(
                hostId: entry.claim.host.bytes, name: entry.claim.name,
                version: entry.claim.version, addrs: [address], scope: entry.claim.scope)
            self.handOver(Array(self.found.values))
        }
        if tracked[endpoint]?.attempt == attempt, tracked[endpoint]?.next == entry.next {
            tracked[endpoint]?.abandon = abandon
        }
    }

    /// The routes to try for an advertisement seen on these interfaces:
    /// the same advertisement scoped to each one, loopback last.
    ///
    /// Only a simulator sees a Mac's loopback interface. There the system
    /// looks the advertised host name up in the Mac's own responder alone,
    /// which answers only for what was registered with it, so an advertisement
    /// published by a daemon's own mDNS library resolves over the network and
    /// never over loopback.
    nonisolated static func routes(to endpoint: NWEndpoint, on interfaces: [NWInterface]) -> [NWEndpoint] {
        guard case .service(let name, let type, let domain, _) = endpoint, !interfaces.isEmpty else {
            return [endpoint]
        }
        let ordered = interfaces.filter { $0.type != .loopback } + interfaces.filter { $0.type == .loopback }
        return ordered.map { .service(name: name, type: type, domain: domain, interface: $0) }
    }

    /// Turns a route into an address by opening the connection the system
    /// resolves it for, reading the address it settled on and dropping it
    /// again. Bonjour gives a name and a port; only a connection gives the
    /// address behind them, and nothing here wants the connection itself.
    private static func connecting(
        to route: NWEndpoint, within timeout: TimeInterval,
        settled: @escaping @MainActor (String?) -> Void
    ) -> @MainActor () -> Void {
        let connection = NWConnection(to: route, using: .udp)
        let once = Once()
        let finish: @MainActor (String?) -> Void = { address in
            guard once.claim() else { return }
            connection.cancel()
            settled(address)
        }
        connection.stateUpdateHandler = { state in
            MainActor.assumeIsolated {
                switch state {
                case .ready:
                    finish(connection.currentPath?.remoteEndpoint.flatMap(Self.address(of:)))
                case .failed:
                    finish(nil)
                default: break
                }
            }
        }
        connection.start(queue: .main)
        DispatchQueue.main.asyncAfter(deadline: .now() + timeout) {
            MainActor.assumeIsolated { finish(nil) }
        }
        return {
            guard once.claim() else { return }
            connection.cancel()
        }
    }

    private static func mainQueue(_ delay: TimeInterval, _ work: @escaping @MainActor () -> Void) {
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) {
            MainActor.assumeIsolated { work() }
        }
    }

    /// Settles a lookup exactly once, whichever of its answer, its deadline or
    /// its abandonment comes first.
    @MainActor
    private final class Once {
        private var settled = false

        func claim() -> Bool {
            guard !settled else { return false }
            settled = true
            return true
        }
    }

    /// The TXT record a browse result carries, where it carries one.
    nonisolated static func record(of metadata: NWBrowser.Result.Metadata) -> NWTXTRecord? {
        guard case .bonjour(let record) = metadata else { return nil }
        return record
    }

    /// What an advertisement's own record claims it is.
    ///
    /// A record missing either field is not an amux host advertising itself —
    /// some other service on the same name, or a version that predates them —
    /// and is passed over rather than guessed at.
    ///
    /// The machine's name is the record's own, where it carries one: a
    /// responder that meets a second "desk" on the network renames this
    /// instance "desk (2)", which is not what the machine is called.
    nonisolated static func claim(
        from record: NWTXTRecord
    ) -> (host: HostId, version: UInt32, scope: String, name: String?)? {
        guard let hid = record["hid"], let host = HostId(hid),
              let claimed = record["v"], let version = UInt32(claimed)
        else { return nil }
        let name = record["name"].flatMap { $0.isEmpty ? nil : $0 }
        return (host, version, record["scope"] ?? "", name)
    }

    /// The instance name a Bonjour endpoint carries: the machine's own,
    /// unless a name conflict on the network renamed it.
    nonisolated static func serviceName(of endpoint: NWEndpoint) -> String? {
        guard case .service(let name, _, _, _) = endpoint else { return nil }
        return name
    }

    /// A resolved address as the shared library spells one, or nothing where
    /// it could not dial what was resolved.
    ///
    /// A link-local IPv6 address is dropped: it is only meaningful with the
    /// interface it was learned on, and the address the library parses has
    /// nowhere to carry one. A machine reachable at all is also reachable at
    /// an address that travels.
    nonisolated static func address(of endpoint: NWEndpoint) -> String? {
        guard case .hostPort(let host, let port) = endpoint else { return nil }
        switch host {
        case .ipv4(let address):
            return "\(unscoped(address)):\(port.rawValue)"
        case .ipv6(let address):
            guard !address.isLinkLocal else { return nil }
            return "[\(unscoped(address))]:\(port.rawValue)"
        case .name:
            return nil
        @unknown default:
            return nil
        }
    }

    /// An address without the `%en0` the system appends when it knows which
    /// interface the address was learned on.
    private nonisolated static func unscoped(_ address: any IPAddress) -> String {
        String("\(address)".prefix { $0 != "%" })
    }

    /// What a browser's state says about being allowed to look at this network.
    ///
    /// A refusal is not an error the browser reports once and recovers from:
    /// it waits, holding the one DNS code that means the person said no. Every
    /// other wait is an ordinary network that is not ready yet.
    nonisolated static func permission(for state: NWBrowser.State) -> LocalNetworkPermission {
        switch state {
        case .ready: return .granted
        case .waiting(.dns(DNSServiceErrorType(kDNSServiceErr_PolicyDenied))): return .denied
        case .failed(.dns(DNSServiceErrorType(kDNSServiceErr_PolicyDenied))): return .denied
        default: return .unknown
        }
    }
}
