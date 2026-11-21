import AmuxValues
import Foundation

/// What deleting an agent says goes with it, in the words every Apple
/// client's confirmation uses, and whether confirming agrees to lose
/// uncommitted changes.
///
/// Only the agent's host knows whether its workspace goes with it, and what
/// that holds now, so a confirmation opens checking and asks the runtime,
/// which asks the host. It says it is checking until the answer comes, and
/// confirming before then keeps a worktree with changes on disk, because
/// nothing it drew named them.
public enum DeleteWords {
    /// Whether a confirmation drawing `warning` is waiting on an answer.
    public static func awaitsAnswer(_ warning: DeleteWarning?) -> Bool {
        if case .checking? = warning { true } else { false }
    }

    /// What a confirmation draws once its question is answered. Only one
    /// still checking takes the answer; an answer that is a failure reads as
    /// a host that could not be asked.
    public static func answered<Failure: Error>(
        _ drawn: DeleteWarning?, with answer: Result<DeleteWarning?, Failure>
    ) -> DeleteWarning? {
        guard case .checking? = drawn else { return drawn }
        switch answer {
        case .success(let warning): return warning
        case .failure: return .unknown
        }
    }

    /// What a confirmation says of the workspace: its line, the branch's
    /// line when the host named one and whether the branch stays, and
    /// whether the host has said the worktree goes, which makes the line a
    /// loss rather than an open question. Nil when the workspace stays.
    public static func lines(
        for warning: DeleteWarning?
    ) -> (workspace: String, branch: String?, branchKept: Bool, settled: Bool)? {
        switch warning {
        case nil: nil
        case .goes(let goes)?: (workspace(goes), branch(goes), branchKept(goes), true)
        case .checking?: (String(localized: "Checking whether its worktree is removed too…"), nil, true, false)
        case .unknown?:
            (String(localized: "Its worktree couldn’t be checked. If it’s removed too, it stays on disk while it has uncommitted changes."),
             nil, true, false)
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

    /// What becomes of the workspace's branch: the host deletes one with no
    /// commits of its own and keeps any other. "The branch fix-scroll keeps
    /// its 3 commits.", or "goes too, since nothing was committed to it.";
    /// when the host could not count them, words true either way.
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
    /// confirmation drew them from the host's answer. A delete without that
    /// consent keeps such a workspace on disk.
    public static func discards(_ warning: DeleteWarning?) -> Bool {
        if case .goes(let goes)? = warning { goes.uncommitted != nil } else { false }
    }
}
