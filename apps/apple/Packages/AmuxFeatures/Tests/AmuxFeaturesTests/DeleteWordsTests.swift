import AmuxCore
import AmuxWords
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
///
/// An agent with no process, which is most agents a delete is offered for,
/// has nothing current on its row about what is uncommitted: its host is
/// asked, and until it answers the confirmation says it is checking and
/// agrees to lose nothing.
@MainActor
final class DeleteWordsTests: XCTestCase {
    private let machine = HostId(UUID(uuidString: "00000000-0000-0000-0000-0000000000AB")!)

    private func row(descendants: UInt32 = 0, goes: WorkspaceGoes?) -> FleetRow {
        FleetRow(
            card: FleetCard(
                agent: AgentKey(host: machine, agent: UUID()), name: "fix-scroll", handle: "fix-scroll",
                kind: .claudeSdk, provider: "claude", providerName: "Claude", attachable: false, attention: .exited, cwd: "~/src/amux", phaseSinceMs: 0,
                host: "Studio.local", hostReach: .online(.direct), children: 0,
                familyAttention: .exited, members: 1, descendants: descendants, backgroundJobs: 0, closeHeld: false,
                filing: .closed, topLevel: true, native: false, branch: "fix-scroll", exitCause: .ended,
                repository: nil, workspaceGoes: goes),
            depth: 0, expanded: false, secondLine: .blank, loud: nil)
    }

    private let dirty = WorkspaceGoes(
        branch: "fix-scroll", branchCommits: nil, uncommitted: ChangeTotals(files: 3, added: 42, removed: 7))

    /// The home's dialog once the host has said what goes.
    private func answered(_ row: FleetRow, _ answer: DeleteWarning?) -> Confirming {
        Confirming.deleting(row).answered(.success(answer), for: row.id)
    }

    func testUncommittedChangesAndTheBranchAreBothNamed() {
        XCTAssertEqual(
            DeleteWords.workspace(dirty), "Its worktree is removed too, with 3 uncommitted files (+42 \u{2212}7).")
        XCTAssertEqual(
            DeleteWords.lines(for: .goes(dirty))?.workspace,
            "Its worktree is removed too, with 3 uncommitted files (+42 \u{2212}7).")
        XCTAssertEqual(DeleteWords.branch(dirty), "The branch fix-scroll stays if anything was committed to it.")
        XCTAssertEqual(
            answered(row(goes: dirty), .goes(dirty)).message,
            "Its chat is deleted on every device. Its worktree is removed too, with 3 uncommitted files "
                + "(+42 \u{2212}7). The branch fix-scroll stays if anything was committed to it.")
        XCTAssertEqual(
            answered(row(descendants: 2, goes: dirty), .goes(dirty)).message,
            "Its chat is deleted on every device, with its subagents. Its worktree is removed too, with 3 "
                + "uncommitted files (+42 \u{2212}7). The branch fix-scroll stays if anything was committed to it.")
    }

    /// What becomes of the branch is the host's count of its own commits:
    /// it keeps them, or goes too when there are none. Until the host has
    /// counted, the words are true either way.
    func testTheBranchLineSaysWhatTheHostCounted() {
        let counted = { (commits: UInt32?) in
            WorkspaceGoes(branch: "fix-scroll", branchCommits: commits, uncommitted: nil)
        }
        XCTAssertEqual(
            DeleteWords.branch(counted(0)), "The branch fix-scroll goes too, since nothing was committed to it.")
        XCTAssertEqual(DeleteWords.branch(counted(1)), "The branch fix-scroll keeps its 1 commit.")
        XCTAssertEqual(DeleteWords.branch(counted(3)), "The branch fix-scroll keeps its 3 commits.")
        XCTAssertEqual(
            DeleteWords.branch(counted(nil)), "The branch fix-scroll stays if anything was committed to it.")
        XCTAssertEqual(
            answered(row(goes: dirty), .goes(counted(2))).message,
            "Its chat is deleted on every device. Its worktree is removed too. "
                + "The branch fix-scroll keeps its 2 commits.")
    }

    func testOneUncommittedFileIsSingular() {
        let one = WorkspaceGoes(branch: nil, branchCommits: nil, uncommitted: ChangeTotals(files: 1, added: 0, removed: 4))
        XCTAssertEqual(DeleteWords.workspace(one), "Its worktree is removed too, with 1 uncommitted file (\u{2212}4).")
        let uncounted = WorkspaceGoes(branch: nil, branchCommits: nil, uncommitted: ChangeTotals(files: 2, added: 0, removed: 0))
        XCTAssertEqual(DeleteWords.workspace(uncounted), "Its worktree is removed too, with 2 uncommitted files.")
    }

    func testACleanWorktreeOnlySaysItGoes() {
        let clean = WorkspaceGoes(branch: "fix-scroll", branchCommits: nil, uncommitted: nil)
        XCTAssertEqual(DeleteWords.workspace(clean), "Its worktree is removed too.")
        XCTAssertEqual(
            answered(row(goes: clean), .goes(clean)).message,
            "Its chat is deleted on every device. Its worktree is removed too. "
                + "The branch fix-scroll stays if anything was committed to it.")
    }

    func testABranchWithNothingOfItsOwnIsNotMentioned() {
        let unbranched = WorkspaceGoes(branch: nil, branchCommits: nil, uncommitted: ChangeTotals(files: 3, added: 42, removed: 7))
        XCTAssertNil(DeleteWords.branch(unbranched))
        XCTAssertEqual(
            answered(row(goes: unbranched), .goes(unbranched)).message,
            "Its chat is deleted on every device. Its worktree is removed too, with 3 uncommitted files "
                + "(+42 \u{2212}7).")
    }

    /// A workspace that stays reads as it always has.
    func testAWorkspaceThatStaysKeepsTodaysWords() {
        XCTAssertEqual(
            Confirming.deleting(row(goes: nil)).message, "Its chat is deleted on every device. Its edits stay.")
        XCTAssertEqual(
            Confirming.deleting(row(descendants: 1, goes: nil)).message,
            "Its chat is deleted on every device, with its subagents. Its edits stay.")
        XCTAssertNil(DeleteWords.lines(for: nil))
    }

    /// While the host is asked the dialog says so, with the branch the row
    /// names, and names no changes.
    func testACheckingConfirmationSaysSoAndNamesNoChanges() {
        let opened = Confirming.deleting(row(goes: WorkspaceGoes(branch: "fix-scroll", branchCommits: nil, uncommitted: nil)))
        XCTAssertEqual(
            DeleteWords.lines(for: .checking(dirty))?.workspace, "Checking its worktree for uncommitted changes\u{2026}")
        XCTAssertEqual(
            opened.message,
            "Its chat is deleted on every device. Checking its worktree for uncommitted changes\u{2026} "
                + "The branch fix-scroll stays if anything was committed to it.")
        XCTAssertNotNil(opened.asking, "a dialog over a workspace that goes does not ask what it holds")
        XCTAssertNil(Confirming.deleting(row(goes: nil)).asking, "a workspace that stays is asked about")
    }

    /// A host that could not say leaves the worktree on disk if it holds
    /// changes, and the dialog says so rather than promising it goes.
    func testAnUnknownWorktreeSaysItStaysIfItHasChanges() {
        let unknown = "Its worktree is removed too, but stays on disk if it has uncommitted changes, "
            + "which couldn\u{2019}t be checked."
        XCTAssertEqual(DeleteWords.lines(for: .unknown(dirty))?.workspace, unknown)
        XCTAssertEqual(DeleteWords.lines(for: .unknown(dirty))?.branch, "The branch fix-scroll stays if anything was committed to it.")
        let asked = row(goes: dirty)
        let failed = Confirming.deleting(asked)
            .answered(.failure(RuntimeFailure("Studio cannot be reached")), for: asked.id)
        guard case .delete(_, let drawn) = failed else { return XCTFail("the dialog stopped asking about a delete") }
        XCTAssertEqual(drawn, .unknown(dirty), "a failed question is not drawn as a host that could not say")
    }

    /// The row says the agent's worktree goes, but nothing about changes in
    /// it: the agent has no process to say. Confirming before the host
    /// answers, or after it failed to, keeps changes; once the host has
    /// counted them, confirming agrees to lose them.
    func testASuspendedAgentsDeleteConsentsOnlyOnceItsHostHasCountedChanges() {
        let suspended = row(goes: WorkspaceGoes(branch: "fix-scroll", branchCommits: nil, uncommitted: nil))
        let opened = Confirming.deleting(suspended)
        XCTAssertEqual(opened.confirmed, .delete(suspended.id), "checking already agrees to lose changes")

        let counted = opened.answered(.success(.goes(dirty)), for: suspended.id)
        XCTAssertEqual(counted.confirmed, .deleteDiscardingChanges(suspended.id))
        XCTAssertNil(counted.asking, "an answered dialog asks again")
        XCTAssertEqual(
            counted.message,
            "Its chat is deleted on every device. Its worktree is removed too, with 3 uncommitted files "
                + "(+42 \u{2212}7). The branch fix-scroll stays if anything was committed to it.")

        let failed = opened.answered(.failure(RuntimeFailure("Studio cannot be reached")), for: suspended.id)
        XCTAssertEqual(failed.confirmed, .delete(suspended.id), "a failed question agrees to lose changes")

        let stays = opened.answered(.success(nil), for: suspended.id)
        XCTAssertEqual(stays.message, "Its chat is deleted on every device. Its edits stay.")
        XCTAssertEqual(stays.confirmed, .delete(suspended.id))
    }

    /// An answer lands only on the dialog that asked it, and only once.
    func testAnAnswerForAnotherAgentOrASettledDialogChangesNothing() {
        let asked = row(goes: dirty)
        let other = row(goes: dirty)
        let opened = Confirming.deleting(other)
        let misdirected = opened.answered(.success(.goes(dirty)), for: asked.id)
        XCTAssertEqual(misdirected.confirmed, .delete(other.id), "another agent's answer agreed to lose changes")
        XCTAssertEqual(misdirected.message, opened.message)

        let clean = WorkspaceGoes(branch: "fix-scroll", branchCommits: nil, uncommitted: nil)
        let settled = opened.answered(.success(.goes(clean)), for: other.id)
        let again = settled.answered(.success(.goes(dirty)), for: other.id)
        XCTAssertEqual(again.confirmed, .delete(other.id), "a second answer replaced what the dialog drew")
        XCTAssertEqual(
            DeleteWords.answered(.unknown(clean), with: Result<DeleteWarning?, RuntimeFailure>.success(.goes(dirty))),
            .unknown(clean),
            "an answer replaced a warning that was not checking")
    }

    /// Consent to lose changes goes only with a confirmation that drew them
    /// from current facts.
    func testOnlyNamedChangesAreDiscarded() {
        let dirtyRow = row(goes: dirty)
        XCTAssertEqual(answered(dirtyRow, .goes(dirty)).confirmed, .deleteDiscardingChanges(dirtyRow.id))
        let cleanGoes = WorkspaceGoes(branch: "fix-scroll", branchCommits: nil, uncommitted: nil)
        let cleanRow = row(goes: cleanGoes)
        XCTAssertEqual(answered(cleanRow, .goes(cleanGoes)).confirmed, .delete(cleanRow.id))
        let staysRow = row(goes: nil)
        XCTAssertEqual(Confirming.deleting(staysRow).confirmed, .delete(staysRow.id))

        XCTAssertTrue(DeleteWords.discards(.goes(dirty)))
        XCTAssertFalse(DeleteWords.discards(.goes(cleanGoes)))
        XCTAssertFalse(DeleteWords.discards(.checking(dirty)), "checking agrees to lose changes")
        XCTAssertFalse(DeleteWords.discards(.unknown(dirty)), "a host that could not say agrees to lose changes")
        XCTAssertFalse(DeleteWords.discards(nil))

        XCTAssertEqual(ChatAction.deleting(.goes(dirty)), .deleteDiscardingChanges)
        XCTAssertEqual(ChatAction.deleting(.goes(cleanGoes)), .delete)
        XCTAssertEqual(ChatAction.deleting(.checking(dirty)), .delete)
        XCTAssertEqual(ChatAction.deleting(.unknown(dirty)), .delete)
        XCTAssertEqual(ChatAction.deleting(nil), .delete)
    }

    // MARK: - The home's dialog, which never redraws once it is up

    /// Asked quickly enough, as the runtime answers for an agent with a
    /// process, the dialog goes up once, on the answer.
    func testAQuickAnswerPutsTheDialogUpOnceOnIt() async {
        let suspended = row(goes: WorkspaceGoes(branch: "fix-scroll", branchCommits: nil, uncommitted: nil))
        var shown: [Confirming?] = []
        await DeleteAsking(patience: .seconds(60), swap: .zero).run(
            .deleting(suspended), ask: { _ in .success(.goes(self.dirty)) },
            showing: { shown.last ?? nil }, show: { shown.append($0) })
        XCTAssertEqual(shown.count, 1, "a quick answer went up as more than one dialog")
        XCTAssertEqual(shown.first??.confirmed, .deleteDiscardingChanges(suspended.id))
    }

    /// A slow host: the dialog goes up checking, which agrees to lose
    /// nothing, then is put away and the answered one put up in its place,
    /// because a dialog that is up keeps what it first said.
    func testASlowAnswerReplacesTheCheckingDialog() async {
        let suspended = row(goes: WorkspaceGoes(branch: "fix-scroll", branchCommits: nil, uncommitted: nil))
        var shown: [Confirming?] = []
        await DeleteAsking(patience: .milliseconds(10), swap: .milliseconds(10)).run(
            .deleting(suspended),
            ask: { _ in
                while shown.isEmpty { try? await Task.sleep(for: .milliseconds(5)) }
                return .success(.goes(self.dirty))
            },
            showing: { shown.last ?? nil }, show: { shown.append($0) })
        XCTAssertEqual(shown.count, 3, "the checking dialog was not replaced: \(shown)")
        XCTAssertEqual(shown.first??.confirmed, .delete(suspended.id), "checking agreed to lose changes")
        XCTAssertTrue(shown.first??.message.contains("Checking its worktree") ?? false)
        XCTAssertNil(shown[1], "the checking dialog was not put away before the answer went up")
        XCTAssertEqual(shown.last??.confirmed, .deleteDiscardingChanges(suspended.id))
    }

    /// Closed while checking, by Cancel or by Delete, the dialog stays
    /// closed when the answer comes.
    func testAnAnswerAfterTheDialogClosedPutsNothingUp() async {
        let suspended = row(goes: WorkspaceGoes(branch: "fix-scroll", branchCommits: nil, uncommitted: nil))
        var shown: [Confirming?] = []
        var up: Confirming?
        await DeleteAsking(patience: .milliseconds(10), swap: .zero).run(
            .deleting(suspended),
            ask: { _ in
                while shown.isEmpty { try? await Task.sleep(for: .milliseconds(5)) }
                up = nil
                return .success(.goes(self.dirty))
            },
            showing: { up }, show: { shown.append($0); up = $0 })
        XCTAssertEqual(shown.count, 1, "a closed dialog came back: \(shown)")
    }

    /// A host that cannot be asked: the dialog says the worktree stays if it
    /// has changes, and confirming agrees to lose none.
    func testAFailedAnswerPutsUpADialogThatKeepsChanges() async {
        let suspended = row(goes: WorkspaceGoes(branch: "fix-scroll", branchCommits: nil, uncommitted: nil))
        var shown: [Confirming?] = []
        await DeleteAsking(patience: .seconds(60), swap: .zero).run(
            .deleting(suspended), ask: { _ in .failure(RuntimeFailure("Studio cannot be reached")) },
            showing: { shown.last ?? nil }, show: { shown.append($0) })
        XCTAssertEqual(shown.count, 1)
        XCTAssertEqual(shown.first??.confirmed, .delete(suspended.id), "a failed question agreed to lose changes")
        XCTAssertTrue(shown.first??.message.contains("stays on disk if it has uncommitted changes") ?? false)
    }

    /// A workspace that stays has nothing to ask: the dialog goes up at once.
    func testADeleteThatKeepsTheWorkspaceAsksNothing() async {
        let stays = row(goes: nil)
        var shown: [Confirming?] = []
        var asked = 0
        await DeleteAsking().run(
            .deleting(stays), ask: { _ in asked += 1; return .success(nil) },
            showing: { shown.last ?? nil }, show: { shown.append($0) })
        XCTAssertEqual(asked, 0)
        XCTAssertEqual(shown.count, 1)
        XCTAssertEqual(shown.first??.confirmed, .delete(stays.id))
    }

    /// A question given up, as when the home goes away, puts nothing up,
    /// not even the checking dialog its patience would have.
    func testACancelledQuestionPutsNothingUp() async {
        let suspended = row(goes: WorkspaceGoes(branch: "fix-scroll", branchCommits: nil, uncommitted: nil))
        var shown: [Confirming?] = []
        let asking = Task { @MainActor in
            await DeleteAsking(patience: .milliseconds(50), swap: .zero).run(
                .deleting(suspended),
                ask: { _ in
                    try? await Task.sleep(for: .seconds(60))
                    return .success(.goes(self.dirty))
                },
                showing: { shown.last ?? nil }, show: { shown.append($0) })
        }
        asking.cancel()
        await asking.value
        try? await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(shown.isEmpty, "a question given up put a dialog up: \(shown)")
    }
}
