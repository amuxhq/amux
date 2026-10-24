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
            reach: reach, addrs: addrs, current: true, providers: [], lastDialError: nil,
            platform: platform, signedIn: nil, version: nil)
    }

    private func tab(_ hosts: [HostView], clock: @escaping @MainActor () -> Date = { Date() })
        -> (HostsTab, HostsStore)
    {
        let model = HostsStore(clock: clock)
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
}
