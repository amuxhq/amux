import AmuxCore
import XCTest

@testable import AmuxShell

/// Records what it was asked to load, and what the router had already done by
/// the time it was asked.
@MainActor
private final class RecordingLoader: RouteLoader {
    private(set) var loaded: [Route] = []
    /// The stack the router was showing at the moment each load was asked for.
    private(set) var pathWhenAsked: [[Route]] = []
    weak var router: Router?
    /// Work that has been started and has not finished, standing in for a
    /// fetch from a host.
    private(set) var outstanding = 0

    /// Every page the router said had been left, in the order it said so.
    private(set) var abandoned: [Route] = []

    func load(_ route: Route) {
        loaded.append(route)
        pathWhenAsked.append(router?.path ?? [])
        outstanding += 1
    }

    func left(_ route: Route) { abandoned.append(route) }
}

@MainActor
final class RouterTests: XCTestCase {
    private let machine = HostId(UUID(uuidString: "00000000-0000-0000-0000-00000000000A")!)

    private func agent() -> AgentKey { AgentKey(host: machine, agent: UUID()) }

    /// Leaving a page is said once, however it was left: by the app popping,
    /// by the system writing the stack back after a back gesture, or by one
    /// page replacing another.
    func testLeavingAPageIsSaidHoweverItWasLeft() {
        let loader = RecordingLoader()
        let router = Router(loader: loader)
        let first = agent()
        let second = agent()

        router.open(.conversation(first))
        router.pop()
        XCTAssertEqual(loader.abandoned, [.conversation(first)])

        // The system's own back: the stack is written back with the page gone.
        router.open(.conversation(second))
        router.setPath([], for: .agents)
        XCTAssertEqual(loader.abandoned, [.conversation(first), .conversation(second)])

        // And one conversation shown in place of another leaves the first.
        router.open(.conversation(first))
        router.show(.conversation(second))
        XCTAssertEqual(
            loader.abandoned,
            [.conversation(first), .conversation(second), .conversation(first)])

        // Nothing is left by opening, and a page still in the stack is not.
        router.open(.changes(second))
        XCTAssertEqual(loader.abandoned.count, 3)
    }

    func testOpeningPushesOnTheSameTurnOfTheRunLoop() {
        let router = Router()
        let agent = agent()

        // No `await` between the call and the assertion, and none is
        // available: `open` is not `async`. Navigation is what happened, not
        // what was started.
        router.open(.conversation(agent))

        XCTAssertEqual(router.path, [.conversation(agent)])
        XCTAssertEqual(router.top, .conversation(agent))
    }

    func testSwitchingConversationsLeavesOneToGoBackFrom() {
        let router = Router()
        let first = agent()
        let second = agent()
        router.open(.conversation(first))

        router.show(.conversation(second))

        // One conversation deep, not two: going back leads to the list both
        // were opened from, and never to the one that was left.
        XCTAssertEqual(router.path, [.conversation(second)])
        router.pop()
        XCTAssertEqual(router.path, [])
    }

    func testShowingFromARootPushes() {
        let router = Router()
        let agent = agent()

        // Nothing is on show to be replaced, so the page has to arrive the
        // ordinary way.
        router.show(.conversation(agent))

        XCTAssertEqual(router.path, [.conversation(agent)])
    }

    func testThePageIsUpBeforeAnythingIsAskedToLoadIt() {
        let loader = RecordingLoader()
        let router = Router(loader: loader)
        loader.router = router
        let agent = agent()

        router.open(.conversation(agent))

        XCTAssertEqual(loader.loaded, [.conversation(agent)])
        // What the loader saw: the page it is being asked to fill was already
        // on the stack. Loading first and pushing after would show the empty
        // stack here.
        XCTAssertEqual(loader.pathWhenAsked, [[.conversation(agent)]])
    }

    func testNavigationDoesNotWaitForTheLoadToFinish() {
        let loader = RecordingLoader()
        let router = Router(loader: loader)
        loader.router = router
        let first = agent()
        let second = agent()

        router.open(.conversation(first))
        router.open(.conversation(second))

        // Two loads started, neither finished, and both pages are on the
        // stack: nothing about where the app is depends on a host answering.
        XCTAssertEqual(loader.outstanding, 2)
        XCTAssertEqual(router.path, [.conversation(first), .conversation(second)])
    }

    func testOpeningARouteFromAnotherTabBringsItsTabWithIt() {
        let router = Router()
        XCTAssertEqual(router.tab, .agents)

        router.open(.host(machine))

        XCTAssertEqual(router.tab, .hosts)
        XCTAssertEqual(router.path, [.host(machine)])
        // The tab that was on show is where it was left.
        XCTAssertEqual(router.path(.agents), [])
    }

    func testEachTabKeepsItsOwnStack() {
        let router = Router()
        let agent = agent()

        router.open(.conversation(agent))
        router.open(.paywall(.you))

        XCTAssertEqual(router.tab, .you)
        XCTAssertEqual(router.path(.agents), [.conversation(agent)])
        XCTAssertEqual(router.path(.you), [.paywall(.you)])

        router.select(.agents)
        XCTAssertEqual(router.path, [.conversation(agent)])
    }

    func testTheSystemCanWriteBackWhatTheBackGestureDid() {
        let router = Router()
        let agent = agent()
        router.open(.conversation(agent))
        router.open(.changes(agent))

        router.setPath([.conversation(agent)], for: .agents)

        XCTAssertEqual(router.path, [.conversation(agent)])
    }

    func testPoppingAnEmptyStackIsNotAnError() {
        let router = Router()
        router.pop()
        XCTAssertEqual(router.path, [])
    }

    func testPopToRootClearsOnlyTheTabOnShow() {
        let router = Router()
        let agent = agent()
        router.open(.conversation(agent))
        router.open(.host(machine))

        router.popToRoot()

        XCTAssertEqual(router.path(.hosts), [])
        XCTAssertEqual(router.path(.agents), [.conversation(agent)])
    }

    /// A conversation has no navigation bar, so reaching for Agents while
    /// inside one is the way back to the list. Coming from another tab is not
    /// that: it finds the stack where it was left.
    func testReachingForTheTabYouAreOnGoesBackToTheTopOfIt() {
        let router = Router()
        let agent = agent()
        router.open(.conversation(agent))

        router.select(.agents)

        XCTAssertEqual(router.tab, .agents)
        XCTAssertEqual(router.path, [])
    }

    func testArrivingFromAnotherTabKeepsTheStackItLeftBehind() {
        let router = Router()
        let agent = agent()
        router.open(.conversation(agent))
        router.select(.you)

        router.select(.agents)

        XCTAssertEqual(router.path, [.conversation(agent)])
    }

    func testNewAgentIsASheetAndWhatItStartsOpensOverTheList() {
        let router = Router()
        router.open(.conversation(agent()))
        router.select(.hosts)
        router.composing = true
        XCTAssertEqual(router.tab, .hosts, "raised over the tab that asked for it")
        let started = agent()
        router.started(started)
        XCTAssertFalse(router.composing)
        XCTAssertEqual(router.tab, .agents)
        XCTAssertEqual(router.path, [.conversation(started)], "going back from it leads to the list")
    }

    func testLeavingAnAccountPutsNewAgentAway() {
        let router = Router()
        router.composing = true
        router.leaveAccount()
        XCTAssertFalse(router.composing)
    }

    /// A code is for a machine one account found; a link names its machine
    /// and is taken up by whichever account signs in for it.
    func testLeavingAnAccountEndsACodeButKeepsALink() {
        let router = Router()
        router.pairing = .code(machine)
        router.leaveAccount()
        XCTAssertNil(router.pairing)

        let invitation = PairingInvitation(
            host: machine, cloudURL: nil, addrs: [], secret: [1, 2, 3], payload: "{}")
        router.pairing = .link(invitation)
        router.leaveAccount()
        XCTAssertEqual(router.pairing, .link(invitation))
    }

    /// Signing in and subscribing are pages of the tab beneath, so the
    /// pairing sheet steps aside for them and comes back as it was.
    func testThePairingSheetStepsAsideForSigningIn() {
        let router = Router()
        router.pairing = .code(machine)
        XCTAssertTrue(router.showsPairing)
        router.open(.signIn(router.tab))
        XCTAssertFalse(router.showsPairing)
        router.pop()
        XCTAssertTrue(router.showsPairing)
        XCTAssertEqual(router.pairing, .code(machine))
    }

    func testEveryRouteBelongsToOneTab() {
        XCTAssertEqual(Route.conversation(agent()).tab, .agents)
        XCTAssertEqual(Route.changes(agent()).tab, .agents)
        XCTAssertEqual(Route.host(HostId(UUID())).tab, .hosts)
        XCTAssertEqual(Route.signIn(.you).tab, .you)
    }

    /// A different account on screen leaves none of the last one's pages on
    /// any stack, so a later tap or a return to the app never shows them;
    /// signing in and the list of accounts stay.
    func testLeavingAnAccountDropsItsPagesFromEveryStack() {
        let loader = RecordingLoader()
        let router = Router(loader: loader)
        let agent = agent()
        router.open(.conversation(agent))
        router.open(.changes(agent))
        router.open(.host(machine))
        router.open(.signIn(.you))

        router.leaveAccount()

        XCTAssertEqual(router.path(.agents), [])
        XCTAssertEqual(router.path(.hosts), [])
        XCTAssertEqual(router.path(.you), [.signIn(.you)])
        XCTAssertEqual(
            loader.abandoned, [.conversation(agent), .changes(agent), .host(machine)],
            "what the pages held open is let go")
    }
}
