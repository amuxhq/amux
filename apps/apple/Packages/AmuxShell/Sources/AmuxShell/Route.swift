import AmuxCore
import Foundation

/// The three places the app is, as the tab bar names them.
public enum Tab: String, Hashable, Sendable, CaseIterable, Codable {
    case agents
    case hosts
    case you

    public var title: String {
        switch self {
        case .agents: "Agents"
        case .hosts: "Hosts"
        case .you: "You"
        }
    }

    public var symbol: String {
        switch self {
        case .agents: "square.stack.3d.up"
        case .hosts: "externaldrive.connected.to.line.below"
        case .you: "person.crop.circle"
        }
    }
}

/// Somewhere the app can be pushed to, and everything needed to draw it there.
///
/// A route carries what it takes to show the page and nothing that has to be
/// fetched first: an agent's page is its identity, and what the agent has said
/// arrives afterwards. That is what lets a tap push on the frame it happened
/// in rather than after a round trip to the host.
public enum Route: Hashable, Sendable {
    case conversation(AgentKey)
    /// The changes one agent has made, opened from its conversation.
    case changes(AgentKey)
    case newAgent
    /// Typing the six-digit code one machine printed. It carries that machine,
    /// because a code proves possession of one machine's offer and is
    /// authenticated against it alone. There is no route without one: a
    /// machine this phone has not found is paired by scanning its code.
    case pairByCode(HostId)
    /// A pairing invitation that arrived as a link, waiting to be confirmed or
    /// abandoned. Arriving here pairs with nobody.
    case pairConfirmation(PairingInvitation)
    case host(HostId)
    /// Signing in, pushed onto whichever stack asked for it. The empty Agents
    /// home and the You tab both offer it, so where it was opened from is a
    /// fact about the push rather than about the screen — and it is what the
    /// back button is allowed to name.
    case signIn(Tab)
    /// Subscribing, pushed onto whichever stack asked for it, for the same
    /// reason signing in is.
    case paywall(Tab)

    /// Which tab this page belongs under. A route opened from elsewhere takes
    /// its tab with it, so the tab bar always agrees with what is on screen.
    public var tab: Tab {
        switch self {
        case .conversation, .changes, .newAgent: .agents
        case .pairByCode, .pairConfirmation, .host: .hosts
        case .signIn(let from), .paywall(let from): from
        }
    }

    /// Whether the page stays when the account on screen changes: it shows no
    /// one account's machines or agents.
    public var outlivesAccount: Bool {
        switch self {
        case .signIn: true
        case .conversation, .changes, .newAgent, .pairByCode, .pairConfirmation, .host, .paywall:
            false
        }
    }

    /// What this page is called where a page is named by a word: a trace of
    /// what somebody was looking at, a journey's assertion, a report.
    public var name: String {
        switch self {
        case .conversation: "conversation"
        case .changes: "changes"
        case .newAgent: "new-agent"
        case .pairByCode: "pin"
        case .pairConfirmation: "pair-confirm"
        case .host: "host"
        case .signIn: "sign-in"
        case .paywall: "paywall"
        }
    }
}
