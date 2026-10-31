import AmuxApp
import AmuxValues
import Foundation

/// Why the library did not do what it was asked, in its own words.
public struct RuntimeFailure: Error, Sendable, Equatable, CustomStringConvertible {
    public let description: String

    public init(_ description: String) {
        self.description = description
    }
}

/// A moment of the app's life the runtime is told of.
public enum Lifecycle: String, Sendable {
    /// Somebody brought the app to the front; counted at most once an hour.
    case foreground = "Foreground"
    /// The app is leaving the screen: the analytics waiting are sent.
    case background = "Background"
    /// The app is about to be suspended: the analytics waiting are sent,
    /// and every profile's store is made whole on the drive, should the
    /// system end the app while it is suspended.
    case suspending = "Suspending"
    /// The phone's network changed: every profile's link socket is bound
    /// afresh, so its links move to the new network with their
    /// connections, and every path to every host is tried again.
    case networkChanged = "NetworkChanged"
}

/// The shared Rust runtime: this phone's installation, hosted in the app's
/// own process, with one profile per account exactly as a desktop has.
///
/// Every value crosses as JSON whose Swift mirror is generated from the Rust
/// definition, so nothing here interprets a row, a card or a host; it only
/// hands them over.
///
/// The app owns the loop: the library's cores step on one serial queue,
/// `CoreLoop`'s, and nothing in the library ever calls into Swift. Reads
/// return at once, on any thread. The calls that block until the cores have
/// done something (starting, stopping, opening a profile or a chat, a
/// moment of the app's life) step the cores on the thread
/// that makes them, so they are made on that queue, never on the main
/// thread. Acts are `async`: each starts on the queue and is answered by a
/// later step.
///
/// The profile list is handed to the main actor whenever it moved, before
/// the answer of the act that moved it. A profile's fleet and chats are a
/// `Profile` opened from here.
public final class Runtime: @unchecked Sendable {
    private let handle: OpaquePointer
    private let loop: CoreLoop
    /// Held across every read made off the queue, so a stop waits for the
    /// reads in flight and none starts after it.
    private let lock = NSLock()
    private var running = true

    private init(handle: OpaquePointer, loop: CoreLoop) {
        self.handle = handle
        self.loop = loop
    }

    /// Starts the runtime; blocks, on the core queue, until every profile's
    /// store is open. `profilesMoved` is handed the profile list on the main
    /// actor whenever it moved, the first time once the registry is read.
    public static func start(
        _ config: StartConfig, profilesMoved: @escaping @MainActor @Sendable ([ProfileView]) -> Void
    ) throws(RuntimeFailure) -> Runtime {
        let loop = CoreLoop(profilesMoved: profilesMoved)
        let config = Bridge.json(config)
        let started = loop.blocking { () -> Result<Live, RuntimeFailure> in
            var error: UnsafeMutablePointer<CChar>?
            guard let started = config.withCString({ amux_runtime_start($0, &error) }) else {
                return .failure(Bridge.failure(error, otherwise: "the runtime did not start"))
            }
            loop.run(started)
            return .success(Live(started))
        }
        return Runtime(handle: try started.get().pointer, loop: loop)
    }

    /// Stops the runtime and flushes every profile's store, closing every
    /// profile opened from it first. Close their chats before that; any read
    /// after this answers nothing, and any act answers that it stopped.
    public func stop() {
        let wasRunning = lock.withLock {
            defer { running = false }
            return running
        }
        guard wasRunning else { return }
        // Only the loop is captured: this runs from `deinit` too.
        let loop = loop
        loop.blocking { loop.stop() }
    }

    /// Runs a read on the live runtime, or answers `stopped` once it is not.
    private func call<T>(_ stopped: T, _ body: (OpaquePointer) -> T) -> T {
        lock.withLock { running ? body(handle) : stopped }
    }

    deinit { stop() }

    // MARK: - The profile registry

    /// A new profile nobody has signed in on.
    public func createProfile() async -> Result<ProfileView, RuntimeFailure> {
        await act(ProfileView.self) { amux_runtime_create_profile($0) }
    }

    /// Deletes a profile: its key, the machines it trusts and its store.
    /// The installation keeps at least one. Close it first.
    public func deleteProfile(_ profile: String) async -> Result<Nothing, RuntimeFailure> {
        await act(Nothing.self) { live in
            profile.withCString { amux_runtime_delete_profile(live, $0) }
        }
    }

    /// Binds a profile to an account with the refresh token a sign-in
    /// obtained as `client`; the profile spends the token from then on. A
    /// named profile is adopted even when it paired machines before; nil
    /// lets the registry pick the account's own profile, else one that
    /// holds nothing, else a new one.
    public func bind(
        _ profile: String?, cloud: URL, client: String, refreshToken: String
    ) async -> Result<ProfileView, RuntimeFailure> {
        let cloud = cloud.absoluteString
        return await act(ProfileView.self) { live in
            Bridge.optional(profile) { profile in
                cloud.withCString { cloud in
                    client.withCString { client in
                        refreshToken.withCString { token in
                            amux_runtime_bind(live, profile, cloud, client, token)
                        }
                    }
                }
            }
        }
    }

    /// Signs a profile out; it stays tied to its account.
    public func signOut(_ profile: String) async -> Result<ProfileView, RuntimeFailure> {
        await act(ProfileView.self) { live in
            profile.withCString { amux_runtime_sign_out(live, $0) }
        }
    }

    /// Every profile whose trust store holds a machine, oldest first.
    public func trusting(_ host: HostId) async -> Result<[String], RuntimeFailure> {
        let host = Bridge.json(host.bytes)
        return await act([String].self) { live in
            host.withCString { amux_runtime_trusting_profiles(live, $0) }
        }
    }

    /// Opens pairing mode on a profile and answers the link its QR code
    /// would carry. Only a driving build offers one.
    public func offerPairing(_ profile: String) async -> Result<String, RuntimeFailure> {
        await act(String.self) { live in
            profile.withCString { amux_runtime_offer_pairing(live, $0) }
        }
    }

    /// Every listed agent of the profile in front of somebody keeps a source
    /// open; every other profile, and every profile when nil (the app in the
    /// background), keeps one only for the chats that open.
    public func listSources(of profile: String?) {
        call(()) { live in Bridge.optional(profile) { amux_runtime_list_sources(live, $0) } }
    }

    /// Whether every listed agent of a profile keeps a source open now.
    public func listsSources(_ profile: String) -> Bool {
        call(false) { live in
            profile.withCString { amux_runtime_source_policy_listed(live, $0) }
        }
    }

    /// Hands over the whole set this phone's browser found on the network,
    /// to every profile.
    public func discovered(_ found: [Found]) {
        call(()) { live in Bridge.json(found).withCString { amux_runtime_discovered(live, $0) } }
    }

    /// A bearer for a profile's account service. The profile alone holds
    /// the refresh token, so this is the only way this app gets a fresh one.
    public func accessToken(_ profile: String) async -> Result<Bearer, RuntimeFailure> {
        await act(Bearer.self) { live in
            profile.withCString { amux_runtime_access_token(live, $0) }
        }
    }

    /// Asks the account service what a profile's account buys now, over its
    /// relay link, so the relay's tier follows a purchase at once.
    public func refreshEntitlement(_ profile: String) async -> Result<Nothing, RuntimeFailure> {
        await act(Nothing.self) { live in
            profile.withCString { amux_runtime_refresh_entitlement(live, $0) }
        }
    }

    // MARK: - The app's life

    /// Tells the runtime of a moment of the app's life, on the core queue,
    /// and waits until it is carried out: a suspending app's stores are
    /// whole on the drive when this returns.
    @discardableResult
    public func feed(_ input: Lifecycle) async -> Result<Nothing, RuntimeFailure> {
        let text = Bridge.json(input.rawValue)
        return await loop.later { live -> Result<Nothing, RuntimeFailure> in
            var error: UnsafeMutablePointer<CChar>?
            let fed = text.withCString { amux_runtime_feed(live, $0, &error) }
            guard fed else { return .failure(Bridge.failure(error, otherwise: Bridge.stoppedReason)) }
            return .success(Nothing())
        } ?? .failure(RuntimeFailure(Bridge.stoppedReason))
    }

    // MARK: - Product analytics

    /// Whether the published app may send product analytics, as the person
    /// set it; off holds back what is waiting as well as what comes next.
    public func setTelemetry(_ on: Bool) {
        call(()) { amux_runtime_set_telemetry($0, on) }
    }

    /// Whether a dump one of amux's checks takes is sent to the profile's
    /// account on its own, as the person set it.
    public func setAutoReports(_ on: Bool) {
        call(()) { amux_runtime_set_auto_reports($0, on) }
    }

    /// Records what only the app sees, on the profile it concerns, or on
    /// every profile when nil.
    public func record(_ event: UsageEvent, profile: String?) {
        call(()) { live in
            Bridge.optional(profile) { profile in
                Bridge.json(event).withCString { amux_runtime_record(live, profile, $0) }
            }
        }
    }

    // MARK: - Profiles

    /// Opens a profile's fleet; blocks, on the core queue, until it has
    /// caught up with the profile's store, so its opening draws what this
    /// phone holds. `moved` is told on the main actor what moved in the
    /// fleet or one of its chats, once each has opened.
    public func open(
        _ profile: String, moved: @escaping @MainActor @Sendable (Moved) -> Void
    ) throws(RuntimeFailure) -> Profile {
        let opened = loop.blocking { [self, loop] () -> Result<Profile, RuntimeFailure> in
            guard let live = loop.live else {
                return .failure(RuntimeFailure(Bridge.stoppedReason))
            }
            var error: UnsafeMutablePointer<CChar>?
            guard let opened = profile.withCString({ amux_profile_open(live, $0, &error) }) else {
                return .failure(Bridge.failure(error, otherwise: "the profile did not open"))
            }
            let open = Profile(id: profile, handle: opened, runtime: self, loop: loop)
            // Registered on the queue before the next step, so nothing that
            // moved in the new fleet goes unheard.
            loop.watch(open, moved: moved)
            return .success(open)
        }
        return try opened.get()
    }

    // MARK: - Acts

    /// Runs an act the library answers once with `{"Ok": T}` or
    /// `{"Err": reason}`.
    private func act<T: Decodable & Sendable>(
        _ type: T.Type, _ start: @escaping @Sendable (OpaquePointer) -> UInt64
    ) async -> Result<T, RuntimeFailure> {
        Bridge.answer(T.self, await loop.answer { start($0) })
    }
}

/// What moved in an open profile, as a step hands it over.
public enum Moved: Sendable {
    case fleet(FleetDelta)
    case chat(ChatDelta)
}

/// One profile's fleet and chats, open while it is on screen.
public final class Profile: @unchecked Sendable {
    /// The profile's id in the installation's registry.
    public let id: String
    private let handle: OpaquePointer
    /// The number the library's deltas name this fleet by.
    let fleet: UInt64
    /// The runtime it was opened from, which outlives it.
    public let runtime: Runtime
    private let loop: CoreLoop
    /// Held across every call into the library, so closing waits for the
    /// calls in flight and none starts after it. Recursive, because a call
    /// may be made from inside another on the same thread.
    private let lock = NSRecursiveLock()
    private var open = true

    fileprivate init(id: String, handle: OpaquePointer, runtime: Runtime, loop: CoreLoop) {
        self.id = id
        self.handle = handle
        self.fleet = amux_profile_fleet_id(handle)
        self.runtime = runtime
        self.loop = loop
    }

    /// Closes the profile's fleet; its wake stops. Close its chats first.
    /// The library keeps what an open profile needs alive however the
    /// runtime is, so this is safe after a stop as well as before.
    public func close() {
        let wasOpen = lock.withLock {
            defer { open = false }
            if open { amux_profile_close(handle) }
            return open
        }
        if wasOpen { loop.forget(self) }
    }

    deinit { close() }

    /// Runs a call on the open profile, or answers `closed` once it is not.
    private func call<T>(_ closed: T, _ body: (OpaquePointer) -> T) -> T {
        lock.withLock { open ? body(handle) : closed }
    }

    // MARK: - The fleet

    /// The fleet whole: home's sections, each family placed by its loudest
    /// member, with the members of the families `toggleFamily` listed under
    /// their heads, narrowed by the last `filter`; and every host, this
    /// phone first. Every delta after it says what moved since.
    public func fleetOpening() -> FleetDelta {
        call(FleetDelta(hosts: nil, view: nil)) { live in
            Bridge.read(FleetDelta.self, amux_fleet_opening(live)) ?? FleetDelta(hosts: nil, view: nil)
        }
    }

    /// Lists a family's members under its head, or folds them away again;
    /// the next delta lists them so.
    public func toggleFamily(_ head: AgentKey) {
        call(()) { live in
            Bridge.json(head.agent).withCString { amux_fleet_toggle_family(live, $0) }
        }
    }

    /// Narrows home's list to what `filter` keeps, as the next delta lists
    /// it.
    public func filter(_ filter: FleetFilter) {
        call(()) { live in Bridge.json(filter).withCString { amux_fleet_filter(live, $0) } }
    }

    /// The app leaving (false) or returning to (true) the foreground: every
    /// agent's stream closes, or reopens and catches up.
    public func setForeground(_ foreground: Bool) {
        call(()) { live in amux_profile_set_foreground(live, foreground) }
    }

    /// One agent's card, whether or not home lists it as a row: a folded
    /// family's members have cards too.
    public func card(_ agent: AgentKey) -> FleetCard? {
        call(nil) { live in
            Bridge.json(agent).withCString {
                Bridge.read(FleetCard?.self, amux_fleet_card(live, $0))
            } ?? nil
        }
    }

    /// The family around an agent's chat: its parent and its children.
    public func family(_ agent: AgentKey) -> FamilyHeader? {
        call(nil) { live in
            Bridge.json(agent).withCString {
                Bridge.read(FamilyHeader?.self, amux_fleet_family(live, $0))
            } ?? nil
        }
    }

    // MARK: - The profile

    /// Whether every listed agent keeps a source open now.
    public var listsSources: Bool { runtime.listsSources(id) }

    /// Reaches and authenticates a machine; nothing is trusted until the
    /// attempt is confirmed.
    public func beginPair(_ request: PairRequest) async -> Result<PendingPair, RuntimeFailure> {
        let request = Bridge.json(request)
        return await act(PendingPair.self) { live in
            request.withCString { amux_profile_begin_pair(live, $0) }
        }
    }

    public func confirmPair(_ pending: PendingPair) async -> Result<Nothing, RuntimeFailure> {
        let token = Bridge.json(pending.token)
        return await act(Nothing.self) { live in
            token.withCString { amux_profile_confirm_pair(live, $0) }
        }
    }

    public func abandonPair(_ pending: PendingPair) async -> Result<Nothing, RuntimeFailure> {
        let token = Bridge.json(pending.token)
        return await act(Nothing.self) { live in
            token.withCString { amux_profile_abandon_pair(live, $0) }
        }
    }

    /// Stops trusting a machine, telling it so where it can be reached.
    public func unpair(_ host: HostId) async -> Result<Nothing, RuntimeFailure> {
        let host = Bridge.json(host.bytes)
        return await act(Nothing.self) { live in
            host.withCString { amux_profile_unpair(live, $0) }
        }
    }

    /// This phone's identity in the profile and the machines it trusts.
    public func roster() async -> Result<Roster, RuntimeFailure> {
        await act(Roster.self) { amux_profile_roster($0) }
    }

    /// A bearer for the profile's account service.
    public func accessToken() async -> Result<Bearer, RuntimeFailure> {
        await runtime.accessToken(id)
    }

    /// Asks the account service what the account buys now.
    public func refreshEntitlement() async -> Result<Nothing, RuntimeFailure> {
        await runtime.refreshEntitlement(id)
    }

    /// Writes a dump of this profile with the fleet's and every open chat's
    /// part, and answers the bundle's directory. For a report it is the
    /// narrower dump that reproduces what the screen showed; otherwise all
    /// this phone holds.
    public func dump(reason: String, forReport: Bool) async -> Result<String, RuntimeFailure> {
        await act(String.self) { live in
            reason.withCString { amux_profile_dump(live, $0, forReport) }
        }
    }

    /// Writes a person's report around the dump `upload` names and sends it
    /// to the profile's account; answers where it is kept, and the receipt
    /// or why it was not sent.
    public func submitReport(_ upload: ReportUpload) async -> Result<ReportSent, RuntimeFailure> {
        let header = String(decoding: upload.header, as: UTF8.self)
        let (path, frame) = (upload.path, upload.frame)
        return await act(ReportSent.self) { live in
            path.withCString { path in
                header.withCString { header in
                    frame.withUnsafeBytes { bytes in
                        amux_profile_submit_report(
                            live, path, header,
                            bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count)
                    }
                }
            }
        }
    }

    // MARK: - Agents

    public func createAgent(_ agent: NewAgent) async -> Result<AgentKey, RuntimeFailure> {
        let agent = Bridge.json(agent)
        return await act(AgentKey.self) { live in
            agent.withCString { amux_profile_create_agent(live, $0) }
        }
    }

    /// What a provider offers on a host with no agent running there: what a
    /// new agent picks its model, effort, permission and mode from.
    public func hostCatalogue(
        on host: HostId, provider: Provider
    ) async -> Result<Catalogue, RuntimeFailure> {
        let (host, provider) = (Bridge.json(host.bytes), Bridge.json(provider))
        return await act(Catalogue.self) { live in
            host.withCString { host in
                provider.withCString { amux_profile_host_catalogue(live, host, $0) }
            }
        }
    }

    /// Where a host offers to start an agent, matching `query`.
    public func directories(
        on host: HostId, matching query: String, limit: UInt32
    ) async -> Result<Directories, RuntimeFailure> {
        let host = Bridge.json(host.bytes)
        return await act(Directories.self) { live in
            host.withCString { host in
                query.withCString { amux_profile_directories(live, host, $0, limit) }
            }
        }
    }

    public func act(_ act: AgentAct, on agent: AgentKey) async -> Result<Nothing, RuntimeFailure> {
        let (agent, act) = (Bridge.json(agent), Bridge.json(act))
        return await self.act(Nothing.self) { live in
            agent.withCString { agent in
                act.withCString { amux_profile_agent_act(live, agent, $0) }
            }
        }
    }

    // MARK: - Chats

    /// Opens an agent's chat with `tail` rows, or the configured number when
    /// zero. Blocks, on the core queue, until the rows this phone holds are
    /// applied, so the first read is right even with the agent's host away.
    public func openChat(_ agent: AgentKey, tail: UInt32 = 0) throws(RuntimeFailure) -> Chat {
        let agent = Bridge.json(agent)
        let opened = loop.blocking { [self] () -> Result<Live, RuntimeFailure> in
            var error: UnsafeMutablePointer<CChar>?
            let opened = call(nil) { live in
                agent.withCString { amux_session_open(live, $0, tail, &error) }
            }
            guard let opened else {
                return .failure(Bridge.failure(error, otherwise: "the chat did not open"))
            }
            return .success(Live(opened))
        }
        return Chat(handle: try opened.get().pointer, loop: loop)
    }

    // MARK: - Acts

    private func act<T: Decodable & Sendable>(
        _ type: T.Type, _ start: @escaping @Sendable (OpaquePointer) -> UInt64
    ) async -> Result<T, RuntimeFailure> {
        Bridge.answer(T.self, await loop.answer { [self] _ in call(nil) { start($0) } })
    }
}

/// An act that answers nothing but whether it happened.
public struct Nothing: Decodable, Sendable, Equatable {
    public init() {}
    public init(from decoder: any Decoder) throws {}
}

/// A pointer the library handed back, carried from the core queue to the
/// thread that asked for it.
struct Live: @unchecked Sendable {
    let pointer: OpaquePointer

    init(_ pointer: OpaquePointer) {
        self.pointer = pointer
    }
}

/// The one serial queue the library's cores step on.
///
/// The library never calls into Swift. Whenever something waits for a step
/// it makes a descriptor readable; a dispatch source watching that
/// descriptor from this queue steps, and what the step hands back is
/// carried out from here: each act's answer goes to whoever awaits its
/// ticket, and what moved in the fleets and chats, and the profile list
/// when it moved, go to the main actor ahead of the answers, so whoever
/// awaits an act finds what it moved in place. The cores' timers sleep on the library's own
/// thread and come back as ordinary inputs that make the descriptor
/// readable, so nothing here arms one.
final class CoreLoop: @unchecked Sendable {
    private let queue = DispatchQueue(label: "amux.core", qos: .userInitiated)
    private let onQueue = DispatchSpecificKey<Bool>()
    private let profilesMoved: @MainActor @Sendable ([ProfileView]) -> Void

    // On the queue only.
    /// The runtime, from its start until its stop.
    private(set) var live: OpaquePointer?
    private var source: DispatchSourceRead?
    /// The acts started and not yet answered, by ticket.
    private var waiting: [UInt64: CheckedContinuation<String, Never>] = [:]

    // Under the lock: written on the queue and by a close, read on the main
    // actor.
    private let lock = NSLock()
    /// Where each open fleet's deltas go, by the number they name it by.
    private var moves: [UInt64: @MainActor @Sendable (Moved) -> Void] = [:]
    /// The profiles opened, closed before the runtime stops.
    private let opened = NSHashTable<Profile>.weakObjects()

    init(profilesMoved: @escaping @MainActor @Sendable ([ProfileView]) -> Void) {
        self.profilesMoved = profilesMoved
        queue.setSpecific(key: onQueue, value: true)
    }

    /// Runs `body` on the queue and waits for it: how a library call that
    /// blocks until the cores have done something is made, since it steps
    /// them on the thread that makes it. Not `queue.sync`, which runs the
    /// block on the calling thread where it can.
    func blocking<T>(_ body: @escaping @Sendable () -> T) -> T {
        if DispatchQueue.getSpecific(key: onQueue) == true { return body() }
        let done = Done<T>()
        queue.async {
            done.value = body()
            done.signal.signal()
        }
        done.signal.wait()
        return done.value!
    }

    /// Runs `body` on the queue with the live runtime, and answers what it
    /// did, or nil once the runtime has stopped.
    func later<T: Sendable>(_ body: @escaping @Sendable (OpaquePointer) -> T) async -> T? {
        await withCheckedContinuation { continuation in
            queue.async { [self] in continuation.resume(returning: live.map(body)) }
        }
    }

    /// Starts an act on the queue and waits for the step that answers its
    /// ticket. `start` answers the ticket, or nil when what it acts on is
    /// closed; an act that cannot start answers that the runtime stopped.
    func answer(_ start: @escaping @Sendable (OpaquePointer) -> UInt64?) async -> String {
        await withCheckedContinuation { continuation in
            queue.async { [self] in
                guard let live, let ticket = start(live), ticket != 0 else {
                    continuation.resume(returning: Bridge.stoppedAnswer)
                    return
                }
                waiting[ticket] = continuation
            }
        }
    }

    /// Watches a started runtime's descriptor. On the queue. Steps once at
    /// once, so what the start left, the profile list, reaches the main
    /// actor ahead of whoever learns there that the runtime started.
    func run(_ runtime: OpaquePointer) {
        dispatchPrecondition(condition: .onQueue(queue))
        live = runtime
        let source = DispatchSource.makeReadSource(
            fileDescriptor: amux_runtime_wake_fd(runtime), queue: queue)
        source.setEventHandler { [weak self] in self?.step() }
        source.resume()
        self.source = source
        step()
    }

    /// Closes every profile still open, stops the runtime, and answers every
    /// act still waiting that it stopped. On the queue.
    func stop() {
        dispatchPrecondition(condition: .onQueue(queue))
        for profile in lock.withLock({ opened.allObjects }) { profile.close() }
        source?.cancel()
        source = nil
        if let live { amux_runtime_stop(live) }
        live = nil
        let unanswered = waiting
        waiting = [:]
        for continuation in unanswered.values { continuation.resume(returning: Bridge.stoppedAnswer) }
    }

    func watch(_ profile: Profile, moved: @escaping @MainActor @Sendable (Moved) -> Void) {
        lock.withLock {
            opened.add(profile)
            moves[profile.fleet] = moved
        }
    }

    func forget(_ profile: Profile) {
        lock.withLock {
            opened.remove(profile)
            moves[profile.fleet] = nil
        }
    }

    private func step() {
        dispatchPrecondition(condition: .onQueue(queue))
        guard let live, let stepped = Bridge.read(Stepped.self, amux_runtime_step(live)) else {
            return
        }
        // Queued on the main actor before any answer resumes its waiter
        // there, so an act is answered after what it moved is in place.
        if stepped.profiles != nil || !stepped.fleets.isEmpty || !stepped.chats.isEmpty {
            DispatchQueue.main.async { [self] in
                MainActor.assumeIsolated {
                    // Read here, not on the queue: a fleet closed since the
                    // step hears nothing.
                    for moved in stepped.fleets {
                        lock.withLock { moves[moved.fleet] }?(.fleet(moved.delta))
                    }
                    for moved in stepped.chats {
                        lock.withLock { moves[moved.fleet] }?(.chat(moved.delta))
                    }
                    if let profiles = stepped.profiles { profilesMoved(profiles) }
                }
            }
        }
        for answer in stepped.answers {
            waiting.removeValue(forKey: answer.ticket)?.resume(returning: answer.json)
        }
    }

    /// What a step hands back.
    private struct Stepped: Decodable, Sendable {
        struct FleetMoved: Decodable, Sendable {
            let fleet: UInt64
            let delta: FleetDelta
        }

        struct ChatMoved: Decodable, Sendable {
            let fleet: UInt64
            let delta: ChatDelta
        }

        struct Answer: Decodable, Sendable {
            let ticket: UInt64
            let json: String
        }

        let profiles: [ProfileView]?
        let fleets: [FleetMoved]
        let chats: [ChatMoved]
        let answers: [Answer]
    }

    private final class Done<T>: @unchecked Sendable {
        var value: T?
        let signal = DispatchSemaphore(value: 0)
    }
}

/// The JSON the library hands back and takes.
public enum Bridge {
    /// The reason a call that returns null wrote, freed, or `otherwise`.
    static func failure(
        _ error: UnsafeMutablePointer<CChar>?, otherwise: String
    ) -> RuntimeFailure {
        let reason = error.map { String(cString: $0) } ?? otherwise
        if let error { amux_string_free(error) }
        return RuntimeFailure(reason)
    }

    /// Passes a string the library may take as null.
    static func optional<T>(_ text: String?, _ body: (UnsafePointer<CChar>?) -> T) -> T {
        guard let text else { return body(nil) }
        return text.withCString { body($0) }
    }

    static func json<T: Encodable>(_ value: T) -> String {
        guard let data = try? JSONEncoder().encode(value) else { return "null" }
        return String(decoding: data, as: UTF8.self)
    }

    /// Decodes a string the library returned, and frees it.
    static func read<T: Decodable>(_ type: T.Type, _ text: UnsafeMutablePointer<CChar>?) -> T? {
        guard let text else { return nil }
        defer { amux_string_free(text) }
        let data = Data(bytesNoCopy: text, count: strlen(text), deallocator: .none)
        return try? JSONDecoder().decode(T.self, from: data)
    }

    /// What an act answers once the runtime has stopped, in the library's
    /// own words for it.
    static let stoppedReason = "the runtime has stopped"
    static let stoppedAnswer = #"{"Err":"the runtime has stopped"}"#

    /// An act's one `{"Ok": T}` or `{"Err": reason}`.
    static func answer<T: Decodable & Sendable>(
        _ type: T.Type, _ text: String
    ) -> Result<T, RuntimeFailure> {
        guard let answer = try? JSONDecoder().decode(Answer<T>.self, from: Data(text.utf8)) else {
            return .failure(RuntimeFailure("the runtime answered what this build cannot read"))
        }
        switch answer {
        case .ok(let value): return .success(value)
        case .err(let reason): return .failure(RuntimeFailure(reason))
        }
    }

    /// An act's JSON that is the value itself, not an answer.
    static func value<T: Decodable & Sendable>(_ type: T.Type, _ text: String) -> T? {
        try? JSONDecoder().decode(T.self, from: Data(text.utf8))
    }

    private enum Answer<T: Decodable>: Decodable {
        case ok(T)
        case err(String)

        private enum Key: String, CodingKey {
            case ok = "Ok"
            case err = "Err"
        }

        init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: Key.self)
            if let reason = try container.decodeIfPresent(String.self, forKey: .err) {
                self = .err(reason)
            } else if T.self == Nothing.self, try container.decodeNil(forKey: .ok) {
                self = .ok(Nothing() as! T)
            } else {
                self = .ok(try container.decode(T.self, forKey: .ok))
            }
        }
    }
}

extension Profile: ReportSubmitting {}
