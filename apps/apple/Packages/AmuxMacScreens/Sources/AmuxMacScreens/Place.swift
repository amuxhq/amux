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
        case .agents: "bubble.left.and.bubble.right"
        case .hosts: "desktopcomputer"
        }
    }
}

/// What the right-hand pane shows about the open agent.
public enum InspectorTab: String, CaseIterable, Identifiable, Sendable {
    case changes
    case family

    public var id: String { rawValue }

    var title: String {
        switch self {
        case .changes: "Changes"
        case .family: "Family"
        }
    }
}

/// Where the window is: the surface, what is open on it, and which panes
/// show. One value, so the storybook can put the window anywhere and the
/// app can remember it.
public struct Place: Equatable, Sendable {
    public var surface: Surface
    /// The agent open in the centre.
    public var agent: AgentKey?
    /// The sidebar lists the settled agents rather than the rest.
    public var settled: Bool
    /// The host open on the hosts surface, by its id's bytes.
    public var host: [UInt8]?
    public var sidebar: Bool
    /// The right-hand pane, nil while it is closed.
    public var inspector: InspectorTab?

    public init(
        surface: Surface = .agents, agent: AgentKey? = nil, settled: Bool = false, host: [UInt8]? = nil,
        sidebar: Bool = true, inspector: InspectorTab? = nil
    ) {
        self.surface = surface
        self.agent = agent
        self.settled = settled
        self.host = host
        self.sidebar = sidebar
        self.inspector = inspector
    }
}
