import AmuxCore
import Foundation
import XCTest
@testable import AmuxFeatures

/// What a Hosts row and a group caption say about a machine.
@MainActor
final class HostsWordingTests: XCTestCase {
    private let studio = HostId(UUID(uuidString: "00000000-0000-0000-0000-000000000601")!)

    private func host(
        reach: Reach = .online(.direct), addrs: [String] = [], platform: String? = "Mac Studio"
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

    /// The group has said where the machine is; the row says the route.
    func testALiveRowNamesItsRoute() {
        let direct = host()
        XCTAssertEqual(tab([direct]).0.status(direct, direct.group), "Mac Studio · direct")
        let relayed = host(reach: .online(.relay), platform: nil)
        XCTAssertEqual(tab([relayed]).0.status(relayed, relayed.group), "via relay")
        let away = host(reach: .away)
        XCTAssertEqual(tab([away]).0.status(away, away.group), "Mac Studio · away on the free plan")
    }

    /// A host is away only when the relay alone sees it on the free plan,
    /// which relays nothing between hosts: every place that says it is away
    /// names the plan as the reason.
    func testAnAwayHostNamesTheFreePlan() {
        let away = host(reach: .away)
        XCTAssertEqual(HostPage.reach(away), "Away · the free plan does not relay between hosts")
        XCTAssertEqual(
            ChatSubject(name: "a", host: "studio", directory: "~/src", reach: .away).place,
            "studio · away on the free plan")
    }

    /// An offline row says the one cause this phone knows, most telling first.
    func testAnOfflineRowSaysTheCauseThisPhoneKnows() {
        let revoked = host(reach: .offline(.revoked), addrs: ["10.0.0.2:7"])
        XCTAssertEqual(
            tab([revoked]).0.status(revoked, revoked.group), "Mac Studio · no longer trusts this phone")
        let found = host(reach: .offline(.plain), addrs: ["10.0.0.2:7"])
        XCTAssertEqual(tab([found]).0.status(found, found.group), "Mac Studio · found, not answering")
        let signedOut = host(reach: .offline(.signedOut))
        XCTAssertEqual(
            tab([signedOut]).0.status(signedOut, signedOut.group),
            "Mac Studio · offline, this phone is signed out")
        let gone = host(reach: .offline(.plain))
        XCTAssertEqual(tab([gone]).0.status(gone, gone.group), "Mac Studio · offline")
    }

    /// Only a departure this phone watched has an age.
    func testAWatchedDepartureSaysHowLongAgo() {
        var now = Date(timeIntervalSince1970: 1_000)
        let (view, model) = tab([host()], clock: { now })
        let gone = host(reach: .offline(.plain))
        model.show([gone])
        now = Date(timeIntervalSince1970: 1_000 + 3 * 86_400)
        XCTAssertEqual(view.status(gone, gone.group), "Mac Studio · offline for 3d")
    }

    /// A caption appears only where there is something to say.
    func testCaptionsSayWhatIsTrueOfTheGroup() {
        let view = tab([host()]).0
        XCTAssertEqual(
            view.caption(.onThisNetwork, offers: true),
            "Run amux pair on one of these and enter the code it prints.")
        XCTAssertNil(view.caption(.onThisNetwork, offers: false))
        XCTAssertNil(view.caption(.throughTheRelay, offers: false))
        XCTAssertEqual(
            view.caption(.away, offers: false),
            "The relay can see these. The free plan does not relay between hosts, so reaching them needs a subscription.")
        XCTAssertEqual(
            view.caption(.offline, offers: false),
            "Agents on an offline host report their state as unknown.")
        let found = tab([host(reach: .offline(.plain), addrs: ["10.0.0.2:7"])]).0
        XCTAssertNotEqual(
            found.caption(.offline, offers: false),
            "Agents on an offline host report their state as unknown.",
            "a host found here that no link reaches points at the network")
    }
}
