import AmuxValues
import Foundation
import Observation

/// A chat session a bundle holds open: the profile's, or one a test hands in.
public protocol OpenChat: ChatSource {
    var id: UInt64 { get }
    func close()
}

extension Chat: OpenChat {}

/// Everything one account's screens draw, over the profile that serves it.
///
/// The profile wakes this once per turn for however much changed; the
/// bundle takes the changes and reads the fleet and hosts again. Acts go
/// to the profile and their answers land in the store the screen draws.
@MainActor
@Observable
public final class StoreBundle {
    public let account: AccountId
    public let fleet: FleetStore
    public let hosts: HostsStore
    public let pairing: PairingStore
    public let newAgent: NewAgentStore
    /// How many times the fleet was read, for a driver waiting on one.
    public private(set) var applied = 0
    /// The launch's reconciliation is marked once: the first read in which
    /// every trusted host was current.
    @ObservationIgnored private var markedReconciled = false

    /// The profile serving this account, while it is on screen.
    @ObservationIgnored public private(set) var profile: Profile?
    /// Open chats by the id the profile's wake names them with.
    @ObservationIgnored private var chats: [UInt64: (chat: OpenChat, woke: @MainActor () -> Void)] = [:]
    /// The chat sessions held open, by agent: every chat a page shows,
    /// chats viewed within the retention, and a chat a push is bringing
    /// current.
    @ObservationIgnored private var models: [AgentKey: (chat: OpenChat, model: ChatModel)] = [:]
    /// The agents whose chat a page shows.
    @ObservationIgnored private var shown: Set<AgentKey> = []
    /// The agents whose chat a push is bringing current.
    @ObservationIgnored private var warming: Set<AgentKey> = []
    /// When a page last stopped showing each chat.
    @ObservationIgnored private var viewed: [AgentKey: Date] = [:]
    /// How long a chat nobody shows stays open after it was last viewed.
    @ObservationIgnored public let sessionRetention: Duration
    /// Opens a session; the profile's unless a test hands one in.
    @ObservationIgnored private let opener: (@MainActor (AgentKey) throws(RuntimeFailure) -> OpenChat)?
    /// Acts on an agent; the profile's unless a test hands one in.
    @ObservationIgnored private let acting: (@MainActor (AgentAct, AgentKey) async -> Result<Nothing, RuntimeFailure>)?
    @ObservationIgnored public let now: @MainActor () -> Date
    /// Told how many machines this account has paired and how many of its
    /// agents need the person, each time the fleet is read.
    @ObservationIgnored public var saw: (@MainActor (_ hosts: Int, _ attention: Int) -> Void)?
    /// Whether a re-read of a relay link still coming up is scheduled.
    @ObservationIgnored private var settling = false
    /// A fleet wake that arrived while the launch's first frame was going up.
    @ObservationIgnored private var held = false
    /// How long a fleet wake waits for the launch's first frame at most.
    public static let firstFrameHold: Duration = .milliseconds(250)
    /// When the fleet was last read for a wake, whether a read is waiting
    /// out the spacing after it, and whether changes taken since are unread.
    @ObservationIgnored private var lastFleetRead: ContinuousClock.Instant?
    @ObservationIgnored private var spacing = false
    @ObservationIgnored private var owed = false
    /// The least time between two fleet reads a wake asks for.
    public static let fleetReadSpacing: Duration = .milliseconds(250)

    public init(
        account: AccountId, clock: @escaping @MainActor () -> Date = { Date() },
        sessionRetention: Duration = StoreBundle.sessionRetention,
        opener: (@MainActor (AgentKey) throws(RuntimeFailure) -> OpenChat)? = nil,
        acting: (@MainActor (AgentAct, AgentKey) async -> Result<Nothing, RuntimeFailure>)? = nil
    ) {
        self.account = account
        self.now = clock
        self.sessionRetention = sessionRetention
        self.opener = opener
        self.acting = acting
        self.fleet = FleetStore(now: clock())
        self.hosts = HostsStore(clock: clock)
        self.pairing = PairingStore(clock: clock)
        self.newAgent = NewAgentStore()
        // Read after the refresh that put the frame up, not inside it.
        fleet.presented = { [weak self] in
            guard let self, self.held else { return }
            self.held = false
            Task { [weak self] in self?.woke(0) }
        }
    }

    /// Starts drawing from a profile, or stops when it is gone.
    public func attach(_ profile: Profile?) {
        self.profile = profile
        guard profile != nil else { return }
        read(hostsMoved: true)
    }

    /// The profile moved: the fleet when `chat` is 0, else that chat.
    public func woke(_ chat: UInt64) {
        if chat == 0 {
            guard let profile else { return }
            // The runtime wakes the fleet as soon as it opens, for its
            // agents' sessions starting; the second lines they bring arrive
            // a moment after the remembered rows. Applied before the launch's
            // first frame is on screen, they lay the rows out again inside
            // it, so they wait for that frame, and not for long.
            if fleet.presenting {
                if !held {
                    held = true
                    Task { [weak self] in
                        try? await Task.sleep(for: Self.firstFrameHold)
                        self?.fleet.finishPresenting()
                    }
                }
                return
            }
            // A working agent's step moving wakes home, which for a
            // streaming turn is many times a second, and each read lays out
            // the whole fleet on the main thread. Agents' changes soon after
            // a read wait out the spacing and one read takes them all; a
            // host moving is read at once, since that is what a reconnecting
            // fleet is waiting on.
            let changes = profile.takeFleetChanges()
            let at = ContinuousClock.now
            if !changes.hosts, !changes.agents.isEmpty,
               let last = lastFleetRead, at - last < Self.fleetReadSpacing {
                owed = true
                if !spacing {
                    spacing = true
                    Task { [weak self] in await self?.readOwed() }
                }
                return
            }
            lastFleetRead = at
            read(hostsMoved: changes.hosts)
        } else {
            chats[chat]?.woke()
        }
    }

    /// Reads the fleet for the changes a wake left owed once the spacing
    /// after the latest read has passed.
    private func readOwed() async {
        while let last = lastFleetRead, ContinuousClock.now < last + Self.fleetReadSpacing {
            try? await Task.sleep(until: last + Self.fleetReadSpacing)
        }
        spacing = false
        guard owed else { return }
        lastFleetRead = ContinuousClock.now
        read(hostsMoved: false)
    }

    private func read(hostsMoved: Bool) {
        guard let profile else { return }
        owed = false
        let views = profile.hosts()
        fleet.show(profile.fleetView(), hosts: views)
        hosts.show(views)
        if !markedReconciled {
            let trusted = views.filter { $0.trusted && !$0.local }
            if !trusted.isEmpty, trusted.allSatisfy(\.current) {
                markedReconciled = true
                Signposts.emitWhenPresented(.reconciled)
            }
        }
        newAgent.remember(fleet.rows)
        saw?(fleet.machines.filter(\.trusted).count, fleet.needYou)
        applied += 1
        keepSessions()
        if hostsMoved {
            Task { await refreshAccount() }
            if hosts.rosterBehind { Task { await refreshRoster() } }
        }
    }

    /// Reads the account and its relay link again.
    public func refreshAccount() async {
        guard let profile, case .success(let account) = await profile.account() else { return }
        hosts.show(account)
        fleet.relay(account.relay)
        // The relay link settles without the host list moving, so while it
        // is still coming up it is read again until it says where it landed.
        guard account.relay == .connecting || account.relay == .retrying,
              !settling else { return }
        settling = true
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(1))
            guard let self, self.profile === profile else { return }
            self.settling = false
            await self.refreshAccount()
        }
    }

    /// Reads this phone's identity and the machines it trusts again.
    public func refreshRoster() async {
        guard let profile, case .success(let roster) = await profile.roster() else { return }
        hosts.show(roster)
    }

    /// Narrows the list to what `filter` keeps.
    public func filterFleet(_ filter: FleetFilter) {
        fleet.filter = filter
        profile?.filter(filter)
        read(hostsMoved: false)
    }

    /// Lists or folds a family's members.
    public func toggleFamily(_ head: AgentKey) {
        profile?.toggleFamily(head)
        read(hostsMoved: false)
    }

    // MARK: - Chats

    /// A viewed chat stays open this long after its page leaves: long
    /// enough that going back to it is instant, short enough that the
    /// sessions a phone holds follow what the person is looking at.
    public static let sessionRetention: Duration = .seconds(300)

    /// Opens an agent's chat and routes its wakes to `woke`.
    public func openChat(
        _ agent: AgentKey, woke: @escaping @MainActor () -> Void
    ) throws(RuntimeFailure) -> OpenChat {
        let chat: OpenChat
        if let opener {
            chat = try opener(agent)
        } else {
            guard let profile else { throw RuntimeFailure("nothing is running for this account") }
            chat = try profile.openChat(agent)
        }
        chats[chat.id] = (chat, woke)
        fleet.opened(agent, at: now())
        return chat
    }

    public func closeChat(_ chat: OpenChat) {
        chats.removeValue(forKey: chat.id)
        chat.close()
    }

    /// The chat a page shows, opened the first time it is asked for and kept
    /// while a page shows it: a child's chat pushed on top keeps the
    /// parent's draft and place.
    public func chat(_ agent: AgentKey) throws(RuntimeFailure) -> ChatModel {
        shown.insert(agent)
        return try model(agent)
    }

    private func model(_ agent: AgentKey) throws(RuntimeFailure) -> ChatModel {
        if let open = models[agent] { return open.model }
        let chat = try openChat(agent) { [weak self] in self?.models[agent]?.model.woke() }
        let model = ChatModel(source: chat)
        if let kept = chat.keptDraft() { model.reopen(kept) }
        model.keeping = { chat.keep($0) }
        models[agent] = (chat, model)
        return model
    }

    /// Whether a session is open on an agent's chat, shown or not.
    public func holds(_ agent: AgentKey) -> Bool { models[agent] != nil }

    /// The page showing this chat is gone. The session stays for the
    /// retention, so coming back to it is instant, then closes.
    public func leave(_ agent: AgentKey) {
        shown.remove(agent)
        viewed[agent] = now()
        Task { [weak self, retention = sessionRetention] in
            try? await Task.sleep(for: retention)
            self?.keepSessions()
        }
    }

    /// Which chat sessions exist, after every read of the fleet. A shown
    /// chat keeps its own; a chat viewed within the retention keeps its
    /// own; a chat a push is bringing current keeps its own until it is; a
    /// chat holding a prompt for a host that cannot be reached keeps its
    /// own until the prompt goes, since only the open chat takes it back out
    /// of the kept draft;
    /// nothing else is opened or kept, not even for an agent that
    /// needs the person, because the runtime keeps every listed agent's
    /// rows current without a session and a chat opens from those rows.
    func keepSessions() {
        let at = now()
        let retention = Double(sessionRetention.components.seconds)
            + Double(sessionRetention.components.attoseconds) / 1e18
        for (agent, open) in models
        where !shown.contains(agent) && !warming.contains(agent) && !open.model.holdsPrompts {
            if let last = viewed[agent], at.timeIntervalSince(last) < retention { continue }
            models.removeValue(forKey: agent)
            viewed.removeValue(forKey: agent)
            closeChat(open.chat)
        }
    }

    /// Brings one agent's chat current, for a push that woke the app in the
    /// background: the ordinary open, which asks the agent's host for what
    /// this phone missed, then a wait for it to catch up. Nothing else is
    /// brought current. Afterwards the chat is kept or closed like any
    /// other; its rows stay in the store, so tapping the notification opens
    /// it at once.
    @discardableResult
    public func warm(_ agent: AgentKey, within limit: Duration = .seconds(25)) async -> Bool {
        guard let model = try? model(agent) else { return false }
        // Every read of the fleet asks which sessions to keep; the one
        // catching up must survive them until it is current.
        warming.insert(agent)
        let deadline = ContinuousClock.now + limit
        while model.frame?.caughtUp != true, ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(100))
        }
        let current = model.frame?.caughtUp == true
        warming.remove(agent)
        keepSessions()
        return current
    }

    /// Closes every chat, before the profile under them closes.
    public func closeChats() {
        for open in chats.values { open.chat.close() }
        chats.removeAll()
        models.removeAll()
        shown.removeAll()
        viewed.removeAll()
    }

    // MARK: - Pairing

    /// Reaches a machine with the six digits typed so far, once there are six.
    @discardableResult
    public func pair(digits typed: String) -> Bool {
        pairing.enter(typed)
        guard let request = pairing.request else { return false }
        begin(request, relayOnly: pairing.machine.map { $0.addrs.isEmpty } ?? true)
        return true
    }

    /// Reaches the machine a pairing link names. A link that carries no
    /// addresses reaches its machine only through the relay, so on an account
    /// that does not buy the relay its refusal is the offer of a
    /// subscription, as a code's is.
    @discardableResult
    public func pair(link: String, relayOnly: Bool = false) -> Bool {
        guard profile != nil else { return false }
        pairing.open()
        begin(.link(link), relayOnly: relayOnly)
        return true
    }

    private func begin(_ request: PairRequest, relayOnly: Bool) {
        guard let profile else { return }
        let attempt = pairing.checking()
        // A machine only the relay can reach is refused on an account that
        // does not buy the relay; said as such rather than as a wrong code.
        let unsubscribed = relayOnly && hosts.account?.pro == false
        Task {
            let reached = await profile.beginPair(request)
            pairing.reached(reached, attempt: attempt, relayOnly: unsubscribed)
        }
    }

    public func confirmPairing(_ pending: PendingPair) {
        guard let profile else { return }
        let attempt = pairing.checking()
        Task {
            let paired = await profile.confirmPair(pending)
            pairing.paired(paired, attempt: attempt)
            await refreshRoster()
        }
    }

    public func abandonPairing(_ pending: PendingPair) {
        pairing.abandoned()
        guard let profile else { return }
        Task { _ = await profile.abandonPair(pending) }
    }

    /// Stops trusting a machine at once; the row leaves when the profile
    /// confirms.
    public func revoke(_ host: HostId) {
        guard let profile else { return }
        Task {
            _ = await profile.unpair(host)
            await refreshRoster()
        }
    }

    // MARK: - Starting an agent

    public func startNewAgent(on host: HostId?) {
        newAgent.open(on: host)
        newAgent.remember(fleet.rows)
        guard let host else { return }
        listDirectories(on: host, matching: "")
        askOffers(on: host)
    }

    public func point(at host: HostId) {
        guard newAgent.machine != host else { return }
        newAgent.point(at: host)
        listDirectories(on: host, matching: "")
        askOffers(on: host)
    }

    /// Asks the host what each provider offers there, for the screen's
    /// model, effort, permission and mode choices.
    private func askOffers(on host: HostId) {
        for provider in NewAgentStore.Provider.allCases {
            let asked = newAgent.askingOffer(for: provider)
            guard let profile else {
                newAgent.offered(
                    .failure(RuntimeFailure("nothing is running")), for: provider, asked: asked)
                continue
            }
            Task {
                let offered = await profile.hostCatalogue(on: host, provider: provider.wire)
                newAgent.offered(offered, for: provider, asked: asked)
            }
        }
    }

    public func searchDirectories() {
        guard newAgent.searchesTheMachine, let host = newAgent.machine else { return }
        listDirectories(on: host, matching: newAgent.query)
    }

    private func listDirectories(on host: HostId, matching query: String) {
        let asked = newAgent.asking()
        guard let profile else {
            newAgent.listed(.failure(RuntimeFailure("nothing is running")), asked: asked)
            return
        }
        Task {
            let listed = await profile.directories(
                on: host, matching: query, limit: NewAgentStore.limit)
            newAgent.listed(listed, asked: asked)
        }
    }

    @discardableResult
    public func startAgent() -> Bool {
        guard let request = newAgent.request, let profile else { return false }
        newAgent.starts()
        Task { newAgent.started(await profile.createAgent(request)) }
        return true
    }

    // MARK: - Agents

    public func rename(_ name: String, of agent: AgentKey) async -> Result<Nothing, RuntimeFailure> {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let profile else {
            return .failure(RuntimeFailure("a name cannot be empty"))
        }
        return await profile.act(.rename(trimmed), on: agent)
    }

    public func stop(_ agent: AgentKey) async -> Result<Nothing, RuntimeFailure> {
        guard let profile else { return .failure(RuntimeFailure("nothing is running")) }
        return await profile.act(.stop, on: agent)
    }

    /// Takes back a stop held while the agent's host could not be reached.
    public func cancelStop(_ agent: AgentKey) async -> Result<Nothing, RuntimeFailure> {
        guard let profile else { return .failure(RuntimeFailure("nothing is running")) }
        return await profile.act(.cancelStop, on: agent)
    }

    /// Deletes an agent. Its draft goes with it: the daemon forgets it, and
    /// nothing typed into its chat while the page leaves is kept.
    public func delete(_ agent: AgentKey) async -> Result<Nothing, RuntimeFailure> {
        let outcome = await act(.delete, on: agent)
        if case .success = outcome {
            models[agent]?.model.keeping = nil
        }
        return outcome
    }

    private func act(_ act: AgentAct, on agent: AgentKey) async -> Result<Nothing, RuntimeFailure> {
        if let acting { return await acting(act, agent) }
        guard let profile else { return .failure(RuntimeFailure("nothing is running")) }
        return await profile.act(act, on: agent)
    }

    // MARK: - Diagnostics

    /// Writes a dump of this account's profile and answers its directory.
    public func dump(reason: String) async -> Result<URL, RuntimeFailure> {
        guard let profile else { return .failure(RuntimeFailure("nothing is running")) }
        return await profile.dump(reason: reason).map { URL(fileURLWithPath: $0) }
    }
}
