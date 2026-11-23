import AmuxValues
import Foundation
import Observation

/// Starting an agent: which host, where on it, what it is called, which
/// agent it is and what that agent runs with. One screen; every choice is
/// held here and reset each time the screen opens. The agents offered are
/// the ones the chosen host lists, named as it names them, a person's own
/// among them; the models, efforts, permissions and modes offered are what
/// the host says the chosen one offers. Nothing chosen means the host's own
/// default, else the agent's.
@MainActor
@Observable
public final class NewAgentStore {
    /// What the chosen host said a provider offers.
    public enum Offer: Equatable, Sendable {
        case asking
        case offered(Catalogue)
        /// The host could not say; the agent starts on the host's defaults.
        case unavailable
    }

    public enum Listing: Equatable, Sendable {
        case none
        case asking
        case ready
        /// The host could not be asked; only the fleet's own directories and
        /// a typed path are offered.
        case unavailable
    }

    /// How many directories a host is asked for; a list that comes back
    /// this long may have more, so a search asks the host again.
    public static let limit: UInt32 = 200

    public private(set) var machine: HostId?
    public private(set) var listing = Listing.none
    /// The folders the host's person chose as repositories, in their order.
    public private(set) var repositories: [Directory] = []
    /// Where agents on the host worked lately, newest first.
    public private(set) var recent: [Directory] = []
    /// Where Claude Code and Codex ran on the host lately, outside amux.
    public private(set) var history: [Directory] = []
    public private(set) var capped = false
    /// The agents the chosen host offers, in its order; empty until it has
    /// said.
    public private(set) var agents: [HostProvider] = []
    public private(set) var directory = ""
    public var query = ""
    public var typed = ""
    public var browsing = false
    /// The chosen agent's id; nil while the host lists none.
    public private(set) var provider: String?
    /// What the chosen host offers, per agent id, once asked.
    public private(set) var offers: [String: Offer] = [:]
    /// The chosen model, effort, permission and mode, by their values in
    /// the host's catalogue; nil leaves each to the host.
    public private(set) var chosen = NewAgentChoices(effort: nil, mode: nil, model: nil, permission: nil)
    public var model: String? { chosen.model }
    public var effort: String? { chosen.effort }
    public var permission: String? { chosen.permission }
    public var mode: String? { chosen.mode }
    /// Start it in a worktree of its own, made from the folder's repository.
    public var newWorktree = false
    public private(set) var typedName: String?
    public private(set) var starting = false
    public private(set) var failure: String?
    /// The agent the host started, once it confirms.
    public private(set) var created: AgentKey?

    /// Where the fleet's agents on each host work, newest first: what the
    /// chooser offers when the host lists nothing of its own.
    @ObservationIgnored private var worked: [HostId: [Directory]] = [:]
    @ObservationIgnored private var asked = 0
    /// Catalogue asks are numbered apart from directory listings, so asking
    /// both providers after a listing leaves the listing's answer current.
    @ObservationIgnored private var offerAsks = 0
    @ObservationIgnored private var offerAsked: [String: Int] = [:]
    /// Whether the chosen machine could be reached when last told.
    @ObservationIgnored private var machineOnline: Bool?
    /// Whether the person has picked the agent since the screen opened, so
    /// the fleet arriving does not change it under them.
    @ObservationIgnored private var providerPicked = false
    /// The agent most recently at work in the fleet: the one to start with
    /// wherever the host offers it.
    @ObservationIgnored private var usual: String?

    public init() {}

    /// The chosen machine's reach as the host list says it now. A refusal
    /// given while the machine could not be reached goes once it can be:
    /// what it said is no longer so, and starting again is the next step.
    public func machineReached(_ online: Bool) {
        defer { machineOnline = online }
        if online, machineOnline == false, failure != nil { failure = nil }
    }

    public func open(on host: HostId?) {
        machine = host
        machineOnline = nil
        listing = .none
        recent = []
        repositories = []
        history = []
        capped = false
        agents = []
        directory = ""
        query = ""
        typed = ""
        typedName = nil
        browsing = false
        provider = nil
        providerPicked = false
        usual = nil
        offers = [:]
        offerAsked = [:]
        startAfresh()
        newWorktree = false
        starting = false
        failure = nil
        created = nil
        asked += 1
    }

    /// Chooses a host. Everything learned about the last one goes.
    public func point(at host: HostId) {
        guard machine != host else { return }
        machine = host
        machineOnline = nil
        listing = .none
        recent = []
        repositories = []
        history = []
        capped = false
        agents = []
        if !providerPicked { provider = nil }
        directory = ""
        query = ""
        failure = nil
        asked += 1
        offers = [:]
        offerAsked = [:]
        startAfresh()
    }

    /// The agents the chosen host offers. Told whenever the host list moves,
    /// since a host lists each agent once it has asked it. Until the person
    /// picks one, the agent is the one most recently at work, where the
    /// host offers it, else the host's first; a pick the host stops
    /// offering gives way the same way.
    public func machineOffers(_ offered: [HostProvider]) {
        agents = offered
        let ids = offered.map(\.provider)
        let offering = { (id: String?) in id.flatMap { ids.contains($0) ? $0 : nil } }
        let wanted = providerPicked
            ? offering(provider) ?? offering(usual) ?? ids.first
            : offering(usual) ?? offering(provider) ?? ids.first
        guard wanted != provider else { return }
        provider = wanted
        startAfresh()
    }

    /// What the fleet lists, for names and directories per host, and the
    /// agent to start with: the one most recently at work, since with
    /// several to choose from that is the one most often wanted again.
    public func remember(_ rows: [FleetRow]) {
        if let latest = rows.filter(\.readable).max(by: { $0.lastActivity < $1.lastActivity }) {
            usual = latest.card.provider
            if !providerPicked, agents.contains(where: { $0.provider == latest.card.provider }),
                provider != latest.card.provider
            {
                provider = latest.card.provider
                startAfresh()
            }
        }
        var places: [HostId: [Directory]] = [:]
        for row in rows.sorted(by: { $0.lastActivity > $1.lastActivity }) {
            guard let host = row.hostId else { continue }
            let path = row.workingDirectory
            guard !path.isEmpty, !(places[host] ?? []).contains(where: { $0.path == path })
            else { continue }
            let name = path.split(separator: "/").last.map(String.init) ?? path
            places[host, default: []].append(Directory(path: path, name: name, repository: nil))
        }
        worked = places
    }

    /// Numbers a listing that keeps what is shown until it answers.
    public func askingQuietly() -> Int {
        asked += 1
        return asked
    }

    /// Marks a listing under way and answers its number.
    public func asking() -> Int {
        asked += 1
        listing = .asking
        return asked
    }

    public func listed(_ result: Result<Directories, RuntimeFailure>, asked number: Int) {
        guard number == asked else { return }
        let fallback = machine.flatMap { worked[$0] } ?? []
        // Where an agent worked last, repository or not, is what is prefilled.
        var last = fallback.first
        switch result {
        case .success(let directories):
            repositories = directories.repositories
            let lately = directories.recent.isEmpty ? fallback : directories.recent
            last = lately.first
            // A folder that is a repository is listed once, among the repositories.
            let chosen = Set(repositories.map(\.path))
            recent = lately.filter { !chosen.contains($0.path) }
            let shown = chosen.union(recent.map(\.path))
            history = (directories.history ?? []).filter { !shown.contains($0.path) }
            capped = UInt32(
                directories.recent.count + directories.repositories.count
                    + (directories.history?.count ?? 0)) >= Self.limit
            listing = .ready
        case .failure:
            recent = fallback
            repositories = []
            history = []
            capped = false
            listing = .unavailable
        }
        if directory.isEmpty, let first = last ?? repositories.first {
            directory = first.path
        }
    }

    /// Whether the host keeps the folder among its repositories.
    public func isPinned(_ path: String) -> Bool {
        repositories.contains { $0.path == path }
    }

    /// Moves a folder in or out of the repositories at once, ahead of the
    /// host's entry saying so, which then lists it where the host puts it.
    public func pinned(_ directory: Directory, _ on: Bool) {
        let path = directory.path
        if on {
            recent.removeAll { $0.path == path }
            history.removeAll { $0.path == path }
            if !isPinned(path) { repositories.append(directory) }
        } else {
            repositories.removeAll { $0.path == path }
            if !recent.contains(where: { $0.path == path }) { recent.insert(directory, at: 0) }
        }
    }

    /// The host refused a pin: says why, until the next change.
    public func pinRefused(_ why: String) {
        failure = why
    }

    public func choose(directory path: String) {
        directory = path
        failure = nil
    }

    /// Another agent starts its model, effort, permission and mode afresh:
    /// they do not carry across agents.
    public func choose(provider chosen: String) {
        providerPicked = true
        guard provider != chosen else { return }
        provider = chosen
        startAfresh()
        failure = nil
    }

    /// The chosen agent as the host lists it.
    public var chosenAgent: HostProvider? {
        agents.first { $0.provider == provider }
    }

    private func startAfresh() {
        chosen = NewAgentChoices(effort: nil, mode: nil, model: nil, permission: nil)
    }

    // MARK: - What the host offers

    /// Marks a provider's catalogue as asked for on the chosen host and
    /// answers the ask's number.
    public func askingOffer(for provider: String) -> Int {
        offerAsks += 1
        offerAsked[provider] = offerAsks
        offers[provider] = .asking
        return offerAsks
    }

    public func offered(
        _ result: Result<Catalogue, RuntimeFailure>, for provider: String, asked number: Int
    ) {
        guard offerAsked[provider] == number else { return }
        switch result {
        case .success(let catalogue): offers[provider] = .offered(catalogue)
        case .failure: offers[provider] = .unavailable
        }
    }

    /// The chosen provider's catalogue on the chosen host, once it said.
    public var catalogue: Catalogue? {
        if let provider, case .offered(let catalogue)? = offers[provider] { return catalogue }
        return nil
    }

    /// What the host said the chosen agent offers; nil before it was asked.
    public var offer: Offer? {
        provider.flatMap { offers[$0] }
    }

    /// The agents the host lists that have not been asked what they offer
    /// since the host was chosen.
    public var unasked: [String] {
        agents.map(\.provider).filter { offerAsked[$0] == nil }
    }

    /// What the chosen provider offers on the chosen host, with each choice
    /// marked, as the shared settings view builds it; nil until the host
    /// says.
    public var settings: SettingsView? {
        guard let catalogue else { return nil }
        return Bridge.newAgentSettings(catalogue, chosen)
    }

    /// Takes a pick. The shared view applies it, with the rules it carries:
    /// another model starts at its own default effort, and a permission it
    /// does not take gives way to the normal one.
    public func choose(_ pick: NewAgentPick) {
        let offered = catalogue ?? Catalogue(models: [], commands: [], permissions: [], modes: [], skills: [], newAgent: nil)
        if let next = Bridge.newAgentPick(offered, chosen, pick) { chosen = next }
    }

    /// Whether a search is typed: the chooser then lists what matches it
    /// in place of its repositories and recent folders.
    public var searching: Bool {
        !query.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// What the chooser shows for the search typed so far: the repositories and
    /// recent folders that match it, repositories first.
    public var found: [Directory] {
        let wanted = query.trimmingCharacters(in: .whitespaces).lowercased()
        guard !wanted.isEmpty else { return [] }
        return (repositories + recent + history).filter {
            $0.name.lowercased().contains(wanted) || $0.path.lowercased().contains(wanted)
        }
    }

    /// Where an agent is most likely wanted, for the chips under the chosen
    /// directory: where agents worked lately, then the repositories.
    public var likely: [Directory] {
        recent + repositories
    }

    /// Whether a search has to ask the host, because the list it gave was cut.
    public var searchesTheMachine: Bool {
        capped && !query.trimmingCharacters(in: .whitespaces).isEmpty
    }

    public var typedPath: String {
        typed.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// What the person typed. Left empty, the host names the agent and its
    /// provider titles it.
    public var name: String { typedName ?? "" }

    public func choose(name written: String) {
        typedName = written
        failure = nil
    }

    public var chosenName: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }

    public var ready: Bool {
        machine != nil && provider != nil && !directory.isEmpty && !starting
    }

    /// What starting asks the host for.
    public var request: NewAgent? {
        guard ready, let machine, let provider else { return nil }
        // Only what the person picked from what the agent offers: anything
        // else is the host's remembered pick or the agent's own default.
        return NewAgent(
            hostId: machine.bytes, provider: provider, cwd: directory,
            name: chosenName,
            effort: effort, mode: mode, model: model,
            newWorktree: newWorktree ? true : nil, permission: permission)
    }

    public func starts() {
        starting = true
        failure = nil
    }

    public func started(_ result: Result<AgentKey, RuntimeFailure>) {
        starting = false
        switch result {
        case .success(let agent):
            created = agent
        case .failure(let refusal):
            failure = refusal.description
        }
    }
}

extension Directory: Identifiable {
    public var id: String { path }
}
