import AmuxCore
import Foundation

/// The three places the app is: the two tabs, and You, the settings sheet
/// raised over them from the account disc on the Agents list.
public enum Tab: String, Hashable, Sendable, CaseIterable, Codable {
    case agents
    case hosts
    case you

    public var title: String {
        switch self {
        case .agents: "Agents"
        case .hosts: "Hosts"
        case .you: "Settings"
        }
    }

    public var symbol: String {
        switch self {
        case .agents: "terminal"
        case .hosts: "server.rack"
        case .you: "gearshape"
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
    case host(HostId)
    /// Subscribing, pushed onto whichever stack asked for it. The Agents list
    /// and the settings sheet both offer it, so where it was opened from is a
    /// fact about the push rather than about the screen — and it is what the
    /// back button is allowed to name.
    case paywall(Tab)

    /// Which tab this page belongs under. A route opened from elsewhere takes
    /// its tab with it, so the tab bar always agrees with what is on screen.
    public var tab: Tab {
        switch self {
        case .conversation, .changes: .agents
        case .host: .hosts
        case .paywall(let from): from
        }
    }

    /// What this page is called where a page is named by a word: a trace of
    /// what somebody was looking at, a journey's assertion, a report.
    public var name: String {
        switch self {
        case .conversation: "conversation"
        case .changes: "changes"
        case .host: "host"
        case .paywall: "paywall"
        }
    }
}

/// Pairing with one machine, raised as a sheet over whichever tab asked.
public enum Pairing: Hashable, Sendable, Identifiable {
    /// Typing the six-digit code one machine printed. It carries that machine,
    /// because a code proves possession of one machine's offer and is
    /// authenticated against it alone. There is no pairing by code without
    /// one: a machine this phone has not found is paired by scanning its code.
    case code(HostId)
    /// A pairing invitation that arrived as a link, waiting to be confirmed or
    /// abandoned. Arriving pairs with nobody.
    case link(PairingInvitation)

    public var id: Self { self }

    /// What the sheet is called where a screen is named by a word.
    public var name: String {
        switch self {
        case .code: "pin"
        case .link: "pair-confirm"
        }
    }
}
