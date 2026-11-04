import AmuxValues
import Foundation
import Observation

/// Starting an agent: which host, where on it, what it is called, which
/// agent it is and what that agent runs with. One screen; every choice is
/// held here and reset each time the screen opens. The models, efforts,
/// permissions and modes offered are what the chosen host says its
/// provider offers; nothing chosen means the host's own default.
@MainActor
@Observable
public final class NewAgentStore {
    public enum Provider: String, Sendable, Equatable, CaseIterable, Identifiable {
        case claude
        case codex

        public var id: String { rawValue }

        /// The provider as the runtime names it.
        public var wire: AmuxValues.Provider {
            switch self {
            case .claude: .claude
            case .codex: .codex
            }
        }

        public var title: String {
            switch self {
            case .claude: "Claude"
            case .codex: "Codex"
            }
        }

        /// The provider an agent of this kind runs, if this phone can start
        /// one.
        public init?(_ kind: Kind) {
            switch kind {
            case .claudePty, .claudeSdk: self = .claude
            case .codex: self = .codex
            case .unspecified: return nil
            }
        }

        /// The kind an agent of this provider starts as on a host whose
        /// settings resolve Claude to `claude`: the terminal or the SDK, as
        /// that host's person chose, so an agent started from the phone is
        /// the same kind as one started at the host. A host that has not
        /// said gets the SDK, which carries every row the phone draws.
        public func kind(claude: Kind?) -> Kind {
            switch self {
            case .claude:
                switch claude {
                case .claudePty: .claudePty
                case .claudeSdk, .codex, .unspecified, nil: .claudeSdk
                }
            case .codex: .codex
            }
        }
    }

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
    /// The folders the host's person chose as projects, in their order.
    public private(set) var projects: [Directory] = []
    /// Where agents on the host worked lately, newest first.
    public private(set) var recent: [Directory] = []
    public private(set) var capped = false
    /// Which Claude the chosen host starts, as its settings resolve it; nil
    /// until it has said.
    public private(set) var claudeKind: Kind?
    public private(set) var directory = ""
    public var query = ""
    public var typed = ""
    public var browsing = false
    public private(set) var provider = Provider.claude
    /// What the chosen host offers, per provider, once asked.
    public private(set) var offers: [Provider: Offer] = [:]
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

    /// Names already taken on each host, so a suggestion is new there.
    @ObservationIgnored private var taken: [HostId: Set<String>] = [:]
    /// Where the fleet's agents on each host work, newest first: what the
    /// chooser offers when the host lists nothing of its own.
    @ObservationIgnored private var worked: [HostId: [Directory]] = [:]
    @ObservationIgnored private var asked = 0
    /// Catalogue asks are numbered apart from directory listings, so asking
    /// both providers after a listing leaves the listing's answer current.
    @ObservationIgnored private var offerAsks = 0
    @ObservationIgnored private var offerAsked: [Provider: Int] = [:]
    /// Whether the chosen machine could be reached when last told.
    @ObservationIgnored private var machineOnline: Bool?
    /// Whether the person has picked the agent since the screen opened, so
    /// the fleet arriving does not change it under them.
    @ObservationIgnored private var providerPicked = false

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
        projects = []
        capped = false
        claudeKind = nil
        directory = ""
        query = ""
        typed = ""
        typedName = nil
        browsing = false
        provider = .claude
        providerPicked = false
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
        projects = []
        capped = false
        claudeKind = nil
        directory = ""
        query = ""
        failure = nil
        asked += 1
        offers = [:]
        offerAsked = [:]
        startAfresh()
    }

    /// Which Claude the chosen host says a new agent runs. Told whenever the
    /// host list moves, since a host says it once its link is up.
    public func machineStarts(claude kind: Kind?) {
        claudeKind = kind
    }

    /// What the fleet lists, for names and directories per host, and the
    /// agent to start with: the kind most recently at work, since with
    /// several to choose from that is the one most often wanted again.
    public func remember(_ rows: [FleetRow]) {
        if !providerPicked, let latest = rows.filter({ Provider($0.card.kind) != nil })
            .max(by: { $0.lastActivity < $1.lastActivity }), let usual = Provider(latest.card.kind) {
            provider = usual
        }
        var names: [HostId: Set<String>] = [:]
        var places: [HostId: [Directory]] = [:]
        for row in rows.sorted(by: { $0.lastActivity > $1.lastActivity }) {
            guard let host = row.hostId else { continue }
            names[host, default: []].insert(row.name)
            let path = row.workingDirectory
            guard !path.isEmpty, !(places[host] ?? []).contains(where: { $0.path == path })
            else { continue }
            let name = path.split(separator: "/").last.map(String.init) ?? path
            places[host, default: []].append(Directory(path: path, name: name))
        }
        taken = names
        worked = places
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
        // Where an agent worked last, project or not, is what is prefilled.
        var last = fallback.first
        switch result {
        case .success(let directories):
            projects = directories.projects
            let lately = directories.recent.isEmpty ? fallback : directories.recent
            last = lately.first
            // A folder that is a project is listed once, among the projects.
            let chosen = Set(projects.map(\.path))
            recent = lately.filter { !chosen.contains($0.path) }
            capped = UInt32(directories.recent.count + directories.projects.count)
                >= Self.limit
            listing = .ready
        case .failure:
            recent = fallback
            projects = []
            capped = false
            listing = .unavailable
        }
        if directory.isEmpty, let first = last ?? projects.first {
            directory = first.path
        }
    }

    public func choose(directory path: String) {
        directory = path
        failure = nil
    }

    /// Another agent starts its model, effort, permission and mode afresh:
    /// they do not carry across providers.
    public func choose(provider chosen: Provider) {
        providerPicked = true
        guard provider != chosen else { return }
        provider = chosen
        startAfresh()
        failure = nil
    }

    private func startAfresh() {
        chosen = NewAgentChoices(effort: nil, mode: nil, model: nil, permission: nil)
    }

    // MARK: - What the host offers

    /// Marks a provider's catalogue as asked for on the chosen host and
    /// answers the ask's number.
    public func askingOffer(for provider: Provider) -> Int {
        offerAsks += 1
        offerAsked[provider] = offerAsks
        offers[provider] = .asking
        return offerAsks
    }

    public func offered(
        _ result: Result<Catalogue, RuntimeFailure>, for provider: Provider, asked number: Int
    ) {
        guard offerAsked[provider] == number else { return }
        switch result {
        case .success(let catalogue): offers[provider] = .offered(catalogue)
        case .failure: offers[provider] = .unavailable
        }
    }

    /// The chosen provider's catalogue on the chosen host, once it said.
    public var catalogue: Catalogue? {
        if case .offered(let catalogue)? = offers[provider] { return catalogue }
        return nil
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
        let offered = catalogue ?? Catalogue(models: [], commands: [], permissions: [], modes: [], skills: [])
        if let next = Bridge.newAgentPick(offered, chosen, pick) { chosen = next }
    }

    /// Whether a search is typed: the chooser then lists what matches it
    /// in place of its projects and recent folders.
    public var searching: Bool {
        !query.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// What the chooser shows for the search typed so far: the projects and
    /// recent folders that match it, projects first.
    public var found: [Directory] {
        let wanted = query.trimmingCharacters(in: .whitespaces).lowercased()
        guard !wanted.isEmpty else { return [] }
        return (projects + recent).filter {
            $0.name.lowercased().contains(wanted) || $0.path.lowercased().contains(wanted)
        }
    }

    /// Where an agent is most likely wanted, for the chips under the chosen
    /// directory: where agents worked lately, then the projects.
    public var likely: [Directory] {
        recent + projects
    }

    /// Whether a search has to ask the host, because the list it gave was cut.
    public var searchesTheMachine: Bool {
        capped && !query.trimmingCharacters(in: .whitespaces).isEmpty
    }

    public var typedPath: String {
        typed.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    public var name: String { typedName ?? suggestedName }

    /// The directory's own name, made new on its host, or "agent".
    public var suggestedName: String {
        let trimmed = directory.hasSuffix("/") ? String(directory.dropLast()) : directory
        let last = trimmed.split(separator: "/").last.map(String.init)
        let base = last.flatMap { $0.isEmpty ? nil : $0 } ?? "agent"
        let names = machine.flatMap { taken[$0] } ?? []
        guard names.contains(base) else { return base }
        var number = 2
        while names.contains("\(base)-\(number)") { number += 1 }
        return "\(base)-\(number)"
    }

    public func choose(name written: String) {
        typedName = written
        failure = nil
    }

    public var chosenName: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }

    public var ready: Bool {
        machine != nil && !directory.isEmpty && !chosenName.isEmpty && !starting
    }

    /// What starting asks the host for.
    public var request: NewAgent? {
        guard ready, let machine else { return nil }
        return NewAgent(
            hostId: machine.bytes, kind: provider.kind(claude: claudeKind), cwd: directory,
            name: chosenName,
            effort: effort, mode: provider == .codex ? mode : nil, model: model,
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
            if let machine { taken[machine, default: []].insert(chosenName) }
        case .failure(let refusal):
            failure = refusal.description
        }
    }
}

extension Directory: Identifiable {
    public var id: String { path }
}
