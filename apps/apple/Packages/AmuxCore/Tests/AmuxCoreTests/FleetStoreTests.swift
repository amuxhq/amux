import Foundation
import XCTest

@testable import AmuxCore

/// Cards as the runtime hands them over, built by hand.
enum Cards {
    static let desk = HostId(UUID(uuidString: "00000000-0000-0000-0000-00000000de5c")!)
    static let now = Date(timeIntervalSince1970: 1_800_000_000)

    static func key(_ index: Int, on host: HostId = desk) -> AgentKey {
        AgentKey(host: host, agent: UUID(uuidString: String(
            format: "00000000-0000-0000-0000-%012d", index))!)
    }

    static func row(
        _ index: Int, _ name: String, attention: Attention = .idle, minutesAgo: Double = 1,
        depth: UInt32 = 0, children: UInt32 = 0, family: Attention? = nil,
        members: UInt32 = 1, expanded: Bool = false, loud: LoudMember? = nil,
        kind: Kind = .claudeSdk, reach: Reach = .online(.direct), host: HostId = desk
    ) -> FleetRow {
        FleetRow(
            card: FleetCard(
                agent: key(index, on: host), name: name, kind: kind, attention: attention,
                cwd: "/Users/pat/source/\(name)",
                phaseSinceMs: Int64(now.addingTimeInterval(-60 * minutesAgo)
                    .timeIntervalSince1970 * 1000),
                host: "desk", hostReach: reach, children: children,
                familyAttention: family ?? attention, members: members, descendants: 0, backgroundJobs: 0, stopHeld: false,
                branch: nil, exitCause: attention == .exited ? .ended : nil),
            depth: depth, expanded: expanded, secondLine: .blank, loud: loud)
    }

    /// The runtime's sections for rows listed heads first, each family's
    /// members after its head: a family sits in the section of its loudest
    /// member, in the order given. The fleet's counts are the rows' unless
    /// given, as they are where a folded family hides somebody.
    static func view(_ rows: [FleetRow], needYou: Int? = nil, working: Int? = nil) -> FleetView {
        var sections: [SectionKind: FleetSection] = [:]
        var kind = SectionKind.live
        for row in rows {
            if row.depth == 0 {
                kind = switch row.card.familyAttention {
                case .needsYou: .needsYou
                case .exited: .exited
                default: .live
                }
                sections[kind, default: section(kind)].families += 1
            }
            sections[kind, default: section(kind)].rows.append(row)
        }
        let counted = { (wanted: Set<Attention>) in
            UInt32(rows.filter { wanted.contains($0.card.attention) }.count)
        }
        return FleetView(
            sections: SectionKind.allCases.compactMap { sections[$0] },
            needYou: needYou.map(UInt32.init) ?? counted([.needsYou]),
            working: working.map(UInt32.init) ?? counted([.working, .starting]))
    }

    private static func section(_ kind: SectionKind) -> FleetSection {
        FleetSection(kind: kind, families: 0, foldedByDefault: kind == .exited, rows: [])
    }

    static func host(
        _ id: HostId = desk, name: String = "desk", reach: Reach = .online(.direct),
        trusted: Bool = true, candidate: Bool = false, local: Bool = false, addrs: [String] = [],
        relayOnly: Bool = false, through: String? = nil
    ) -> HostView {
        HostView(
            hostId: id.bytes, name: name, local: local, trusted: trusted, candidate: candidate,
            reach: reach, addrs: addrs, relayOnly: relayOnly, current: true, providers: [],
            lastDialError: nil, platform: nil, signedIn: nil, through: through, version: nil)
    }
}

@MainActor
final class FleetStoreTests: XCTestCase {
    func testRowsFollowTheRuntimesSectionsAndOrder() {
        let fleet = FleetStore(now: Cards.now)
        fleet.show(Cards.view([Cards.row(1, "one"), Cards.row(2, "two")]), hosts: [Cards.host()])
        XCTAssertEqual(fleet.sections.map(\.kind), [.live])
        XCTAssertEqual(fleet.rows.map(\.name), ["one", "two"])

        // Two asks for the person now: it moves to the section that says so,
        // which every client draws first.
        fleet.show(
            Cards.view([Cards.row(1, "one"), Cards.row(2, "two", attention: .needsYou)]),
            hosts: [Cards.host()])
        XCTAssertEqual(fleet.sections.map(\.kind), [.needsYou, .live])
        XCTAssertEqual(fleet.sections.map { $0.rows.map(\.name) }, [["two"], ["one"]])
        XCTAssertEqual(fleet.rows.map(\.name), ["two", "one"])
    }

    func testTheListsClockIsNeverOlderThanTheRowsItShows() {
        // Ages are read against the moment the list was last ordered. A row
        // whose state changed after that, as every row of a list left open
        // does, reads "0s" until the person pulls the list again: a phone left
        // on its Agents tab said "0s" for minutes while the terminal counted.
        let fleet = FleetStore(now: Cards.now)
        fleet.show(Cards.view([Cards.row(1, "pa", minutesAgo: -3)]), hosts: [Cards.host()])
        let row = fleet.rows[0]
        XCTAssertGreaterThanOrEqual(
            fleet.orderedAt, row.lastActivity,
            "ages are read against \(fleet.orderedAt), before the row's own change at \(row.lastActivity), so it reads \(row.age(at: fleet.orderedAt))")
    }

    func testExitedStartsFoldedAndOpensWhenAsked() {
        let fleet = FleetStore(now: Cards.now)
        fleet.show(
            Cards.view([Cards.row(1, "live"), Cards.row(2, "done", attention: .exited)]), hosts: [])
        XCTAssertEqual(fleet.sections.map(\.kind), [.live, .exited])
        XCTAssertEqual(fleet.sections.map(\.folded), [false, true], "as the runtime says")
        XCTAssertEqual(fleet.sections[1].families, 1)
        fleet.toggle(.exited)
        XCTAssertEqual(fleet.sections.map(\.folded), [false, false])
        fleet.show(
            Cards.view([Cards.row(1, "live"), Cards.row(2, "done", attention: .exited)]), hosts: [])
        XCTAssertFalse(fleet.sections[1].folded, "opened stays open as the fleet moves")
        fleet.toggle(.exited)
        XCTAssertTrue(fleet.sections[1].folded)
    }

    func testARowCarriesItsBranchAndSecondLine() {
        var row = Cards.row(1, "fixer")
        row.card.branch = "fix-auth"
        row.secondLine = .lastSaid("Done: the tests pass.")
        let fleet = FleetStore(now: Cards.now)
        fleet.show(Cards.view([row]), hosts: [])
        XCTAssertEqual(fleet.rows[0].branch, "fix-auth")
        XCTAssertEqual(fleet.rows[0].secondLine, .lastSaid("Done: the tests pass."))
    }

    func testAFamilysMembersFollowTheirHead() {
        let fleet = FleetStore(now: Cards.now)
        fleet.show(Cards.view([
            Cards.row(1, "parent", children: 2, family: .needsYou, expanded: true),
            Cards.row(2, "child", attention: .needsYou, depth: 1),
            Cards.row(3, "other"),
        ]), hosts: [])
        XCTAssertEqual(fleet.rows.map(\.name), ["parent", "child", "other"])
        XCTAssertEqual(fleet.rows.map(\.depth), [0, 1, 0])
        // Unfolded, the child speaks for itself.
        XCTAssertFalse(fleet.rows[0].speaksForAMember)

        // Folded, the head speaks for the member that needs the person.
        let loud = LoudMember(agent: Cards.key(2), name: "child", secondLine: .blank)
        fleet.show(Cards.view([
            Cards.row(1, "parent", children: 2, family: .needsYou, loud: loud),
            Cards.row(3, "other"),
        ], needYou: 1), hosts: [])
        XCTAssertTrue(fleet.rows[0].speaksForAMember)
    }

    func testActivitySinceTheLastOpeningIsUnread() {
        let fleet = FleetStore(now: Cards.now.addingTimeInterval(-3600))
        fleet.show(Cards.view([Cards.row(1, "one", minutesAgo: 1)]), hosts: [])
        XCTAssertTrue(fleet.rows[0].unread, "active since the launch")
        fleet.opened(Cards.key(1), at: Cards.now)
        XCTAssertFalse(fleet.rows[0].unread)
    }

    func testTheSubtitleCountsWhoNeedsYou() {
        let fleet = FleetStore(now: Cards.now)
        fleet.show(Cards.view([Cards.row(1, "a", attention: .working), Cards.row(2, "b")]), hosts: [])
        XCTAssertEqual(fleet.subtitle, "Nothing needs you · 1 working")
        // The counts are the runtime's, whatever the rows show.
        fleet.show(Cards.view([Cards.row(1, "a", attention: .starting)], working: 3), hosts: [])
        XCTAssertEqual(fleet.subtitle, "Nothing needs you · 3 working")
        XCTAssertEqual(fleet.working, 3)
        fleet.show(Cards.view([Cards.row(1, "a", attention: .needsYou), Cards.row(2, "b")]), hosts: [])
        XCTAssertEqual(fleet.subtitle, "1 need you · 2 agents")
    }

    func testAFoldedFamilyCountsTheMembersItHides() {
        let fleet = FleetStore(now: Cards.now)
        // Folded: the lead stands for itself and the helper that needs you.
        let folded = Cards.row(1, "lead", children: 1, family: .needsYou, members: 2)
        fleet.show(Cards.view([folded, Cards.row(3, "other")], needYou: 1), hosts: [])
        XCTAssertEqual(fleet.subtitle, "1 need you · 3 agents")
        XCTAssertEqual(fleet.needYou, 1)

        // Unfolded, the helper is its own row and nothing is counted twice.
        let open = Cards.row(1, "lead", children: 1, family: .needsYou, members: 2, expanded: true)
        fleet.show(
            Cards.view([open, Cards.row(2, "helper", attention: .needsYou, depth: 1), Cards.row(3, "other")]),
            hosts: [])
        XCTAssertEqual(fleet.subtitle, "1 need you · 3 agents")
    }

    func testAnOfflineHostIsTheExceptionAndTheRelayOutranksIt() {
        let fleet = FleetStore(now: Cards.now)
        fleet.show(Cards.view([Cards.row(1, "a")]), hosts: [Cards.host(reach: .offline(.plain))])
        XCTAssertEqual(fleet.exceptions, "desk offline")
        XCTAssertEqual(fleet.unreachableHost, "desk")
        fleet.relay(.signInAgain)
        XCTAssertEqual(fleet.exceptions, "Sign in again to reach your hosts through the relay")
        fleet.relay(.connecting)
        XCTAssertNil(fleet.relayTrouble, "a link coming up for the first time is not trouble")
        XCTAssertEqual(fleet.exceptions, "desk offline")
        fleet.relay(.retrying)
        XCTAssertEqual(fleet.exceptions, "Reconnecting to the relay")
    }

    func testAKindThisBuildCannotOpenIsListedButNotReadable() {
        let fleet = FleetStore(now: Cards.now)
        fleet.show(Cards.view([Cards.row(1, "a", kind: .unspecified)]), hosts: [])
        XCTAssertFalse(fleet.rows[0].readable)
    }
}
