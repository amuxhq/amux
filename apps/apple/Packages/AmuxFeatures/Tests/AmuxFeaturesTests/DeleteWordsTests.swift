import AmuxCore
import Foundation
import XCTest
@testable import AmuxFeatures

/// What a delete's confirmation says goes with the agent, and which delete
/// confirming it sends.
///
/// A worktree amux made goes with the last agent in it. The confirmation has
/// to say so, naming uncommitted changes when there are any, and only a
/// confirmation that named them may agree to lose them: a delete without
/// that consent keeps such a worktree on disk.
@MainActor
final class DeleteWordsTests: XCTestCase {
    private let machine = HostId(UUID(uuidString: "00000000-0000-0000-0000-0000000000AB")!)

    private func row(descendants: UInt32 = 0, goes: WorkspaceGoes?) -> FleetRow {
        FleetRow(
            card: FleetCard(
                agent: AgentKey(host: machine, agent: UUID()), name: "fix-scroll", handle: "fix-scroll",
                kind: .claudeSdk, attention: .exited, cwd: "~/src/amux", phaseSinceMs: 0,
                host: "Studio.local", hostReach: .online(.direct), children: 0,
                familyAttention: .exited, members: 1, descendants: descendants, backgroundJobs: 0, closeHeld: false,
                filing: .closed, topLevel: true, branch: "fix-scroll", exitCause: .ended, workspaceGoes: goes),
            depth: 0, expanded: false, secondLine: .blank, loud: nil)
    }

    private let dirty = WorkspaceGoes(
        branch: "fix-scroll", uncommitted: ChangeTotals(files: 3, added: 42, removed: 7))

    func testUncommittedChangesAndTheBranchAreBothNamed() {
        XCTAssertEqual(
            DeleteWords.workspace(dirty), "Its worktree is removed too, with 3 uncommitted files (+42 \u{2212}7).")
        XCTAssertEqual(DeleteWords.branch(dirty), "The branch fix-scroll keeps what was committed.")
        XCTAssertEqual(
            Confirming.delete(row(goes: dirty)).message,
            "Its chat is deleted on every device. Its worktree is removed too, with 3 uncommitted files "
                + "(+42 \u{2212}7). The branch fix-scroll keeps what was committed.")
        XCTAssertEqual(
            Confirming.delete(row(descendants: 2, goes: dirty)).message,
            "Its chat is deleted on every device, with its subagents. Its worktree is removed too, with 3 "
                + "uncommitted files (+42 \u{2212}7). The branch fix-scroll keeps what was committed.")
    }

    func testOneUncommittedFileIsSingular() {
        let one = WorkspaceGoes(branch: nil, uncommitted: ChangeTotals(files: 1, added: 0, removed: 4))
        XCTAssertEqual(DeleteWords.workspace(one), "Its worktree is removed too, with 1 uncommitted file (\u{2212}4).")
        let uncounted = WorkspaceGoes(branch: nil, uncommitted: ChangeTotals(files: 2, added: 0, removed: 0))
        XCTAssertEqual(DeleteWords.workspace(uncounted), "Its worktree is removed too, with 2 uncommitted files.")
    }

    func testACleanWorktreeOnlySaysItGoes() {
        let clean = WorkspaceGoes(branch: "fix-scroll", uncommitted: nil)
        XCTAssertEqual(DeleteWords.workspace(clean), "Its worktree is removed too.")
        XCTAssertEqual(
            Confirming.delete(row(goes: clean)).message,
            "Its chat is deleted on every device. Its worktree is removed too. "
                + "The branch fix-scroll keeps what was committed.")
    }

    func testABranchWithNothingOfItsOwnIsNotMentioned() {
        let unbranched = WorkspaceGoes(branch: nil, uncommitted: ChangeTotals(files: 3, added: 42, removed: 7))
        XCTAssertNil(DeleteWords.branch(unbranched))
        XCTAssertEqual(
            Confirming.delete(row(goes: unbranched)).message,
            "Its chat is deleted on every device. Its worktree is removed too, with 3 uncommitted files "
                + "(+42 \u{2212}7).")
    }

    /// A workspace that stays reads as it always has.
    func testAWorkspaceThatStaysKeepsTodaysWords() {
        XCTAssertEqual(
            Confirming.delete(row(goes: nil)).message, "Its chat is deleted on every device. Its edits stay.")
        XCTAssertEqual(
            Confirming.delete(row(descendants: 1, goes: nil)).message,
            "Its chat is deleted on every device, with its subagents. Its edits stay.")
    }

    /// Consent to lose changes goes only with a confirmation that named them.
    func testOnlyNamedChangesAreDiscarded() {
        let dirtyRow = row(goes: dirty)
        XCTAssertEqual(Confirming.delete(dirtyRow).confirmed, .deleteDiscardingChanges(dirtyRow.id))
        let cleanRow = row(goes: WorkspaceGoes(branch: "fix-scroll", uncommitted: nil))
        XCTAssertEqual(Confirming.delete(cleanRow).confirmed, .delete(cleanRow.id))
        let staysRow = row(goes: nil)
        XCTAssertEqual(Confirming.delete(staysRow).confirmed, .delete(staysRow.id))

        XCTAssertEqual(ChatAction.deleting(dirty), .deleteDiscardingChanges)
        XCTAssertEqual(ChatAction.deleting(WorkspaceGoes(branch: "fix-scroll", uncommitted: nil)), .delete)
        XCTAssertEqual(ChatAction.deleting(nil), .delete)
    }
}
