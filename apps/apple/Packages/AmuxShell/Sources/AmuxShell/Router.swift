import AmuxCore
import Foundation
import Observation

/// What the shell asks for once a page has been pushed.
///
/// It is called after the push and its work is never awaited before one. A
/// navigation that waited for a fetch would leave the tap looking dropped for
/// as long as the host took to answer, and would make the app's
/// responsiveness a property of the network. The page goes up first; what
/// belongs on it loads into place.
@MainActor
public protocol RouteLoader: AnyObject {
    func load(_ route: Route)
    /// A page that is no longer anywhere in its tab's stack. Whatever it held
    /// open while somebody was reading it is let go here.
    func left(_ route: Route)
}

/// Where the app is, and the only thing that changes it.
///
/// Screens do not navigate. They say what happened and this decides where that
/// leads, so the same screen can be pushed, presented or captured without
/// carrying a route with it.
@MainActor
@Observable
public final class Router {
    /// The place on show. Each keeps its own stack, so coming back to one
    /// finds it where it was left.
    public var tab: Tab = .agents {
        didSet {
            if tab != .you { beneath = tab }
            arrived?()
        }
    }

    /// The tab the tab bar shows: the one on show, or, while the settings
    /// sheet is up, the one it was raised over and that putting it away
    /// returns to.
    public private(set) var beneath: Tab = .agents

    /// Told after every arrival: a tab reached for, a page pushed, a page
    /// gone back from.
    ///
    /// Nothing in the shipping app sets this. It exists so a build with the
    /// reporting tools in it can keep a recording of where somebody has been,
    /// which is what a report of a screen is replayed from — and it is a
    /// closure rather than a recorder this file knows about, because the
    /// recording is a debug build's and navigation must not link it.
    @ObservationIgnored public var arrived: (@MainActor () -> Void)?

    /// One stack per tab, written to by the shell's navigation as well as
    /// read: the back gesture and the tab bar are the system's to drive, and
    /// what they did has to land somewhere the app can see.
    ///
    /// Leaving is noticed on the stacks themselves rather than at any one call
    /// that changes them: the system pops these directly — a back gesture, and
    /// reaching for the tab already on show — so a page that holds something
    /// open while somebody is reading it would otherwise go on holding it
    /// whenever the app was not the one doing the navigating.
    public var agentsPath: [Route] = [] { didSet { departed(oldValue, agentsPath) } }
    public var hostsPath: [Route] = [] { didSet { departed(oldValue, hostsPath) } }
    public var youPath: [Route] = [] { didSet { departed(oldValue, youPath) } }

    /// Whether New Agent is up: a sheet over whichever tab asked for it, as
    /// a new message is in a chat app, rather than a page in a stack. Putting
    /// it away leaves everything beneath it where it was.
    public var composing = false { didSet { arrived?() } }

    /// A link that is not navigation, kept until whatever it answers comes
    /// for it. A sign-in callback can arrive before the sign-in that started
    /// it is listening — during a cold start the system hands the link over
    /// before the first frame — and dropping it would strand the sign-in.
    public private(set) var held: DeepLink?

    private weak var loader: (any RouteLoader)?

    public init(loader: (any RouteLoader)? = nil) {
        self.loader = loader
    }

    /// Points the router at what loads a page. Set here rather than at
    /// construction when the thing that loads pages needs the router itself.
    public func loads(with loader: any RouteLoader) {
        self.loader = loader
    }

    public func path(_ tab: Tab) -> [Route] { self[keyPath: Self.stack(of: tab)] }

    /// The stack on show.
    public var path: [Route] { path(tab) }

    /// The page on show, or nothing when a tab is showing its own root.
    public var top: Route? { path.last }

    /// Goes to a tab, or — when it is the tab already on show — back to the
    /// top of it.
    ///
    /// That second half is the platform's own convention and it is load-bearing
    /// here: a conversation has no navigation bar, so reaching for Agents while
    /// already inside one is how a person gets back to the list.
    public func select(_ tab: Tab) {
        if tab == self.tab {
            popToRoot()
        } else {
            self.tab = tab
        }
    }

    /// Pushes a page. Synchronous, and deliberately not `async`: the frame the
    /// tap happened on is the frame the page appears on.
    public func open(_ route: Route) {
        if route.tab != tab { tab = route.tab }
        self[keyPath: Self.stack(of: route.tab)].append(route)
        loader?.load(route)
    }

    /// Shows a page in place of the one on show, for a move that is sideways
    /// rather than deeper.
    ///
    /// Switching between two conversations is the case: they are peers, so
    /// going back from the second has to lead where going back from the first
    /// would have. Pushing instead would build a trail of every agent that had
    /// been looked at, and going back would walk it.
    public func show(_ route: Route) {
        guard route.tab == tab, let last = path.indices.last else { return open(route) }
        var replacing = path
        replacing[last] = route
        setPath(replacing, for: tab)
        loader?.load(route)
    }

    /// Replaces a tab's stack. The back gesture and the tab bar write through
    /// here, which is why the stack is settable at all.
    public func setPath(_ routes: [Route], for tab: Tab) {
        self[keyPath: Self.stack(of: tab)] = routes
    }

    /// Tells whoever loads pages about every page that is no longer in the
    /// stack it was in.
    private func departed(_ before: [Route], _ now: [Route]) {
        for route in before where !now.contains(route) { loader?.left(route) }
        arrived?()
    }

    /// Empties every stack of the pages about the account that just left the
    /// screen: its conversations, machines, pairings and subscription. Signing
    /// in is about the account arriving and the list of accounts about none,
    /// so those stay.
    public func leaveAccount() {
        // New Agent offers one account's machines.
        composing = false
        for tab in Tab.allCases {
            setPath(path(tab).filter(\.outlivesAccount), for: tab)
        }
    }

    /// Puts New Agent away and opens the agent it started, straight over
    /// the list: going back from it leads to the list it joined.
    public func started(_ agent: AgentKey) {
        composing = false
        setPath([], for: .agents)
        open(.conversation(agent))
    }

    public func pop() {
        guard !path.isEmpty else { return }
        setPath(Array(path.dropLast()), for: tab)
    }

    public func popToRoot() {
        setPath([], for: tab)
    }

    private static func stack(of tab: Tab) -> ReferenceWritableKeyPath<Router, [Route]> {
        switch tab {
        case .agents: \.agentsPath
        case .hosts: \.hostsPath
        case .you: \.youPath
        }
    }

    /// Takes a link the app was opened with.
    ///
    /// A pairing link becomes a confirmation page and pairs with nobody: the
    /// machine on the other end is named and its fingerprint shown, and trust
    /// is committed only when the person says so. A sign-in callback is not
    /// navigation; it is held for the sign-in that started it to collect.
    @discardableResult
    public func open(_ url: URL) -> DeepLink? {
        guard let link = DeepLink(url) else { return nil }
        switch link {
        case .pair(let invitation):
            open(.pairConfirmation(invitation))
        case .signInCallback:
            held = link
        }
        return link
    }

    /// Takes the held link, if it is the kind asked for, and forgets it. A link
    /// is acted on once.
    public func takeHeld(_ isWanted: (DeepLink) -> Bool = { _ in true }) -> DeepLink? {
        guard let held, isWanted(held) else { return nil }
        self.held = nil
        return held
    }
}
