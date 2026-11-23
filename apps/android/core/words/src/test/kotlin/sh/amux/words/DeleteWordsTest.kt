package sh.amux.words

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.ChangeTotals
import sh.amux.values.DeleteWarning
import sh.amux.values.WorkspaceGoes

class DeleteWordsTest {
    private val dirty = WorkspaceGoes(
        branch = "fix-scroll",
        branchCommits = null,
        uncommitted = ChangeTotals(files = 3, added = 42, removed = 7),
    )

    @Test
    fun uncommittedChangesAndTheBranchAreBothNamed() {
        assertEquals(
            "Its worktree is removed too, with 3 uncommitted files (+42 \u22127).",
            DeleteWords.workspace(dirty),
        )
        assertEquals(
            "Its worktree is removed too, with 3 uncommitted files (+42 \u22127).",
            DeleteWords.lines(DeleteWarning.Goes(dirty))?.workspace,
        )
        assertEquals(
            "The branch fix-scroll stays if anything was committed to it.",
            DeleteWords.branch(dirty),
        )
        assertEquals(
            "Its chat is deleted on every device. Its worktree is removed too, with 3 uncommitted files " +
                "(+42 \u22127). The branch fix-scroll stays if anything was committed to it.",
            DeleteWords.message(descendants = 0, warning = DeleteWarning.Goes(dirty)),
        )
        assertEquals(
            "Its chat is deleted on every device, with its subagents. Its worktree is removed too, with 3 " +
                "uncommitted files (+42 \u22127). The branch fix-scroll stays if anything was committed to it.",
            DeleteWords.message(descendants = 2, warning = DeleteWarning.Goes(dirty)),
        )
    }

    @Test
    fun theBranchLineSaysWhatTheHostCounted() {
        fun counted(commits: Int?) = WorkspaceGoes(
            branch = "fix-scroll",
            branchCommits = commits,
            uncommitted = null,
        )
        assertEquals(
            "The branch fix-scroll goes too, since nothing was committed to it.",
            DeleteWords.branch(counted(0)),
        )
        assertFalse(DeleteWords.branchKept(counted(0)))
        assertEquals("The branch fix-scroll keeps its 1 commit.", DeleteWords.branch(counted(1)))
        assertTrue(DeleteWords.branchKept(counted(1)))
        assertEquals("The branch fix-scroll keeps its 3 commits.", DeleteWords.branch(counted(3)))
        assertEquals(
            "The branch fix-scroll stays if anything was committed to it.",
            DeleteWords.branch(counted(null)),
        )
        assertTrue(DeleteWords.branchKept(counted(null)))
        assertEquals(
            "Its chat is deleted on every device. Its worktree is removed too. " +
                "The branch fix-scroll keeps its 2 commits.",
            DeleteWords.message(descendants = 0, warning = DeleteWarning.Goes(counted(2))),
        )
    }

    @Test
    fun oneUncommittedFileIsSingular() {
        val one = WorkspaceGoes(
            branch = null,
            branchCommits = null,
            uncommitted = ChangeTotals(files = 1, added = 0, removed = 4),
        )
        assertEquals("Its worktree is removed too, with 1 uncommitted file (\u22124).", DeleteWords.workspace(one))
        val uncountedOne = WorkspaceGoes(
            branch = null,
            branchCommits = null,
            uncommitted = ChangeTotals(files = 1, added = 0, removed = 0),
        )
        assertEquals("Its worktree is removed too, with 1 uncommitted file.", DeleteWords.workspace(uncountedOne))
        val uncounted = WorkspaceGoes(
            branch = null,
            branchCommits = null,
            uncommitted = ChangeTotals(files = 2, added = 0, removed = 0),
        )
        assertEquals("Its worktree is removed too, with 2 uncommitted files.", DeleteWords.workspace(uncounted))
    }

    @Test
    fun aCleanWorktreeOnlySaysItGoes() {
        val clean = WorkspaceGoes(branch = "fix-scroll", branchCommits = null, uncommitted = null)
        assertEquals("Its worktree is removed too.", DeleteWords.workspace(clean))
        assertEquals(
            "Its chat is deleted on every device. Its worktree is removed too. " +
                "The branch fix-scroll stays if anything was committed to it.",
            DeleteWords.message(descendants = 0, warning = DeleteWarning.Goes(clean)),
        )
    }

    @Test
    fun aBranchWithNothingOfItsOwnIsNotMentioned() {
        val unbranched = WorkspaceGoes(
            branch = null,
            branchCommits = null,
            uncommitted = ChangeTotals(files = 3, added = 42, removed = 7),
        )
        assertNull(DeleteWords.branch(unbranched))
        assertEquals(
            "Its chat is deleted on every device. Its worktree is removed too, with 3 uncommitted files (+42 \u22127).",
            DeleteWords.message(descendants = 0, warning = DeleteWarning.Goes(unbranched)),
        )
    }

    @Test
    fun aWorkspaceThatStaysKeepsStandardWords() {
        assertEquals(
            "Its chat is deleted on every device. Its edits stay.",
            DeleteWords.message(descendants = 0, warning = null),
        )
        assertEquals(
            "Its chat is deleted on every device, with its subagents. Its edits stay.",
            DeleteWords.message(descendants = 1, warning = null),
        )
        assertNull(DeleteWords.lines(null))
    }

    @Test
    fun checkingAndUnknownWarningsBehaveSafely() {
        val checking: DeleteWarning = DeleteWarning.Checking
        assertTrue(DeleteWords.awaitsAnswer(checking))
        assertFalse(DeleteWords.discards(checking))
        val checkingLines = DeleteWords.lines(checking)
        assertEquals("Checking whether its worktree is removed too\u2026", checkingLines?.workspace)
        assertNull(checkingLines?.branch)
        assertEquals(false, checkingLines?.settled)

        val unknown = DeleteWords.answered(checking, Result.failure(IllegalStateException("offline")))
        assertEquals(DeleteWarning.Unknown, unknown)
        assertFalse(DeleteWords.awaitsAnswer(unknown))
        assertFalse(DeleteWords.discards(unknown))
        val unknownLines = DeleteWords.lines(unknown)
        assertEquals(
            "Its worktree couldn\u2019t be checked. If it\u2019s removed too, it stays on disk while it has uncommitted changes.",
            unknownLines?.workspace,
        )
        // Nothing is said to go, so the line is not drawn as a loss.
        assertEquals(false, unknownLines?.settled)

        // An answer replaces only a checking warning.
        assertEquals(unknown, DeleteWords.answered(unknown, Result.success(DeleteWarning.Goes(dirty))))
        assertEquals(DeleteWarning.Goes(dirty), DeleteWords.answered(checking, Result.success(DeleteWarning.Goes(dirty))))
        assertNull(DeleteWords.answered(checking, Result.success(null)))
    }

    @Test
    fun onlyNamedChangesAreDiscarded() {
        val clean = WorkspaceGoes(branch = "fix-scroll", branchCommits = null, uncommitted = null)
        assertTrue(DeleteWords.discards(DeleteWarning.Goes(dirty)))
        assertFalse(DeleteWords.discards(DeleteWarning.Goes(clean)))
        assertFalse(DeleteWords.discards(DeleteWarning.Checking))
        assertFalse(DeleteWords.discards(DeleteWarning.Unknown))
        assertFalse(DeleteWords.discards(null))
    }
}
