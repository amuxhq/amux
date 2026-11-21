package sh.amux.words

import sh.amux.values.DeleteWarning
import sh.amux.values.WorkspaceGoes

/**
 * What deleting an agent says goes with it, in the words the delete
 * confirmation uses, and whether confirming agrees to lose uncommitted
 * changes.
 */
object DeleteWords {
    data class Lines(
        val workspace: String,
        val branch: String?,
        val branchKept: Boolean,
        val settled: Boolean,
    )

    /**
     * What a confirmation draws on opening, from what the row says goes:
     * checking, when a workspace goes at all.
     */
    fun asking(goes: WorkspaceGoes?): DeleteWarning? = goes?.let { DeleteWarning.Checking(it) }

    /** Whether a confirmation drawing [warning] is waiting on an answer. */
    fun awaitsAnswer(warning: DeleteWarning?): Boolean = warning is DeleteWarning.Checking

    /**
     * What a confirmation draws once its question is answered. Only one
     * still checking takes the answer; an answer that is a failure reads as
     * a host that could not say.
     */
    fun answered(drawn: DeleteWarning?, answer: Result<DeleteWarning?>): DeleteWarning? {
        if (drawn !is DeleteWarning.Checking) return drawn
        return answer.fold(
            onSuccess = { it },
            onFailure = { DeleteWarning.Unknown(drawn.value) },
        )
    }

    /**
     * What a confirmation says of the workspace: its line, the branch's line
     * when it names one and whether the branch stays, and whether what it says
     * is settled rather than still being checked. Null when the workspace stays.
     */
    fun lines(warning: DeleteWarning?): Lines? = when (warning) {
        null -> null
        is DeleteWarning.Goes -> Lines(
            workspace = workspace(warning.value),
            branch = branch(warning.value),
            branchKept = branchKept(warning.value),
            settled = true,
        )
        is DeleteWarning.Checking -> Lines(
            workspace = "Checking its worktree for uncommitted changes\u2026",
            branch = branch(warning.value),
            branchKept = branchKept(warning.value),
            settled = false,
        )
        is DeleteWarning.Unknown -> Lines(
            workspace = "Its worktree is removed too, but stays on disk if it has uncommitted changes, which couldn\u2019t be checked.",
            branch = branch(warning.value),
            branchKept = branchKept(warning.value),
            settled = true,
        )
    }

    /**
     * `"Its worktree is removed too, with 3 uncommitted files (+42 −7)."`, or
     * `"Its worktree is removed too."` when nothing in it is uncommitted.
     */
    fun workspace(goes: WorkspaceGoes): String {
        val uncommitted = goes.uncommitted ?: return "Its worktree is removed too."
        val files = uncommitted.files
        val counts = ChatWords.counts(added = uncommitted.added, removed = uncommitted.removed)
        return when {
            files == 1 && counts.isNotEmpty() -> "Its worktree is removed too, with 1 uncommitted file ($counts)."
            files == 1 -> "Its worktree is removed too, with 1 uncommitted file."
            counts.isNotEmpty() -> "Its worktree is removed too, with $files uncommitted files ($counts)."
            else -> "Its worktree is removed too, with $files uncommitted files."
        }
    }

    /**
     * What becomes of the branch the row names: the host deletes one with
     * no commits of its own and keeps any other.
     */
    fun branch(goes: WorkspaceGoes): String? {
        val branch = goes.branch ?: return null
        return when (val commits = goes.branchCommits) {
            0 -> "The branch $branch goes too, since nothing was committed to it."
            1 -> "The branch $branch keeps its 1 commit."
            null -> "The branch $branch stays if anything was committed to it."
            else -> "The branch $branch keeps its $commits commits."
        }
    }

    /**
     * Whether the branch is kept: unless the host counted no commits of its
     * own, which is when it goes with the workspace.
     */
    fun branchKept(goes: WorkspaceGoes): Boolean = goes.branchCommits != 0

    /**
     * A dialog's words under `"Delete name?"`: the chat and its subagents,
     * then what happens to the workspace.
     */
    fun message(descendants: Int, warning: DeleteWarning?): String {
        val lines = lines(warning)
            ?: return if (descendants > 0) {
                "Its chat is deleted on every device, with its subagents. Its edits stay."
            } else {
                "Its chat is deleted on every device. Its edits stay."
            }
        val chat = if (descendants > 0) {
            "Its chat is deleted on every device, with its subagents."
        } else {
            "Its chat is deleted on every device."
        }
        return listOfNotNull(chat, lines.workspace, lines.branch).joinToString(" ")
    }

    /**
     * Whether confirming agrees to lose uncommitted changes: only when the
     * confirmation drew them from current facts. A delete without that
     * consent keeps such a workspace on disk.
     */
    fun discards(warning: DeleteWarning?): Boolean =
        warning is DeleteWarning.Goes && warning.value.uncommitted != null
}
