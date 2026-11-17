import AmuxValues
import Foundation

/// A part of the app the rail switches between. Each has its own sidebar
/// and its own centre; the window around them stays put.
public enum Surface: String, CaseIterable, Identifiable, Sendable {
    case agents
    case hosts

    public var id: String { rawValue }

    var title: String {
        switch self {
        case .agents: "Agents"
        case .hosts: "Hosts"
        }
    }

    var symbol: String {
        switch self {
        case .agents: "messages-square"
        case .hosts: "server"
        }
    }
}

/// Where the window is: the surface, what is open on it, and which panes
/// show. One value, so the storybook can put the window anywhere and the
/// app can remember it.
public struct Place: Equatable, Sendable {
    public var surface: Surface
    /// The agent open in the centre; none opens the new-agent page. Each
    /// change is remembered, so the window can go back and forward through
    /// the agents opened, as a browser does through pages.
    public var agent: AgentKey? {
        didSet {
            guard agent != oldValue, !travelling else { return }
            if let oldValue { back = Array((back + [oldValue]).suffix(50)) }
            forward = []
        }
    }
    /// The sidebar lists the settled agents rather than the rest.
    public var settled: Bool
    /// The host open on the hosts surface, by its id's bytes.
    public var host: [UInt8]?
    public var sidebar: Bool
    /// The right-hand pane with the open agent's changes.
    public var changes: Bool

    private var back: [AgentKey] = []
    private var forward: [AgentKey] = []
    private var travelling = false

    public init(
        surface: Surface = .agents, agent: AgentKey? = nil, settled: Bool = false, host: [UInt8]? = nil,
        sidebar: Bool = true, changes: Bool = false
    ) {
        self.surface = surface
        self.agent = agent
        self.settled = settled
        self.host = host
        self.sidebar = sidebar
        self.changes = changes
    }

    /// Whether the changes pane shows, for an open agent that is a
    /// provider subagent (`native`) or not. A provider subagent has no
    /// folder of its own, so on one the pane stays shut while the choice
    /// stands: opening a subagent from its parent's chat and coming back
    /// finds the parent's changes still open.
    public func showsChanges(native: Bool) -> Bool {
        changes && surface == .agents && agent != nil && !native
    }

    public var canGoBack: Bool { !back.isEmpty }
    public var canGoForward: Bool { !forward.isEmpty }

    /// Opens the agent opened before this one, passing over any that
    /// `exists` no longer finds.
    public mutating func goBack(where exists: (AgentKey) -> Bool) {
        while let to = back.popLast() {
            guard exists(to) else { continue }
            if let agent { forward.append(agent) }
            travel(to)
            return
        }
    }

    public mutating func goForward(where exists: (AgentKey) -> Bool) {
        while let to = forward.popLast() {
            guard exists(to) else { continue }
            if let agent { back.append(agent) }
            travel(to)
            return
        }
    }

    private mutating func travel(_ to: AgentKey) {
        travelling = true
        surface = .agents
        agent = to
        travelling = false
    }
}
