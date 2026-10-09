import AmuxValues
import Foundation
import Observation

/// One agent as the home screen lists it: the runtime's card, where it sits
/// in its family, and whether this phone has seen its latest activity.
public struct AgentRow: Sendable, Equatable, Identifiable {
    public var row: FleetRow
    public var unread: Bool

    public init(row: FleetRow, unread: Bool) {
        self.row = row
        self.unread = unread
    }

    public var card: FleetCard { row.card }
    public var id: AgentKey { card.agent }
    public var name: String { card.name.isEmpty ? "agent" : card.name }
    public var attention: Attention { card.attention }
    /// Zero for a family's head.
    public var depth: Int { Int(row.depth) }
    public var children: Int { Int(card.children) }
    public var expanded: Bool { row.expanded }
    public var hostId: HostId? { card.agent.hostId }
    public var hostName: String { card.host }
    /// How this phone reaches the agent's host.
    public var hostReach: Reach { card.hostReach }
    public var workingDirectory: String { card.cwd }
    /// The branch its folder is on, when it is on one.
    public var branch: String? { card.branch }
    /// What its state calls for, under its name.
    public var secondLine: SecondLine { row.secondLine }
    /// When the agent's state last changed.
    public var lastActivity: Date { Date(milliseconds: card.phaseSinceMs) }
    /// A kind this build knows how to open.
    public var readable: Bool { card.kind != .unspecified }
    public var needsYou: Bool { attention == .needsYou }
    /// A folded family's head speaking for a member that needs the person.
    public var speaksForAMember: Bool { row.loud != nil }

    public func age(at now: Date) -> String {
        Elapsed.spelled(max(0, now.timeIntervalSince(lastActivity)))
    }
}

extension Date {
    public init(milliseconds: Int64) {
        self.init(timeIntervalSince1970: TimeInterval(milliseconds) / 1000)
    }
}

/// A duration as a row has room for it.
public enum Elapsed {
    public static func spelled(_ seconds: TimeInterval) -> String {
        switch seconds {
        case ..<60: "\(Int(seconds))s"
        case ..<3600: "\(Int(seconds / 60))m"
        case ..<86_400: "\(Int(seconds / 3600))h"
        default: "\(Int(seconds / 86_400))d"
        }
    }
}

/// One of home's sections as the runtime groups them: a family sits in
/// the section of its loudest member.
public struct HomeSection: Sendable, Equatable, Identifiable {
    public var kind: SectionKind
    /// How many families the section holds.
    public var families: Int
    public var rows: [AgentRow]
    public var folded: Bool
    public var id: String { kind.rawValue }

    public init(kind: SectionKind, families: Int, rows: [AgentRow], folded: Bool) {
        self.kind = kind
        self.families = families
        self.rows = rows
        self.folded = folded
    }
}

/// The fleet as the home screen draws it: the runtime's sections, in the
/// runtime's order, which every client shares.
@MainActor
@Observable
public final class FleetStore {
    /// Every row on screen, families expanded where asked.
    public private(set) var rows: [AgentRow] = []
    public private(set) var sections: [HomeSection] = []
    /// Agents across the fleet that need the person, and that are working
    /// or starting, whatever is folded.
    public private(set) var needYou = 0
    public private(set) var working = 0
    /// Trusted hosts by id, this phone included.
    public private(set) var hosts: [HostId: HostView] = [:]
    /// The family heads whose members are listed under them.
    public private(set) var expanded: Set<[UInt8]> = []
    /// The sections opened or folded against their default.
    public private(set) var flipped: Set<SectionKind> = []
    /// When the list was last pulled or shown again; ages are read against
    /// it so a list at rest does not tick.
    public private(set) var orderedAt: Date
    /// The relay link, as the account screens report it.
    public private(set) var relay: RelayLink = .off

    @ObservationIgnored private var view = FleetView(sections: [], needYou: 0, working: 0)
    @ObservationIgnored private var seen: [AgentKey: Date] = [:]
    @ObservationIgnored private let launched: Date
    @ObservationIgnored private var markedFirstFrame = false
    /// The launch's first frame with rows is built and not yet on screen.
    @ObservationIgnored public private(set) var presenting = false
    /// Told once that frame is on screen.
    @ObservationIgnored public var presented: (@MainActor () -> Void)?

    public init(now: Date = Date()) {
        orderedAt = now
        launched = now
    }

    /// What the runtime lists now.
    public func show(_ view: FleetView, hosts views: [HostView]) {
        self.view = view
        hosts = Dictionary(
            views.filter(\.trusted).compactMap { view in view.id.map { ($0, view) } },
            uniquingKeysWith: { _, last in last })
        rebuild()
        if !markedFirstFrame && !self.rows.isEmpty {
            markedFirstFrame = true
            presenting = true
            Presentation.after { [weak self] in
                Signposts.emit(.firstCachedFrame)
                MainActor.assumeIsolated { self?.finishPresenting() }
            }
        }
    }

    /// The first frame is on screen, or waiting for it was given up.
    public func finishPresenting() {
        guard presenting else { return }
        presenting = false
        presented?()
    }

    public func relay(_ link: RelayLink) {
        relay = link
    }

    /// The list was pulled or shown again: ages are read from now.
    public func refreshOrder(now: Date) {
        orderedAt = now
    }

    /// Opens a folded section, or folds an open one.
    public func toggle(_ section: SectionKind) {
        if flipped.contains(section) {
            flipped.remove(section)
        } else {
            flipped.insert(section)
        }
        rebuild()
    }

    /// Somebody opened this agent's chat.
    public func opened(_ agent: AgentKey, at time: Date = Date()) {
        seen[agent] = time
        rebuild()
    }

    /// Lists or folds away a family's members; the owner reads the rows
    /// again with the new set.
    public func toggle(_ head: AgentKey) {
        if expanded.contains(head.agent) {
            expanded.remove(head.agent)
        } else {
            expanded.insert(head.agent)
        }
    }

    public func host(_ id: HostId?) -> HostView? {
        id.flatMap { hosts[$0] }
    }

    public func row(_ agent: AgentKey) -> AgentRow? {
        rows.first { $0.id == agent }
    }

    /// Hosts other than this phone.
    public var machines: [HostView] {
        hosts.values.filter { !$0.local }
    }

    public var subtitle: String {
        guard needYou > 0 else { return "Nothing needs you · \(working) working" }
        // A folded family stands for every member under it.
        let agents = rows.reduce(0) { $0 + ($1.expanded ? 1 : Int($1.card.members)) }
        return "\(needYou) need you · \(agents) agent\(agents == 1 ? "" : "s")"
    }

    /// One line for what is wrong with reaching the fleet, if anything.
    public var exceptions: String? {
        if let relayTrouble { return relayTrouble }
        let offline = machines.filter { !$0.online }.map(\.name).sorted()
        switch offline.count {
        case 0: return nil
        case 1: return "\(offline[0]) offline"
        default: return "\(offline.count) hosts offline"
        }
    }

    /// What is wrong with the relay link, when something is. A link still
    /// coming up for the first time is not trouble; one that went down is.
    public var relayTrouble: String? {
        switch relay {
        case .signInAgain: return "Sign in again to reach your hosts through the relay"
        case .versionMismatch: return "Update amux to reach your hosts through the relay"
        case .retrying: return "Reconnecting to the relay"
        case .off, .connecting, .connected: return nil
        }
    }

    /// A host the relay would reach if this account paid for it.
    public var awayHost: String? {
        machines.filter { $0.group == .away }.map(\.name).sorted().first
    }

    public var unreachableHost: String? {
        machines.filter { !$0.online }.map(\.name).sorted().first
    }

    private func isUnread(_ row: FleetRow) -> Bool {
        let activity = Date(milliseconds: row.card.phaseSinceMs)
        return activity > (seen[row.card.agent] ?? launched)
    }

    private func rebuild() {
        sections = view.sections.map { section in
            HomeSection(
                kind: section.kind, families: Int(section.families),
                rows: section.rows.map { AgentRow(row: $0, unread: isUnread($0)) },
                folded: section.foldedByDefault != flipped.contains(section.kind))
        }
        rows = sections.flatMap(\.rows)
        needYou = Int(view.needYou)
        working = Int(view.working)
    }
}
