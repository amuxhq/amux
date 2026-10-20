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

/// The shared Rust runtime: this phone's installation, hosted in the app's
/// own process, with one profile per account exactly as a desktop has.
///
/// Every value crosses as JSON whose Swift mirror is generated from the Rust
/// definition, so nothing here interprets a row, a card or a host; it only
/// hands them over. Reads return at once. Acts that wait on a machine are
/// `async` and resume on whatever thread the library answers on.
///
/// The wake is called on a library worker with 0 whenever the profile list
/// moved; the owner reads it again on the main actor. A profile's fleet and
/// chats are a `Profile` opened from here.
public final class Runtime: @unchecked Sendable {
    private let handle: OpaquePointer
    private let waker: Unmanaged<Waker>
    /// Held across every call into the library, so stopping waits for the
    /// calls in flight and none starts after it. Recursive, because a call
    /// may be made from inside another on the same thread.
    private let lock = NSRecursiveLock()
    private var running = true
    /// The profiles opened from this runtime, closed before it stops.
    private let opened = NSHashTable<Profile>.weakObjects()

    private init(handle: OpaquePointer, waker: Unmanaged<Waker>) {
        self.handle = handle
        self.waker = waker
    }

    /// Starts the runtime; blocks until every profile's store is open.
    public static func start(
        _ config: StartConfig, wake: @escaping @Sendable (UInt64) -> Void
    ) throws(RuntimeFailure) -> Runtime {
        let waker = Unmanaged.passRetained(Waker(wake))
        var error: UnsafeMutablePointer<CChar>?
        let started = Bridge.json(config).withCString { config in
            amux_runtime_start(config, Waker.call, waker.toOpaque(), &error)
        }
        guard let started else {
            waker.release()
            throw Bridge.failure(error, otherwise: "the runtime did not start")
        }
        return Runtime(handle: started, waker: waker)
    }

    /// Stops the runtime and flushes every profile's store, closing every
    /// profile opened from it first. Close their chats before that; any read
    /// after this answers nothing.
    public func stop() {
        // One hold of the lock from the first close to the stop: no open
        // can register a profile this misses, and a profile closing itself
        // meanwhile does so before or after, never between. The lock is
        // recursive, so each close's call back in is admitted.
        let wasRunning = lock.withLock {
            for profile in opened.allObjects { profile.close() }
            defer { running = false }
            return running
        }
        guard wasRunning else { return }
        amux_runtime_stop(handle)
        waker.release()
    }

    /// Runs a call on the live runtime, or answers `stopped` once it is not.
    fileprivate func call<T>(_ stopped: T, _ body: (OpaquePointer) -> T) -> T {
        lock.withLock { running ? body(handle) : stopped }
    }

    deinit { stop() }

    // MARK: - The profile registry

    /// Every profile, oldest first.
    public func profiles() -> [ProfileView] {
        call([]) { Bridge.read([ProfileView].self, amux_runtime_profiles($0)) ?? [] }
    }

    /// A new profile nobody has signed in on.
    public func createProfile() async -> Result<ProfileView, RuntimeFailure> {
        await act(ProfileView.self) { live, callback, context in
            amux_runtime_create_profile(live, callback, context)
        }
    }

    /// Deletes a profile: its key, the machines it trusts and its store.
    /// The installation keeps at least one. Close it first.
    public func deleteProfile(_ profile: String) async -> Result<Nothing, RuntimeFailure> {
        await act(Nothing.self) { live, callback, context in
            profile.withCString { amux_runtime_delete_profile(live, $0, callback, context) }
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
        await act(ProfileView.self) { live, callback, context in
            Bridge.optional(profile) { profile in
                cloud.absoluteString.withCString { cloud in
                    client.withCString { client in
                        refreshToken.withCString { token in
                            amux_runtime_bind(live, profile, cloud, client, token, callback, context)
                        }
                    }
                }
            }
        }
    }

    /// Signs a profile out; it stays tied to its account.
    public func signOut(_ profile: String) async -> Result<ProfileView, RuntimeFailure> {
        await act(ProfileView.self) { live, callback, context in
            profile.withCString { amux_runtime_sign_out(live, $0, callback, context) }
        }
    }

    /// Holds a bound profile's relay link down; its direct links and store
    /// stay.
    public func pause(_ profile: String) async -> Result<ProfileView, RuntimeFailure> {
        await act(ProfileView.self) { live, callback, context in
            profile.withCString { amux_runtime_pause(live, $0, callback, context) }
        }
    }

    public func resume(_ profile: String) async -> Result<ProfileView, RuntimeFailure> {
        await act(ProfileView.self) { live, callback, context in
            profile.withCString { amux_runtime_resume(live, $0, callback, context) }
        }
    }

    /// Every profile whose trust store holds a machine, oldest first.
    public func trusting(_ host: HostId) async -> Result<[String], RuntimeFailure> {
        await act([String].self) { live, callback, context in
            Bridge.json(host.bytes).withCString {
                amux_runtime_trusting_profiles(live, $0, callback, context)
            }
        }
    }

    /// Opens pairing mode on a profile and answers the link its QR code
    /// would carry. Only a driving build offers one.
    public func offerPairing(_ profile: String) async -> Result<String, RuntimeFailure> {
        await act(String.self) { live, callback, context in
            profile.withCString { amux_runtime_offer_pairing(live, $0, callback, context) }
        }
    }

    /// Listed for the profile in front of somebody: every listed agent keeps
    /// a source open. Not listed for every other profile, and in the
    /// background: only the chats that open keep one.
    public func setSourcePolicy(_ profile: String, listed: Bool) {
        call(()) { live in
            profile.withCString { amux_runtime_set_source_policy(live, $0, listed) }
        }
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
        await act(Bearer.self) { live, callback, context in
            profile.withCString { amux_runtime_access_token(live, $0, callback, context) }
        }
    }

    /// Asks the account service what a profile's account buys now, over its
    /// relay link, so the relay's tier follows a purchase at once.
    public func refreshEntitlement(_ profile: String) async -> Result<Nothing, RuntimeFailure> {
        await act(Nothing.self) { live, callback, context in
            profile.withCString { amux_runtime_refresh_entitlement(live, $0, callback, context) }
        }
    }

    // MARK: - Product analytics

    /// Whether the published app may send product analytics, as the person
    /// set it; off holds back what is waiting as well as what comes next.
    public func setTelemetry(_ on: Bool) {
        call(()) { amux_runtime_set_telemetry($0, on) }
    }

    /// Somebody brought the app to the front; counted at most once an hour.
    public func foreground() {
        call(()) { amux_runtime_foreground($0) }
    }

    /// The app is leaving the screen: sends what analytics is waiting, for
    /// a few seconds at most, before the system may suspend it.
    public func background() async {
        _ = await Bridge.value(Nothing?.self) { callback, context in
            let ran = call(false) { live in
                amux_runtime_background(live, callback, context)
                return true
            }
            if !ran { Bridge.stopped(callback, context) }
        }
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

    /// Opens a profile's fleet; blocks until it has caught up with the
    /// profile's store, so the first read draws what this phone holds. The
    /// wake is called with a chat's id when it moved, or 0 when the fleet
    /// did.
    public func open(
        _ profile: String, wake: @escaping @Sendable (UInt64) -> Void
    ) throws(RuntimeFailure) -> Profile {
        let waker = Unmanaged.passRetained(Waker(wake))
        var error: UnsafeMutablePointer<CChar>?
        // Opened and registered under one hold of the lock: a stop that
        // runs between the two would close the profiles it saw and miss
        // this one, which would then outlive the runtime unclosed.
        let open: Profile? = call(nil) { live in
            let opened = profile.withCString {
                amux_profile_open(live, $0, Waker.call, waker.toOpaque(), &error)
            }
            guard let opened else { return nil }
            let open = Profile(id: profile, handle: opened, waker: waker, runtime: self)
            self.opened.add(open)
            return open
        }
        guard let open else {
            waker.release()
            throw Bridge.failure(error, otherwise: "the profile did not open")
        }
        return open
    }

    // MARK: - Acts

    /// Runs an act the library answers once with `{"Ok": T}` or
    /// `{"Err": reason}`.
    private func act<T: Decodable & Sendable>(
        _ type: T.Type,
        _ body: @escaping (OpaquePointer, AmuxCallback, UnsafeMutableRawPointer) -> Void
    ) async -> Result<T, RuntimeFailure> {
        await Bridge.answer(T.self) { callback, context in
            let ran = self.call(false) { live in
                body(live, callback, context)
                return true
            }
            if !ran { Bridge.stopped(callback, context) }
        }
    }
}

/// One profile's fleet and chats, open while it is on screen.
public final class Profile: @unchecked Sendable {
    /// The profile's id in the installation's registry.
    public let id: String
    private let handle: OpaquePointer
    private let waker: Unmanaged<Waker>
    /// The runtime it was opened from, which outlives it.
    public let runtime: Runtime
    /// Held across every call into the library, so closing waits for the
    /// calls in flight and none starts after it.
    private let lock = NSRecursiveLock()
    private var open = true

    fileprivate init(id: String, handle: OpaquePointer, waker: Unmanaged<Waker>, runtime: Runtime) {
        self.id = id
        self.handle = handle
        self.waker = waker
        self.runtime = runtime
    }

    /// Closes the profile's fleet; its wake stops. Close its chats first.
    public func close() {
        // The flag turns and the library is told under the runtime's lock,
        // so a stop sees this profile either still open, and closes it, or
        // already closed in the library, never closed in name only.
        let wasOpen = runtime.call(false) { _ in
            lock.withLock {
                defer { open = false }
                if open { amux_profile_close(handle) }
                return open
            }
        }
        if wasOpen { waker.release() }
    }

    deinit { close() }

    /// Runs a call on the open profile of a running runtime, or answers
    /// `closed` once either is not.
    private func call<T>(_ closed: T, _ body: (OpaquePointer) -> T) -> T {
        runtime.call(closed) { _ in lock.withLock { open ? body(handle) : closed } }
    }

    // MARK: - The fleet

    /// Home's sections, each family placed by its loudest member, with the
    /// members of the families whose root agent ids `expanding` names
    /// under their heads, listing the agents `filter` keeps.
    public func fleetView(expanding: [[UInt8]] = [], filter: FleetFilter? = nil) -> FleetView {
        call(FleetView(sections: [], needYou: 0, working: 0)) { live in
            Bridge.json(expanding).withCString { expand in
                Bridge.json(filter).withCString {
                    Bridge.read(FleetView.self, amux_fleet_view(live, expand, $0))
                }
            } ?? FleetView(sections: [], needYou: 0, working: 0)
        }
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

    /// Every host in the fleet, this phone first.
    public func hosts() -> [HostView] {
        call([]) { live in
            Bridge.read([HostView].self, amux_fleet_hosts(live)) ?? []
        }
    }

    /// What moved in the fleet since the last take; the next change wakes
    /// the owner again.
    public func takeFleetChanges() -> FleetChanges {
        call(FleetChanges(agents: [], hosts: false)) { live in
            Bridge.read(FleetChanges.self, amux_fleet_take_changes(live))
                ?? FleetChanges(agents: [], hosts: false)
        }
    }

    // MARK: - The profile

    /// Listed in the foreground: every listed agent keeps a source open.
    /// Not listed when a push woke the app in the background: only the chats
    /// that open keep one.
    public func setSourcePolicy(listed: Bool) {
        runtime.setSourcePolicy(id, listed: listed)
    }

    /// Whether every listed agent keeps a source open now.
    public var listsSources: Bool { runtime.listsSources(id) }

    /// Reaches and authenticates a machine; nothing is trusted until the
    /// attempt is confirmed.
    public func beginPair(_ request: PairRequest) async -> Result<PendingPair, RuntimeFailure> {
        await act(PendingPair.self) { live, callback, context in
            Bridge.json(request).withCString {
                amux_profile_begin_pair(live, $0, callback, context)
            }
        }
    }

    public func confirmPair(_ pending: PendingPair) async -> Result<Paired, RuntimeFailure> {
        await act(Paired.self) { live, callback, context in
            Bridge.json(pending.token).withCString {
                amux_profile_confirm_pair(live, $0, callback, context)
            }
        }
    }

    public func abandonPair(_ pending: PendingPair) async -> Result<Nothing, RuntimeFailure> {
        await act(Nothing.self) { live, callback, context in
            Bridge.json(pending.token).withCString {
                amux_profile_abandon_pair(live, $0, callback, context)
            }
        }
    }

    /// Stops trusting a machine, telling it so where it can be reached.
    public func unpair(_ host: HostId) async -> Result<Nothing, RuntimeFailure> {
        await act(Nothing.self) { live, callback, context in
            Bridge.json(host.bytes).withCString {
                amux_profile_unpair(live, $0, callback, context)
            }
        }
    }

    /// This phone's identity in the profile and the machines it trusts.
    public func roster() async -> Result<Roster, RuntimeFailure> {
        await act(Roster.self) { live, callback, context in
            amux_profile_roster(live, callback, context)
        }
    }

    public func account() async -> Result<AccountView, RuntimeFailure> {
        await act(AccountView.self) { live, callback, context in
            amux_profile_account(live, callback, context)
        }
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
    /// part, and answers the bundle's directory.
    public func dump(reason: String) async -> Result<String, RuntimeFailure> {
        await act(String.self) { live, callback, context in
            reason.withCString { amux_profile_dump(live, $0, callback, context) }
        }
    }

    // MARK: - Agents

    public func createAgent(_ agent: NewAgent) async -> Result<AgentKey, RuntimeFailure> {
        await act(AgentKey.self) { live, callback, context in
            Bridge.json(agent).withCString {
                amux_profile_create_agent(live, $0, callback, context)
            }
        }
    }

    /// What a provider offers on a host with no agent running there: what a
    /// new agent picks its model, effort, permission and mode from.
    public func hostCatalogue(
        on host: HostId, provider: Provider
    ) async -> Result<Catalogue, RuntimeFailure> {
        await act(Catalogue.self) { live, callback, context in
            Bridge.json(host.bytes).withCString { host in
                Bridge.json(provider).withCString { provider in
                    amux_profile_host_catalogue(live, host, provider, callback, context)
                }
            }
        }
    }

    /// Where a host offers to start an agent, matching `query`.
    public func directories(
        on host: HostId, matching query: String, limit: UInt32
    ) async -> Result<Directories, RuntimeFailure> {
        await act(Directories.self) { live, callback, context in
            Bridge.json(host.bytes).withCString { host in
                query.withCString { query in
                    amux_profile_directories(live, host, query, limit, callback, context)
                }
            }
        }
    }

    public func act(_ act: AgentAct, on agent: AgentKey) async -> Result<Nothing, RuntimeFailure> {
        await self.act(Nothing.self) { live, callback, context in
            Bridge.json(agent).withCString { agent in
                Bridge.json(act).withCString { act in
                    amux_profile_agent_act(live, agent, act, callback, context)
                }
            }
        }
    }

    // MARK: - Chats

    /// Opens an agent's chat with `tail` rows, or the configured number when
    /// zero. Blocks until the rows this phone holds are applied, so the
    /// first read is right even with the agent's host away.
    public func openChat(_ agent: AgentKey, tail: UInt32 = 0) throws(RuntimeFailure) -> Chat {
        var error: UnsafeMutablePointer<CChar>?
        let opened = call(nil) { live in
            Bridge.json(agent).withCString { amux_session_open(live, $0, tail, &error) }
        }
        guard let opened else {
            throw Bridge.failure(error, otherwise: "the chat did not open")
        }
        return Chat(handle: opened)
    }

    // MARK: - Acts

    private func act<T: Decodable & Sendable>(
        _ type: T.Type,
        _ body: @escaping (OpaquePointer, AmuxCallback, UnsafeMutableRawPointer) -> Void
    ) async -> Result<T, RuntimeFailure> {
        await Bridge.answer(T.self) { callback, context in
            let ran = self.call(false) { live in
                body(live, callback, context)
                return true
            }
            if !ran { Bridge.stopped(callback, context) }
        }
    }
}

/// What a pairing trusted.
public struct Paired: Decodable, Sendable, Equatable {
    public var hostId: [UInt8]
    public var name: String

    private enum CodingKeys: String, CodingKey {
        case hostId = "host_id"
        case name
    }
}

/// An act that answers nothing but whether it happened.
public struct Nothing: Decodable, Sendable, Equatable {
    public init() {}
    public init(from decoder: any Decoder) throws {}
}

/// The owner's wake, held where the library can reach it from its worker.
private final class Waker: @unchecked Sendable {
    let wake: @Sendable (UInt64) -> Void

    init(_ wake: @escaping @Sendable (UInt64) -> Void) {
        self.wake = wake
    }

    /// What the library calls, with the waker as its context.
    static let call: AmuxWake = { context, chat in
        guard let context else { return }
        Unmanaged<Waker>.fromOpaque(context).takeUnretainedValue().wake(chat)
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

    /// Waits for a callback's one `{"Ok": T}` or `{"Err": reason}`.
    static func answer<T: Decodable & Sendable>(
        _ type: T.Type, _ call: (AmuxCallback, UnsafeMutableRawPointer) -> Void
    ) async -> Result<T, RuntimeFailure> {
        let text: String = await withCheckedContinuation { continuation in
            let reply = Unmanaged.passRetained(Reply(continuation)).toOpaque()
            call({ context, json in
                guard let context else { return }
                let reply = Unmanaged<Reply>.fromOpaque(context).takeRetainedValue()
                reply.continuation.resume(returning: json.map { String(cString: $0) } ?? "null")
            }, reply)
        }
        guard let answer = try? JSONDecoder().decode(Answer<T>.self, from: Data(text.utf8)) else {
            return .failure(RuntimeFailure("the runtime answered what this build cannot read"))
        }
        switch answer {
        case .ok(let value): return .success(value)
        case .err(let reason): return .failure(RuntimeFailure(reason))
        }
    }

    /// Answers a callback at once for a call that can no longer be made, so
    /// whoever waits on it is not left waiting.
    static func stopped(_ callback: AmuxCallback, _ context: UnsafeMutableRawPointer) {
        #"{"Err":"the runtime has stopped"}"#.withCString { callback(context, $0) }
    }

    /// Decodes a callback's JSON that is the value itself, not an answer.
    static func value<T: Decodable & Sendable>(
        _ type: T.Type, _ call: (AmuxCallback, UnsafeMutableRawPointer) -> Void
    ) async -> T? {
        let text: String = await withCheckedContinuation { continuation in
            let reply = Unmanaged.passRetained(Reply(continuation)).toOpaque()
            call({ context, json in
                guard let context else { return }
                let reply = Unmanaged<Reply>.fromOpaque(context).takeRetainedValue()
                reply.continuation.resume(returning: json.map { String(cString: $0) } ?? "null")
            }, reply)
        }
        return try? JSONDecoder().decode(T.self, from: Data(text.utf8))
    }

    private final class Reply: @unchecked Sendable {
        let continuation: CheckedContinuation<String, Never>

        init(_ continuation: CheckedContinuation<String, Never>) {
            self.continuation = continuation
        }
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
