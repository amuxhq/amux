import AmuxMacScreens
import AmuxValues
import XCTest

/// The changes pane is the open agent's own folder. A provider subagent has
/// none, so opening one from a parent whose changes are open leaves the
/// pane shut, and going back finds the parent's changes still open.
@MainActor
final class SubagentChanges: XCTestCase {
    private let parent = AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: 1, count: 16))
    private let subagent = AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: 2, count: 16))

    func testOpeningASubagentLeavesTheChangesShutUntilItsParentIsBack() {
        var place = Place(agent: parent)
        place.changes = true
        XCTAssertTrue(content.showsChanges(place), "the parent's changes are open")

        place.agent = subagent
        XCTAssertFalse(content.showsChanges(place), "a provider subagent has no changes to show")
        XCTAssertTrue(place.changes, "the choice stands for the next agent with a folder")

        place.goBack(where: { _ in true })
        XCTAssertEqual(place.agent, parent)
        XCTAssertTrue(content.showsChanges(place), "back on the parent, its changes are open again")
    }

    /// A parent on this Mac and the provider subagent it started.
    private var content: MainContent {
        let rows = [row(parent, "lead", native: false), row(subagent, "survey part 1", native: true)]
        return MainContent(
            profiles: [],
            fleet: FleetContent(sections: [FleetSection(kind: .live, families: 1, foldedByDefault: false, rows: rows)]))
    }

    private func row(_ agent: AgentKey, _ name: String, native: Bool) -> FleetRow {
        FleetRow(
            card: FleetCard(
                agent: agent, name: name, handle: name, kind: .claudeSdk, attention: .working, cwd: "/src/amux",
                phaseSinceMs: 0, host: "this Mac", hostReach: .online(.direct), children: native ? 0 : 1,
                familyAttention: .working, members: native ? 1 : 2, descendants: native ? 0 : 1,
                backgroundJobs: 0, closeHeld: false, filing: .open, topLevel: !native, native: native,
                branch: nil, exitCause: nil, repository: nil, workspaceGoes: nil),
            depth: native ? 1 : 0, expanded: true, secondLine: .blank, loud: nil)
    }
}
