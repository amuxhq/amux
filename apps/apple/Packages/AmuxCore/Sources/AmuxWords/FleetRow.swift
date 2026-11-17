import AmuxValues
import Foundation

/// One agent as the home screen lists it: the runtime's card and where it
/// sits in its family.
extension FleetRow: Identifiable {
    public var id: AgentKey { card.agent }
    public var name: String { card.name.isEmpty ? "agent" : card.name }
    public var attention: Attention { card.attention }
    public var children: Int { Int(card.children) }
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
    /// How the person filed it, apart from its lifecycle.
    public var filing: Filing { card.filing }
    public var pinned: Bool { card.filing == .pinned }
    public var closed: Bool { card.filing == .closed }
    /// It has no parent, so it may be pinned.
    public var pinnable: Bool { card.topLevel && !card.native && !closed }
    /// A subagent its parent's provider runs: it goes with its parent and
    /// has no folder or terminal, so it is never pinned, deleted, resumed or
    /// opened in a terminal on its own.
    public var native: Bool { card.native }
    /// Settled, and not a provider subagent, which goes with its parent.
    public var deletable: Bool { closed && !card.native }
    /// A folded family's head speaking for a member that needs the person.
    public var speaksForAMember: Bool { loud != nil }

    public func age(at now: Date) -> String {
        Elapsed.spelled(max(0, now.timeIntervalSince(lastActivity)))
    }
}
