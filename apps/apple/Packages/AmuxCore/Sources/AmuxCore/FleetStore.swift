import AmuxValues
import Foundation
import Observation

/// One agent as the home screen lists it: the runtime's card and where it
/// sits in its family.
extension FleetRow: Identifiable {
    public var id: AgentKey { card.agent }
    public var name: String { card.name.isEmpty ? "agent" : card.name }
    public var attention: Attention { card.attention }
    public var children: Int { Int(card.children) }
    public var hostId: HostId? { card.agent.hostId }
    public var hostName: String { card.host }
    /// How this phone reaches the agent's host.
    public var hostReach: Reach { card.hostReach }
    public var workingDirectory: String { card.cwd }
    /// The branch its folder is on, when it is on one.
    public var branch: String? { card.branch }
    /// When the agent's state last changed.
    public var lastActivity: Date { Date(milliseconds: card.phaseSinceMs) }
    /// A kind this build knows how to open.
    public var readable: Bool { card.kind != .unspecified }
    public var needsYou: Bool { attention == .needsYou }
    /// A folded family's head speaking for a member that needs the person.
    public var speaksForAMember: Bool { loud != nil }

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

/// The fleet as the home screen draws it: the runtime's sections, in the
/// runtime's order, which every client shares.
@MainActor
@Observable
public final class FleetStore {
    /// Every row on screen, families expanded where asked.
    public private(set) var rows: [FleetRow] = []
    /// Home's sections as the runtime groups them: a family sits in the
    /// section of its loudest member.
    public private(set) var sections: [FleetSection] = []
    /// Agents across the fleet that need the person, and that are working
    /// or starting, whatever is folded.
    public private(set) var needYou = 0
    public private(set) var working = 0
    /// Trusted hosts by id, this phone included.
    public private(set) var hosts: [HostId: HostView] = [:]
    /// The sections opened or folded against their default.
    public private(set) var flipped: Set<SectionKind> = []
    /// What the list is narrowed to, as the search field shows it; the
    /// owner hands it to the runtime.
    public var filter = FleetFilter(words: "", only: nil)
    /// The clock ages are read against: moved on by the screen while the
    /// list is on it, so ages count, and never behind the newest row it
    /// shows, so no age reads less than nothing.
    public private(set) var orderedAt: Date
    /// Whether the runtime has said what it lists at all: until then an empty
    /// list is not yet read, not a phone with nothing paired.
    public private(set) var loaded = false
    /// The relay link, as the account screens report it.
    public private(set) var relay: RelayLink = .off

    @ObservationIgnored private var markedFirstFrame = false
    /// The launch's first frame with rows is built and not yet on screen.
    @ObservationIgnored public private(set) var presenting = false
    /// Told once that frame is on screen.
    @ObservationIgnored public var presented: (@MainActor () -> Void)?

    public init(now: Date = Date()) {
        orderedAt = now
    }

    /// What the runtime lists now.
    public func show(_ view: FleetView, hosts views: [HostView]) {
        loaded = true
        hosts = Dictionary(
            views.filter(\.trusted).compactMap { view in view.id.map { ($0, view) } },
            uniquingKeysWith: { _, last in last })
        sections = view.sections
        rows = sections.flatMap(\.rows)
        needYou = Int(view.needYou)
        working = Int(view.working)
        if let newest = rows.map(\.lastActivity).max(), newest > orderedAt { orderedAt = newest }
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

    /// The clock moved on: ages are read from now. Never backwards, so a
    /// row changed on a host whose clock runs ahead reads 0s, not less.
    public func refreshOrder(now: Date) {
        if now > orderedAt { orderedAt = now }
    }

    /// Opens a folded section, or folds an open one.
    public func toggle(_ section: SectionKind) {
        if flipped.contains(section) {
            flipped.remove(section)
        } else {
            flipped.insert(section)
        }
    }

    /// Whether a section is folded: its default, unless opened or folded
    /// against it.
    public func folded(_ section: FleetSection) -> Bool {
        section.foldedByDefault != flipped.contains(section.kind)
    }

    public func host(_ id: HostId?) -> HostView? {
        id.flatMap { hosts[$0] }
    }

    public func row(_ agent: AgentKey) -> FleetRow? {
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
    /// What the person must do for the relay link, when something will
    /// only come right that way. A link coming up or coming back is not
    /// that: the rows' rings say a machine is out of reach meanwhile.
    public var relayTrouble: String? {
        switch relay {
        case .signInAgain: return "Sign in again to reach your hosts through the relay"
        case .versionMismatch: return "Update amux to reach your hosts through the relay"
        case .off, .connecting, .retrying, .connected: return nil
        }
    }

    /// A host the relay would reach if this account paid for it.
    public var awayHost: String? {
        machines.filter { $0.group == .away }.map(\.name).sorted().first
    }

    public var unreachableHost: String? {
        machines.filter { !$0.online }.map(\.name).sorted().first
    }
}
