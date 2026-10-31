import AmuxValues
import Foundation
import Observation

/// Runs this phone's installation and keeps the stores of the account on
/// screen fed from its profile.
///
/// One installation per process, with one profile per account, as a
/// desktop has. The profile on screen is the only one whose fleet is open,
/// and the only one whose every listed agent keeps a source while the app
/// is in front of somebody; every bound profile keeps its relay link.
/// Switching account never stops the installation. Starting reads this phone's own store before any network is
/// dialled, so the first frame after a launch is what the phone last held.
///
/// While the runtime runs, the coordinator watches the phone's network and
/// tells the runtime of every change, so each link moves to the new network
/// rather than waiting for its old path to time out.
@MainActor
@Observable
public final class RuntimeCoordinator {
    /// How a test or a driving build changes where and how the runtime runs.
    public struct Options: Sendable {
        /// Advertisers with another scope are never listed; real machines
        /// advertise none.
        public var discoveryScope: String
        /// Where direct links listen instead of every interface, as
        /// `ip:port`. Only a driving build's library reads it.
        public var lanBind: String?
        /// A served test relay's plaintext carrier, as `ip:port`. Only a
        /// driving build's library reads it.
        public var relayTCP: String?
        /// A served test relay's QUIC carrier, as `ip:port`, trusted by
        /// `relayRoot`, its certificate as hex. Only a driving build's
        /// library reads them.
        public var relayQUIC: String?
        public var relayRoot: String?

        public init(
            discoveryScope: String = "", lanBind: String? = nil, relayTCP: String? = nil,
            relayQUIC: String? = nil, relayRoot: String? = nil
        ) {
            self.discoveryScope = discoveryScope
            self.lanBind = lanBind
            self.relayTCP = relayTCP
            self.relayQUIC = relayQUIC
            self.relayRoot = relayRoot
        }
    }

    /// Starts an installation, handing the closure it is given the profile
    /// list on the main actor whenever it moves.
    public typealias Starter = @Sendable (
        StartConfig, @escaping @MainActor @Sendable ([ProfileView]) -> Void
    ) throws(RuntimeFailure) -> Runtime

    /// The installation, once started.
    public private(set) var runtime: Runtime?
    /// The fleet of the profile on screen.
    public private(set) var profile: Profile?
    /// The installation's profiles as the runtime last handed them over.
    public private(set) var profiles: [ProfileView] = []
    /// Why the runtime is not running, when it failed to start.
    public private(set) var failure: String?
    /// A failure to open the store stops the whole app: nothing it holds is
    /// drawable, and a page drawn from nothing would lie.
    public private(set) var storeFailure: String? {
        didSet { if storeFailure != oldValue { storeFailureChanged?(storeFailure) } }
    }
    @ObservationIgnored public var storeFailureChanged: (@MainActor (String?) -> Void)?
    /// Told when a different account's stores go on screen, however that
    /// came about: a person choosing one, a sign-in, a removal, a push.
    @ObservationIgnored public var accountChanged: (@MainActor () -> Void)?
    public let deviceName: String

    /// The phone's own browser; only the system may look at the network.
    @ObservationIgnored public var discovery: LocalDiscovery? {
        didSet {
            discovery?.permissionChanged = { [weak self] permission in
                self?.permission = permission
                self?.stores.hosts.sawLocalNetwork(permission)
            }
        }
    }

    /// What a phone nobody has signed in on draws.
    @ObservationIgnored public var signedOutStores: StoreBundle

    @ObservationIgnored private let registry: AccountRegistry
    @ObservationIgnored private let support: URL
    @ObservationIgnored private let options: Options
    @ObservationIgnored private let starter: Starter
    @ObservationIgnored private let networkWatch: NetworkPathWatching
    #if DEBUG
    /// Every moment of the app's life fed to a runtime, in order, for the
    /// tests; nil outside test builds.
    @ObservationIgnored public private(set) var fedLifecycles: [Lifecycle]? = []
    #else
    public var fedLifecycles: [Lifecycle]? { nil }
    #endif
    /// Whether the published app sends product analytics, as the person
    /// set it; a running installation follows a change at once.
    public var shareUsage = true {
        didSet { if shareUsage != oldValue { runtime?.setTelemetry(shareUsage) } }
    }
    /// Whether the dumps amux's own checks take are sent to the account, as
    /// the person set it; off unless they turned it on.
    public var autoReports = false {
        didSet { if autoReports != oldValue { runtime?.setAutoReports(autoReports) } }
    }
    /// Whether somebody is in front of the app, as the app last said.
    @ObservationIgnored public private(set) var active = true
    @ObservationIgnored private var found: [Found] = []
    @ObservationIgnored private var permission: LocalNetworkPermission = .unknown
    @ObservationIgnored private var fed: StoreBundle?
    /// The stores on screen as last told to `accountChanged`.
    @ObservationIgnored private var shown: StoreBundle?
    /// Which opening of a profile the wakes belong to.
    @ObservationIgnored private var opening = 0
    /// The launch's store read is marked once, however many profiles open.
    @ObservationIgnored private var markedStoreRead = false
    @ObservationIgnored private var waiting: [CheckedContinuation<Runtime?, Never>] = []
    @ObservationIgnored private var starting: Task<Void, Never>?
    /// Which start a started runtime answers; one a stop overtook is stopped.
    @ObservationIgnored private var launch = 0

    public init(
        registry: AccountRegistry, support: URL, deviceName: String,
        signedOut: StoreBundle, options: Options = Options(),
        networkWatch: NetworkPathWatching = SystemNetworkWatch(),
        starter: @escaping Starter = { config, profilesMoved throws(RuntimeFailure) in
            try Runtime.start(config, profilesMoved: profilesMoved)
        }
    ) {
        self.registry = registry
        self.support = support
        self.deviceName = deviceName
        self.signedOutStores = signedOut
        self.options = options
        self.starter = starter
        self.networkWatch = networkWatch
        shown = registry.stores ?? signedOut
        registry.switching = { [weak self] _ in self?.switched() }
    }

    private func switched() {
        let now = stores
        if shown !== now {
            shown = now
            accountChanged?()
        }
        place()
    }

    /// The stores on screen.
    public var stores: StoreBundle { registry.stores ?? signedOutStores }

    /// Where the installation lives: every profile's key, trust and store.
    public var directory: URL {
        support.appendingPathComponent("installation", isDirectory: true)
    }

    /// Starts the installation, if it is not running or starting.
    public func start() {
        guard runtime == nil, starting == nil else { return }
        let directory = directory
        let config = StartConfig(
            dataDir: directory.path, deviceName: deviceName, autoReports: autoReports,
            discoveryScope: options.discoveryScope, lan: true, lanBind: options.lanBind,
            logPath: directory.appendingPathComponent("runtime.log").path,
            relayQuic: options.relayQUIC, relayRoot: options.relayRoot,
            relayTcp: options.relayTCP, tail: nil, telemetry: shareUsage)
        let starter = starter
        launch += 1
        let expected = launch
        starting = Task.detached {
            let started: Result<Runtime, RuntimeFailure>
            do {
                try FileManager.default.createDirectory(
                    at: directory, withIntermediateDirectories: true)
                Signposts.emit(.storeReadBegan)
                started = .success(try starter(config) { [weak self] listed in
                    self?.profilesMoved(listed, launch: expected)
                })
                Signposts.emit(.nodeStarted)
            } catch let failure as RuntimeFailure {
                started = .failure(failure)
            } catch {
                started = .failure(RuntimeFailure(error.localizedDescription))
            }
            await self.started(started, launch: expected)
        }
    }

    private func started(_ result: Result<Runtime, RuntimeFailure>, launch expected: Int) {
        guard expected == launch else {
            if case .success(let runtime) = result { runtime.stop() }
            return
        }
        starting = nil
        switch result {
        case .success(let runtime):
            self.runtime = runtime
            failure = nil
            storeFailure = nil
            // The setting may have moved while the runtime was starting.
            runtime.setTelemetry(shareUsage)
            runtime.setAutoReports(autoReports)
            if active { feed(runtime, .foreground) }
            networkWatch.start { [weak self] in
                Task { @MainActor in self?.networkChanged() }
            }
            if !found.isEmpty { runtime.discovered(found) }
            if !profiles.isEmpty {
                registry.show(profiles)
                place()
            }
            answer(runtime)
        case .failure(let reason):
            failure = reason.description
            storeFailure = reason.description
            answer(nil)
        }
    }

    /// Tells the runtime of a moment of the app's life, without waiting.
    private func feed(_ runtime: Runtime, _ input: Lifecycle) {
        remember(input)
        Task { await runtime.feed(input) }
    }

    private func remember(_ input: Lifecycle) {
        #if DEBUG
        fedLifecycles?.append(input)
        #endif
    }

    /// The phone's network changed: a running runtime moves its links.
    private func networkChanged() {
        guard let runtime else { return }
        feed(runtime, .networkChanged)
    }

    private func answer(_ runtime: Runtime?) {
        let answered = waiting
        waiting = []
        for waiter in answered { waiter.resume(returning: runtime) }
    }

    /// The installation once it has started, or nil when it failed.
    public func started() async -> Runtime? {
        if let runtime { return runtime }
        guard starting != nil else { return nil }
        return await withCheckedContinuation { waiting.append($0) }
    }

    /// Takes the profile list as this launch's runtime hands it over, and
    /// puts the one on screen in front. A list that comes before the runtime
    /// is in place waits for it there.
    private func profilesMoved(_ listed: [ProfileView], launch expected: Int) {
        guard expected == launch else { return }
        let added = Set(listed.map(\.id)).subtracting(profiles.map(\.id))
        profiles = listed
        guard let runtime else { return }
        // A profile made since the browser last reported has not been told
        // what is on the network.
        if !added.isEmpty, !found.isEmpty { runtime.discovered(found) }
        registry.show(listed)
        place()
    }

    /// Opens the fleet of the profile on screen when it is not the one open,
    /// then leaves every other profile on demand.
    private func place() {
        guard let runtime, let id = registry.profile else { return }
        let stores = stores
        if profile?.id != id || fed !== stores {
            close()
            opening += 1
            let expected = opening
            if !markedStoreRead { Signposts.emit(.fleetOpenBegan) }
            do {
                let opened = try runtime.open(id) { [weak self] moved in
                    guard self?.opening == expected else { return }
                    stores.moved(moved)
                }
                profile = opened
                fed = stores
                if !markedStoreRead {
                    markedStoreRead = true
                    Signposts.emit(.storeReadEnded)
                }
                stores.hosts.sawLocalNetwork(permission)
                stores.attach(opened)
            } catch {
                failure = error.description
            }
        }
        listSources()
    }

    /// Only the profile on screen lists every agent's source, and only in
    /// front of somebody. Out of the foreground the open profile's chat
    /// sessions drop their streams, except a chat that opens.
    private func listSources() {
        guard let runtime else { return }
        runtime.listSources(of: active ? registry.profile : nil)
        profile?.setForeground(active)
    }

    /// Makes sure an account whose profile trusts `host` is on screen, as a
    /// push in the background or a tap on one naming that host asks. The
    /// account on screen stays when its profile trusts the host; otherwise
    /// the first signed-in account whose profile does comes forward, else
    /// the first signed-out one. False when no account's profile trusts it.
    @discardableResult
    public func bringForward(_ host: HostId) async -> Bool {
        let trusting = await trusting(host)
        guard let onScreen = registry.profile, trusting.contains(onScreen) else {
            let accounts = registry.accounts.filter { trusting.contains($0.profile) }
            guard let account = accounts.first(where: \.signedIn) ?? accounts.first
            else { return false }
            registry.select(account.id)
            return profile?.id == account.profile
        }
        return profile?.id == onScreen
    }

    /// Every profile whose trust store holds `host`, oldest first.
    private func trusting(_ host: HostId) async -> [String] {
        guard let runtime = await started(),
              case .success(let trusting) = await runtime.trusting(host) else { return [] }
        return trusting
    }

    /// Closes the fleet on screen, and every chat on it first.
    private func close() {
        opening += 1
        fed?.closeChats()
        fed?.attach(nil)
        fed = nil
        profile?.close()
        profile = nil
    }

    /// Binds an account that signed in with its refresh token, and puts it
    /// on screen; its profile spends the token from then on and brings the
    /// relay link up. An account this phone knows binds its own profile; the
    /// first account takes over the profile nobody signed in on, keeping
    /// what it paired; any other gets a profile of its own.
    public func bind(
        _ account: AccountId, cloud: URL, client: String, refreshToken: String
    ) async -> Result<Nothing, RuntimeFailure> {
        guard let runtime = await started() else {
            return .failure(RuntimeFailure("this phone’s runtime did not start"))
        }
        let known = registry.accounts.first { $0.id == account }?.profile
        let target = known ?? (registry.selected == nil ? registry.unbound : nil)
        let bound = await runtime.bind(
            target, cloud: cloud, client: client, refreshToken: refreshToken)
        guard case .success(let view) = bound else {
            return bound.map { _ in Nothing() }
        }
        registry.select(AccountId(view.subject))
        await stores.refreshAccount()
        return .success(Nothing())
    }

    /// Signs an account out; its profile stays, tied to the account and
    /// still reaching the machines on this network.
    public func signOut(_ account: AccountId) async {
        guard let runtime = await started(),
              let entry = registry.accounts.first(where: { $0.id == account }) else { return }
        _ = await runtime.signOut(entry.profile)
        await stores.refreshAccount()
    }

    /// Takes an account off this phone: its profile — its key, the machines
    /// it paired, what it held — is deleted. The installation keeps one
    /// profile at least, so removing the last account leaves a fresh one
    /// nobody has signed in on.
    public func remove(_ account: AccountId) async {
        guard let runtime = await started(),
              let entry = registry.accounts.first(where: { $0.id == account }) else { return }
        if profiles.count == 1 {
            _ = await runtime.createProfile()
        }
        registry.leave(account)
        _ = await runtime.deleteProfile(entry.profile)
    }

    /// Stops the installation, closing the fleet and every chat on it first.
    public func stop() {
        close()
        launch += 1
        starting = nil
        answer(nil)
        networkWatch.stop()
        runtime?.stop()
        runtime = nil
        profiles = []
    }

    /// Starts again after the store failed to open.
    public func relaunch() {
        storeFailure = nil
        stop()
        start()
    }

    /// In front of somebody, every listed agent of the profile on screen
    /// keeps a source open and the browser runs. Put away, only the chats a
    /// push opens keep one, and the browser stops and forgets what it found.
    public func setActive(_ active: Bool) {
        guard self.active != active else { return }
        self.active = active
        listSources()
        if active {
            discovery?.start()
            if let runtime { feed(runtime, .foreground) }
            start()
        } else {
            discovery?.stop()
            if let runtime { feed(runtime, .background) }
        }
    }

    /// The app is about to be suspended: what analytics is waiting is sent,
    /// and every profile's store is made whole on the drive before this
    /// returns, should the system end the app while it is suspended.
    public func suspending() async {
        guard let runtime else { return }
        remember(.suspending)
        await runtime.feed(.suspending)
    }

    /// Records what only the app sees, on the account on screen.
    public func record(_ event: UsageEvent) {
        runtime?.record(event, profile: registry.profile)
    }

    /// Brings one agent's chat current for a push, under an account whose
    /// profile trusts the agent's host. In the background that account goes
    /// on screen first when the one there does not trust the host (see
    /// `bringForward`), and the profile is put under the on-demand policy, so the
    /// chat's own open is the only source that runs; coming to the
    /// foreground lists every agent again. In front of somebody the account
    /// on screen never changes under them: only its own hosts' chats are
    /// warmed, and a tap on the notification brings another forward. A host
    /// no profile trusts is left alone.
    public func warm(
        _ agent: AgentKey, inBackground: Bool, within limit: Duration = .seconds(25)
    ) async -> Warmed {
        if inBackground { setActive(false) }
        start()
        guard let host = agent.hostId else { return .unknownHost }
        if inBackground {
            guard await bringForward(host) else { return .unknownHost }
        } else {
            let trusting = await trusting(host)
            guard !trusting.isEmpty else { return .unknownHost }
            guard let onScreen = registry.profile, trusting.contains(onScreen) else {
                return .offScreen
            }
        }
        return await stores.warm(agent, within: limit) ? .current : .behind
    }

    /// What the browser found, handed over whole to every profile, and to
    /// any runtime started later.
    public func discovered(_ hosts: [Found]) {
        found = hosts
        runtime?.discovered(hosts)
    }

    /// A bearer for an account, borrowed from its profile, which alone holds
    /// the account's refresh token.
    public func bearer(for account: AccountId) async -> String? {
        guard let runtime,
              let entry = registry.accounts.first(where: { $0.id == account }),
              case .success(let bearer) = await runtime.accessToken(entry.profile)
        else { return nil }
        return bearer.bearer
    }

    /// Asks the account service what an account buys now, over its
    /// profile's relay link.
    @discardableResult
    public func refreshEntitlement(for account: AccountId) async -> Bool {
        guard let runtime,
              let entry = registry.accounts.first(where: { $0.id == account }),
              case .success = await runtime.refreshEntitlement(entry.profile)
        else { return false }
        return true
    }
}

/// How a push's chat was brought current.
public enum Warmed: Sendable, Equatable {
    case current
    /// It did not catch up in time.
    case behind
    /// No profile on this phone trusts the host the push named.
    case unknownHost
    /// Another account's host, pushed while somebody was using the app: the
    /// account on screen stays, and the notification's tap brings the other.
    case offScreen
}
