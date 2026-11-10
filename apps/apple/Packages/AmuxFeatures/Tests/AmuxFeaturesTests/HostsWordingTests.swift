import AmuxCore
import Foundation
import XCTest
@testable import AmuxFeatures

/// What a Hosts row and a host's page say about a machine.
@MainActor
final class HostsWordingTests: XCTestCase {
    private let studio = HostId(UUID(uuidString: "00000000-0000-0000-0000-000000000601")!)

    private func host(
        reach: Reach = .online(.direct), addrs: [String] = [], platform: Platform? = .macos
    ) -> HostView {
        HostView(
            hostId: studio.bytes, name: "studio", local: false, trusted: true, candidate: false,
            reach: reach, addrs: addrs, relayOnly: false, pairingOpen: false, current: true, providers: [], claudeKind: nil, lastDialError: nil,
            platform: platform, signedIn: nil, through: nil, version: nil)
    }

    /// The tab once every machine's first dial has ended, unless it is
    /// `connecting`, as it is in the moments after the app comes forward.
    private func tab(
        _ hosts: [HostView], connecting: Bool = false,
        clock: @escaping @MainActor () -> Date = { Date() }
    ) -> (HostsTab, HostsStore) {
        let model = HostsStore(clock: clock)
        if !connecting { model.stopWaiting() }
        model.show(hosts)
        return (HostsTab(model: model, actions: { _ in }), model)
    }

    private func page(_ hosts: [HostView]) -> HostPage {
        let model = HostsStore()
        model.show(hosts)
        return HostPage(model: model, host: studio, actions: { _ in })
    }

    /// The row says what the machine is and the route this phone takes.
    func testALiveRowNamesItsRoute() {
        let direct = host()
        XCTAssertEqual(tab([direct]).0.status(direct), "macOS · direct")
        let relayed = host(reach: .online(.relay), platform: nil)
        XCTAssertEqual(tab([relayed]).0.status(relayed), "via relay")
        let away = host(reach: .away)
        XCTAssertEqual(tab([away]).0.status(away), "macOS · away on the free plan")
    }

    /// A host is away only when the relay alone sees it on the free plan,
    /// which relays nothing between hosts: every place that says it is away
    /// names the plan as the reason.
    func testAnAwayHostNamesTheFreePlan() {
        let away = host(reach: .away)
        XCTAssertEqual(HostPage.reach(away), "Away on the free plan")
        XCTAssertEqual(
            page([away]).meaning(away),
            "The relay can see it. The free plan does not relay between hosts, so reaching it needs a subscription.")
        XCTAssertEqual(
            ChatSubject(name: "a", host: "studio", directory: "~/src", reach: .away).place,
            "studio · away on the free plan")
    }

    /// An offline row says the one cause this phone knows, most telling first.
    func testAnOfflineRowSaysTheCauseThisPhoneKnows() {
        let revoked = host(reach: .offline(.revoked), addrs: ["10.0.0.2:7"])
        XCTAssertEqual(
            tab([revoked]).0.status(revoked), "macOS · no longer trusts this phone")
        let found = host(reach: .offline(.plain), addrs: ["10.0.0.2:7"])
        XCTAssertEqual(tab([found]).0.status(found), "macOS · found, not answering")
        let signedOut = host(reach: .offline(.signedOut))
        XCTAssertEqual(
            tab([signedOut]).0.status(signedOut),
            "macOS · offline, this phone is signed out")
        let gone = host(reach: .offline(.plain))
        XCTAssertEqual(tab([gone]).0.status(gone), "macOS · offline")
    }

    /// A machine not yet reached as the app comes forward is being dialled,
    /// not offline; one whose dial already failed is offline at once.
    func testAMachineStillBeingDialledIsConnecting() {
        let dialling = host(reach: .offline(.plain))
        XCTAssertEqual(tab([dialling], connecting: true).0.status(dialling), "macOS · connecting")
        var failed = host(reach: .offline(.plain))
        failed.lastDialError = "connection refused"
        XCTAssertEqual(tab([failed], connecting: true).0.status(failed), "macOS · offline")
        XCTAssertEqual(
            ChatSubject(name: "a", host: "studio", directory: "~/src", reach: .offline(.plain), connecting: true).place,
            ChatSubject(name: "a", host: "studio", directory: "~/src", reach: .online(.direct)).place)
    }

    /// Only a departure this phone watched has an age.
    func testAWatchedDepartureSaysHowLongAgo() {
        var now = Date(timeIntervalSince1970: 1_000)
        let (view, model) = tab([host()], clock: { now })
        let gone = host(reach: .offline(.plain))
        model.show([gone])
        now = Date(timeIntervalSince1970: 1_000 + 3 * 86_400)
        XCTAssertEqual(view.status(gone), "macOS · offline for 3d")
    }

    /// The page explains what a host's state means for its agents only where
    /// there is something to say.
    func testThePageSaysWhatTheStateMeans() {
        let direct = host()
        XCTAssertNil(page([direct]).meaning(direct))
        let relayed = host(reach: .online(.relay))
        XCTAssertNil(page([relayed]).meaning(relayed))
        let gone = host(reach: .offline(.plain))
        XCTAssertEqual(
            page([gone]).meaning(gone), "Its agents report their state as unknown until it is back.")
        let found = host(reach: .offline(.plain), addrs: ["10.0.0.2:7"])
        XCTAssertNotEqual(
            page([found]).meaning(found),
            "Its agents report their state as unknown until it is back.",
            "a host found here that no link reaches points at the network")
        XCTAssertNil(page([host(reach: .offline(.revoked))]).meaning(host(reach: .offline(.revoked))))
    }

    private func candidate(
        addrs: [String] = [], relayOnly: Bool = false, through: String? = nil, open: Bool = false
    ) -> HostView {
        HostView(
            hostId: studio.bytes, name: "nova", local: false, trusted: false, candidate: true,
            reach: .online(.unspecified), addrs: addrs, relayOnly: relayOnly, pairingOpen: open, current: false,
            providers: [], claudeKind: nil, lastDialError: nil, platform: .linux, signedIn: nil, through: through,
            version: nil)
    }

    /// A machine nobody paired is offered by how the phone reaches it, and
    /// only one the relay alone reaches on the free plan is sold a
    /// subscription in place of a pairing.
    func testAPairingCardSaysHowTheMachineIsReachedAndSellsOnlyWhatPairsIt() {
        // Nearby with no pairing window: no keypad, only where to start.
        let nearby = PairOffer(candidate(addrs: ["10.0.0.9:7"]), pro: false)
        XCTAssertEqual(nearby.title, "nova is nearby")
        XCTAssertEqual(nearby.detail, "Open amux on nova to pair")
        XCTAssertNil(nearby.button)
        XCTAssertTrue(nearby.waiting)
        XCTAssertFalse(nearby.subscribe)
        XCTAssertEqual(nearby.identifier("hosts", "nova"), "hosts.found.nova")

        // Nearby with its window open: a code typed now works.
        let open = PairOffer(candidate(addrs: ["10.0.0.9:7"], open: true), pro: false)
        XCTAssertEqual(open.title, "Pair with nova?")
        XCTAssertEqual(open.detail, "Enter the code amux shows there")
        XCTAssertEqual(open.button, "Pair")
        XCTAssertFalse(open.waiting)
        XCTAssertEqual(open.identifier("hosts", "nova"), "hosts.pair.nova")

        // Only its own advertisement says a window is open: one the relay
        // announces is offered the keypad as before.
        let announced = PairOffer(candidate(relayOnly: true, open: true), pro: true)
        XCTAssertEqual(announced.title, "nova is online")
        XCTAssertFalse(announced.waiting)

        let through = PairOffer(candidate(through: "studio"), pro: false)
        XCTAssertEqual(through.title, "nova is online")
        XCTAssertEqual(through.detail, "Pair it through studio")
        XCTAssertEqual(through.button, "Pair")
        XCTAssertFalse(through.subscribe, "a paired machine forwards on any plan")

        let free = PairOffer(candidate(relayOnly: true), pro: false)
        XCTAssertEqual(free.title, "nova is online")
        XCTAssertEqual(free.detail, "Pro pairs it from anywhere")
        XCTAssertEqual(free.button, "Subscribe")
        XCTAssertTrue(free.subscribe)
        XCTAssertEqual(free.identifier("home", "nova"), "home.subscribe.nova")

        let paid = PairOffer(candidate(relayOnly: true), pro: true)
        XCTAssertEqual(paid.button, "Pair")
        XCTAssertEqual(paid.detail, "Pair it from anywhere")
        XCTAssertFalse(paid.subscribe)

        let unheard = PairOffer(candidate(relayOnly: true), pro: nil)
        XCTAssertFalse(unheard.subscribe, "a plan not heard yet is no reason to sell one")
    }
}
