import AmuxValues
import Foundation

/// What deleting an agent says goes with it, in the words every Apple
/// client's confirmation uses, and whether confirming agrees to lose
/// uncommitted changes.
///
/// A confirmation opens on what the agent's row says goes, and asks the
/// runtime for current facts: the uncommitted changes of an agent with no
/// process are known only to its host, and so are the commits its branch
/// holds that no other branch does. It says it is checking until the answer
/// comes, and confirming before then keeps a worktree with changes on disk,
/// because nothing it drew named them.
public enum DeleteWords {
    /// What a confirmation draws on opening, from what the row says goes:
    /// checking, when a workspace goes at all.
    public static func asking(_ goes: WorkspaceGoes?) -> DeleteWarning? { goes.map(DeleteWarning.checking) }

    /// Whether a confirmation drawing `warning` is waiting on an answer.
    public static func awaitsAnswer(_ warning: DeleteWarning?) -> Bool {
        if case .checking? = warning { true } else { false }
    }

    /// What a confirmation draws once its question is answered. Only one
    /// still checking takes the answer; an answer that is a failure reads as
    /// a host that could not say.
    public static func answered<Failure: Error>(
        _ drawn: DeleteWarning?, with answer: Result<DeleteWarning?, Failure>
    ) -> DeleteWarning? {
        guard case .checking(let asked)? = drawn else { return drawn }
        switch answer {
        case .success(let warning): return warning
        case .failure: return .unknown(asked)
        }
    }

    /// What a confirmation says of the workspace: its line, the branch's
    /// line when it names one and whether the branch stays, and whether
    /// what it says is settled rather than still being checked. Nil when the
    /// workspace stays.
    public static func lines(
        for warning: DeleteWarning?
    ) -> (workspace: String, branch: String?, branchKept: Bool, settled: Bool)? {
        switch warning {
        case nil: nil
        case .goes(let goes)?: (workspace(goes), branch(goes), branchKept(goes), true)
        case .checking(let goes)?:
            (String(localized: "Checking its worktree for uncommitted changes…"), branch(goes), branchKept(goes), false)
        case .unknown(let goes)?:
            (String(localized: "Its worktree is removed too, but stays on disk if it has uncommitted changes, which couldn’t be checked."),
             branch(goes), branchKept(goes), true)
        }
    }

    /// "Its worktree is removed too, with 3 uncommitted files (+42 −7).", or
    /// "Its worktree is removed too." when nothing in it is uncommitted.
    public static func workspace(_ goes: WorkspaceGoes) -> String {
        guard let uncommitted = goes.uncommitted else {
            return String(localized: "Its worktree is removed too.")
        }
        let files = uncommitted.files
        // Files can change with no line counted, a binary or a mode change,
        // and then there are no counts to show.
        let counts = ChatWords.counts(added: uncommitted.added, removed: uncommitted.removed)
        switch (files == 1, counts.isEmpty) {
        case (true, false): return String(localized: "Its worktree is removed too, with 1 uncommitted file (\(counts)).")
        case (true, true): return String(localized: "Its worktree is removed too, with 1 uncommitted file.")
        case (false, false):
            return String(localized: "Its worktree is removed too, with \(files) uncommitted files (\(counts)).")
        case (false, true): return String(localized: "Its worktree is removed too, with \(files) uncommitted files.")
        }
    }

    /// What becomes of the branch the row names: the host deletes one with
    /// no commits of its own and keeps any other. "The branch fix-scroll
    /// keeps its 3 commits.", or "goes too, since nothing was committed to
    /// it."; until the host has counted them, words true either way.
    public static func branch(_ goes: WorkspaceGoes) -> String? {
        guard let branch = goes.branch else { return nil }
        switch goes.branchCommits {
        case 0?: return String(localized: "The branch \(branch) goes too, since nothing was committed to it.")
        case 1?: return String(localized: "The branch \(branch) keeps its 1 commit.")
        case let commits?: return String(localized: "The branch \(branch) keeps its \(commits) commits.")
        case nil: return String(localized: "The branch \(branch) stays if anything was committed to it.")
        }
    }

    /// Whether the branch is kept: unless the host counted no commits of its
    /// own, which is when it goes with the workspace.
    public static func branchKept(_ goes: WorkspaceGoes) -> Bool { goes.branchCommits != 0 }

    /// A dialog's words under "Delete name?": the chat and its subagents,
    /// then what happens to the workspace.
    public static func message(descendants: UInt32, warning: DeleteWarning?) -> String {
        guard let lines = lines(for: warning) else {
            return descendants > 0
                ? String(localized: "Its chat is deleted on every device, with its subagents. Its edits stay.")
                : String(localized: "Its chat is deleted on every device. Its edits stay.")
        }
        let chat = descendants > 0
            ? String(localized: "Its chat is deleted on every device, with its subagents.")
            : String(localized: "Its chat is deleted on every device.")
        return [chat, lines.workspace, lines.branch].compactMap { $0 }.joined(separator: " ")
    }

    /// Whether confirming agrees to lose uncommitted changes: only when the
    /// confirmation drew them from current facts. A delete without that
    /// consent keeps such a workspace on disk.
    public static func discards(_ warning: DeleteWarning?) -> Bool {
        if case .goes(let goes)? = warning { goes.uncommitted != nil } else { false }
    }
}
