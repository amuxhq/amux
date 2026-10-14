import Foundation
import XCTest

@testable import AmuxCore

@MainActor
final class HostsStoreTests: XCTestCase {
    private let studio = HostId(UUID(uuidString: "00000000-0000-0000-0000-000000000501")!)
    private let mini = HostId(UUID(uuidString: "00000000-0000-0000-0000-000000000502")!)
    private let found = HostId(UUID(uuidString: "00000000-0000-0000-0000-000000000503")!)
    private let phone = HostId(UUID(uuidString: "00000000-0000-0000-0000-000000000504")!)

    func testHostsAreSplitIntoThisPhoneTrustedAndFound() {
        let hosts = HostsStore()
        hosts.show([
            Cards.host(phone, name: "phone", local: true),
            Cards.host(studio, name: "studio"),
            Cards.host(found, name: "found", trusted: false, candidate: true, addrs: ["10.0.0.9:7"]),
        ])
        XCTAssertEqual(hosts.local?.name, "phone")
        XCTAssertEqual(hosts.hosts.map(\.name), ["studio"])
        XCTAssertEqual(hosts.discovered.map(\.name), ["found"])
        XCTAssertEqual(hosts.candidates(.onThisNetwork).map(\.name), ["found"])
        XCTAssertEqual(hosts.known(found)?.addrs, ["10.0.0.9:7"])
    }

    func testGroupsFollowTheRouteAndTheReach() {
        XCTAssertEqual(Cards.host(reach: .online(.direct)).group, .onThisNetwork)
        XCTAssertEqual(Cards.host(reach: .online(.relay)).group, .throughTheRelay)
        XCTAssertEqual(Cards.host(reach: .online(.ssh)).group, .throughTheRelay)
        XCTAssertEqual(Cards.host(reach: .away).group, .away)
        XCTAssertEqual(Cards.host(reach: .offline(.plain)).group, .offline)
        // Only a host the relay sees is away here: one that no longer trusts
        // this phone, or that only a signed-out phone misses, has no route.
        XCTAssertEqual(Cards.host(reach: .offline(.revoked)).group, .offline)
        XCTAssertEqual(Cards.host(reach: .offline(.signedOut)).group, .offline)
        XCTAssertTrue(Cards.host(reach: .online(.direct)).online)
        XCTAssertFalse(Cards.host(reach: .away).online)
    }

    func testOnlineHostsListFirstThenByName() {
        let hosts = HostsStore()
        hosts.show([
            Cards.host(studio, name: "Zed", reach: .offline(.plain)),
            Cards.host(mini, name: "alpha", reach: .offline(.plain)),
            Cards.host(found, name: "mid"),
        ])
        XCTAssertEqual(hosts.hosts.map(\.name), ["mid", "alpha", "Zed"])
    }

    func testOnlyADepartureThisPhoneWatchedHasATime() {
        var clock = Date(timeIntervalSince1970: 1_000)
        let hosts = HostsStore(clock: { clock })
        hosts.show([Cards.host(studio, reach: .offline(.plain))])
        XCTAssertNil(hosts.wentOffline(studio), "already gone when first seen")
        hosts.show([Cards.host(studio)])
        clock = Date(timeIntervalSince1970: 2_000)
        hosts.show([Cards.host(studio, reach: .offline(.plain))])
        XCTAssertEqual(hosts.wentOffline(studio), clock)
        hosts.show([Cards.host(studio)])
        XCTAssertNil(hosts.wentOffline(studio))
    }

    func testTheRosterAndThePermissionAreHeld() {
        let hosts = HostsStore()
        let roster = Roster(
            identity: Identity(hostId: phone.bytes, name: "phone", fingerprint: "ab"),
            peers: [PairedPeer(hostId: studio.bytes, name: "studio", fingerprint: "cd")])
        hosts.show(roster)
        hosts.sawLocalNetwork(.denied)
        XCTAssertEqual(hosts.devices.map(\.name), ["studio"])
        XCTAssertEqual(hosts.localNetwork, .denied)
    }
}
