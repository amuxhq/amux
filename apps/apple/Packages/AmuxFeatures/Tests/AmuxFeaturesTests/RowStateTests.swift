import AmuxCore
import Foundation
import XCTest
@testable import AmuxFeatures

/// What a fleet row says about itself.
///
/// Three facts decide it — whether this build can open the agent's kind,
/// whether the owning machine is reachable, and the agent's attention — and
/// the order they are read in is the whole of the design. These pin that
/// order, because an offline machine that stopped outranking a request would
/// hide the machine behind a request nobody can answer.
@MainActor
final class RowStateTests: XCTestCase {
    private let machine = HostId(UUID(uuidString: "00000000-0000-0000-0000-0000000000AA")!)

    private func row(
        _ attention: Attention, kind: Kind = .claudeSdk, reach: Reach = .online(.direct),
        exitCause: ExitCause? = nil
    ) -> AgentRow {
        AgentRow(
            row: FleetRow(
                card: FleetCard(
                    agent: AgentKey(host: machine, agent: UUID()), name: "refactor-auth",
                    kind: kind, attention: attention, cwd: "~/src/amux", phaseSinceMs: 0,
                    host: "Studio.local", hostReach: reach, children: 0,
                    familyAttention: attention, members: 1, descendants: 0, backgroundJobs: 0, branch: nil, exitCause: exitCause),
                depth: 0, expanded: false, secondLine: .blank, loud: nil),
            unread: false)
    }

    func testEachAttentionSpeaksInWords() {
        XCTAssertEqual(RowState(row: row(.working)), .working)
        XCTAssertEqual(RowState(row: row(.working)).word, "Working")
        XCTAssertEqual(RowState(row: row(.starting)).word, "Starting")
        XCTAssertEqual(RowState(row: row(.idle)).word, "Idle")
        XCTAssertEqual(RowState(row: row(.exited, exitCause: .finished)).word, "Finished")
        XCTAssertNil(RowState(row: row(.exited, exitCause: .finished)).elaboration)
        XCTAssertEqual(RowState(row: row(.exited, exitCause: .ended)).word, "Exited")
        XCTAssertEqual(RowState(row: row(.exited, exitCause: .failed("code 1"))).word, "Exited")
        XCTAssertNil(
            RowState(row: row(.exited, exitCause: .failed("code 1"))).elaboration,
            "why it exited is the second line's to say")
    }

    func testNeedingYouSaysSoAndIsTheOnlyMark() {
        let state = RowState(row: row(.needsYou))
        XCTAssertEqual(state, .needsYou)
        XCTAssertEqual(state.word, "Needs you")
        XCTAssertTrue(state.needsYou)
        XCTAssertEqual(state.spoken, "Needs you")
    }

    func testAKindThisBuildCannotOpenOutranksEverything() {
        let state = RowState(row: row(.needsYou, kind: .unspecified, reach: .offline))
        XCTAssertEqual(state, .unsupported)
        XCTAssertEqual(state.elaboration, "update amux to open it")
    }

    func testAnOfflineMachineOutranksARequest() {
        let state = RowState(row: row(.needsYou, reach: .offline))
        XCTAssertEqual(state, .hostOffline("Studio"))
        XCTAssertEqual(state.word, "Studio offline")
        XCTAssertTrue(state.namesTheHost)
        // A machine that no longer trusts this phone, or that a signed-out
        // phone cannot reach, has no route: offline here, never away.
        XCTAssertEqual(RowState(row: row(.working, reach: .away(.revoked))), .hostOffline("Studio"))
        XCTAssertEqual(RowState(row: row(.working, reach: .away(.signedOut))), .hostOffline("Studio"))
    }

    /// How an agent ended is history, whatever its machine is doing now: the
    /// order the runtime's second line has.
    func testAnEndingOutranksAnOfflineMachine() {
        XCTAssertEqual(
            RowState(row: row(.exited, reach: .offline, exitCause: .finished)), .exited(.finished))
    }

    func testAnAwayMachineIsNotLive() {
        let state = RowState(row: row(.working, reach: .away(.plain)))
        XCTAssertEqual(state, .hostAway("Studio"))
        XCTAssertEqual(state.elaboration, "not live")
        XCTAssertEqual(state.spoken, "Studio is away, not live")
    }

    func testAMachineReachedAnyWayLetsTheAgentSpeak() {
        XCTAssertEqual(RowState(row: row(.working, reach: .online(.direct))), .working)
        XCTAssertEqual(RowState(row: row(.working, reach: .online(.relay))), .working)
    }

    func testEveryStateHasADistinctName() {
        let names = [
            RowState.needsYou, .working, .starting, .hostOffline("a"), .hostAway("a"),
            .unsupported, .exited(.ended), .idle,
        ].map(\.name)
        XCTAssertEqual(Set(names).count, names.count)
    }
}
