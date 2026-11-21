package sh.amux.core.runtime

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.ActOutcome
import sh.amux.values.AgentKey
import sh.amux.values.Base
import sh.amux.values.BlobRef
import sh.amux.values.ChatDelta
import sh.amux.values.ChatFrame
import sh.amux.values.Comparison
import sh.amux.values.Composer
import sh.amux.values.ComposerView
import sh.amux.values.Connection
import sh.amux.values.ControlsSummary
import sh.amux.values.Diff
import sh.amux.values.DiffBase
import sh.amux.values.DiffFile
import sh.amux.values.Draft
import sh.amux.values.DraftAttachment
import sh.amux.values.Empty
import sh.amux.values.FileStatus
import sh.amux.values.FormValue
import sh.amux.values.FrozenReview
import sh.amux.values.InputState
import sh.amux.values.KeysMoved
import sh.amux.values.Kind
import sh.amux.values.LineKind
import sh.amux.values.Overview
import sh.amux.values.PageOutcome
import sh.amux.values.PhaseView
import sh.amux.values.QuestionResponse
import sh.amux.values.ReviewComment
import sh.amux.values.Row
import sh.amux.values.SendOutcome
import sh.amux.values.SettingChange

class ReviewModelTest {
    private val samplePatch = """
        diff --git a/src/pairing/errors.rs b/src/pairing/errors.rs
        index 1111111..2222222 100644
        --- a/src/pairing/errors.rs
        +++ b/src/pairing/errors.rs
        @@ -10,4 +10,3 @@ pub enum PairError {
             Expired,
        -    Refused,
        -    Unknown,
        +    Refused(String),
         }
        diff --git a/tests/errors.rs b/tests/errors.rs
        index 3333333..4444444 100644
        --- a/tests/errors.rs
        +++ b/tests/errors.rs
        @@ -1,2 +1,3 @@
         use pairing::PairError;
        +use pairing::describe;
         fn main() {}
        diff --git a/docs/PAIRING.md b/docs/PAIRING.md
        new file mode 100644
        index 0000000..5555555
        --- /dev/null
        +++ b/docs/PAIRING.md
        @@ -0,0 +1,2 @@
        +# Pairing
        +Errors now carry the reason.
    """.trimIndent() + "\n"

    private val branchPatch = """
        diff --git a/README.md b/README.md
        index 1111111..2222222 100644
        --- a/README.md
        +++ b/README.md
        @@ -1,1 +1,2 @@
         # amux
        +Android support.
    """.trimIndent() + "\n"

    private val frozenWorking = FrozenReview(
        diff = Diff(
            head = "4f2a9c1",
            files = emptyList(),
            base = DiffBase(base = Base.Head(Empty())),
            patch = BlobRef(
                hash = listOf(7, 7, 7),
                name = "patch",
                mime = "text/x-diff",
                size = samplePatch.encodeToByteArray().size.toLong(),
            ),
        ),
        patch = samplePatch,
    )

    private val frozenBranch = FrozenReview(
        diff = Diff(
            head = "9c0ffee",
            files = emptyList(),
            base = DiffBase(base = Base.Branch("main")),
            patch = BlobRef(
                hash = listOf(8, 8, 8),
                name = "patch",
                mime = "text/x-diff",
                size = branchPatch.encodeToByteArray().size.toLong(),
            ),
        ),
        patch = branchPatch,
    )

    private class ReviewSource(
        val byComparison: MutableMap<Comparison, Result<FrozenReview>> = mutableMapOf(),
    ) : ChatSource {
        val sent = mutableListOf<Draft>()

        override fun opening(): ChatDelta = ChatDelta(
            chat = 0L,
            rows = emptyList(),
            frame = ChatFrame(
                agent = AgentKey(listOf(1), listOf(2)),
                name = "a",
                kind = Kind.ClaudeSdk,
                phase = PhaseView.Idle,
                composer = ComposerView(mode = Composer.Send, switchedAway = false),
                controls = ControlsSummary(),
                connection = Connection.Live,
                caughtUp = true,
                hasOlder = false,
                arrivalsHeld = false,
                queue = emptyList(),
                underway = emptyList(),
                refused = emptyList(),
            ),
            keys = KeysMoved.Reload(emptyList()),
        )

        override fun follow(following: Boolean) {}
        override fun rowsFor(keys: List<String>): List<Row> = emptyList()
        override suspend fun send(draft: Draft): Result<SendOutcome> {
            sent.add(draft)
            return Result.success(SendOutcome(listOf(1), InputState.Sent))
        }
        override suspend fun answer(ask: String, choice: Int, note: String?): ActOutcome = ActOutcome.Done
        override suspend fun answerQuestions(ask: String, responses: List<QuestionResponse>): ActOutcome = ActOutcome.Done
        override suspend fun replyInstead(ask: String, text: String, soFar: List<QuestionResponse>): ActOutcome = ActOutcome.Done
        override suspend fun answerForm(ask: String, values: List<FormValue>): ActOutcome = ActOutcome.Done
        override fun toggleRun(member: String) {}
        override fun toggleGroup(member: String) {}
        override fun foldTurns(on: Boolean) {}
        override fun toggleTurn(end: String) {}
        override suspend fun withdraw(inputId: List<Int>): ActOutcome = ActOutcome.Done
        override fun draftOf(inputId: List<Int>): Draft? = null
        override suspend fun sendNow(inputId: List<Int>): ActOutcome = ActOutcome.Done
        override suspend fun resend(inputId: List<Int>): SendOutcome? = null
        override fun discard(inputId: List<Int>) {}
        override suspend fun interrupt(): ActOutcome = ActOutcome.Done
        override suspend fun stopJob(key: String): ActOutcome = ActOutcome.Done
        override suspend fun changeSetting(setting: SettingChange): ActOutcome = ActOutcome.Done
        override suspend fun resume(draft: Draft): ActOutcome = ActOutcome.Done
        override suspend fun pageOlder(rows: Int): PageOutcome = PageOutcome.Arrived(0)
        override suspend fun putBlob(data: ByteArray, name: String, mime: String): Result<BlobRef> =
            Result.failure(RuntimeFailure("unused"))
        override fun blob(hash: List<Int>): ByteArray? = null
        override suspend fun review(comparison: Comparison): Result<FrozenReview> =
            byComparison[comparison] ?: Result.failure(RuntimeFailure("no machine"))
        override suspend fun openOverview(comparison: Comparison): Result<Overview> =
            Result.success(Overview(emptyList(), emptyList()))
    }

    @Test
    fun parsesPatchViaRustBridgeAndDisambiguatesFileLabels() {
        val model = ReviewModel(review = frozenWorking)

        assertEquals(
            listOf("src/pairing/errors.rs", "tests/errors.rs", "docs/PAIRING.md"),
            model.doc.files.map { it.path },
        )
        assertEquals(listOf(1, 1, 2), model.doc.files.map { it.added })
        assertEquals(listOf(2, 0, 0), model.doc.files.map { it.removed })
        assertEquals(FileStatus.Added, model.doc.files[2].status)
        assertEquals(
            listOf(LineKind.Context, LineKind.Removed, LineKind.Removed, LineKind.Added, LineKind.Context),
            model.doc.files[0].hunks[0].lines.map { it.kind },
        )
        assertEquals(
            listOf("pairing/errors.rs", "tests/errors.rs", "PAIRING.md"),
            (0..2).map(model::labelOfFile),
        )
    }

    @Test
    fun inlineLineCommentsFileCommentsRewriteAndRemoveUpdateDocumentAndPlacement() {
        val model = ReviewModel(review = frozenWorking)

        // Selection stays inside the file where it began.
        model.begin(ReviewLine(file = 0, hunk = 0, line = 0))
        model.extend(ReviewLine(file = 1, hunk = 0, line = 1))
        assertEquals(ReviewLine(0, 0, 0), model.selection?.endInclusive)
        model.extend(ReviewLine(file = 0, hunk = 0, line = 3))
        assertTrue(model.isSelected(ReviewLine(0, 0, 2)))
        assertFalse(model.canAttach)

        assertFalse("blank comment ignored", model.comment("   "))
        assertTrue(model.comment("Name the reason."))
        assertNull(model.selection)
        assertEquals(
            listOf(ReviewComment(path = "src/pairing/errors.rs", line = 11, oldLine = 0, text = "Name the reason.")),
            model.comments,
        )
        assertEquals(listOf("Name the reason."), model.doc.files[0].hunks[0].lines[3].comments)
        assertTrue(model.canAttach)

        // Dragging upward on removed lines targets the removed line's oldLine.
        model.begin(ReviewLine(file = 0, hunk = 0, line = 2))
        model.extend(ReviewLine(file = 0, hunk = 0, line = 1))
        assertTrue(model.comment("Unknown was never raised."))
        assertEquals(12, model.comments[1].oldLine)
        assertEquals(0, model.comments[1].line)
        assertEquals(listOf("Unknown was never raised."), model.doc.files[0].hunks[0].lines[2].comments)

        // File-level comment places with line = null.
        assertFalse(model.commentOnFile(2, "   "))
        assertTrue(model.commentOnFile(2, "Link the pairing table."))
        assertEquals(listOf(2), model.placed[CommentPlace(file = 2, line = null)])
        assertEquals(listOf("Link the pairing table."), model.doc.files[2].comments)

        // Rewrite & remove keep placement indices consistent.
        assertFalse(model.rewrite(1, "  "))
        assertTrue(model.rewrite(1, "Refused carries why."))
        assertEquals(listOf("Refused carries why."), model.doc.files[0].hunks[0].lines[2].comments)

        model.remove(0)
        assertEquals(listOf("Refused carries why.", "Link the pairing table."), model.comments.map { it.text })
        assertTrue(model.doc.files[0].hunks[0].lines[3].comments.isEmpty())
        assertEquals(listOf(1), model.placed[CommentPlace(file = 2, line = null)])
        assertEquals(1, model.countInFile(0))
        assertEquals(0, model.countInFile(1))
        assertEquals(1, model.countInFile(2))
    }

    @Test
    fun activeFileFoldingAndComparisonSwitchingWork() = runTest {
        val source = ReviewSource(
            mutableMapOf(
                Comparison.Uncommitted to Result.success(frozenWorking),
                Comparison.OnBranch to Result.success(frozenBranch),
            ),
        )
        val model = ReviewModel(review = frozenWorking, comparison = Comparison.Uncommitted)

        model.selectFile(2)
        assertEquals(2, model.activeFile)
        model.toggleFile(1)
        assertEquals(setOf(1), model.folded)
        model.toggleFile(1)
        assertTrue(model.folded.isEmpty())

        model.commentOnFile(0, "Check error naming")
        assertEquals(1, model.count)

        val switched = model.switchComparison(Comparison.OnBranch, source)
        assertTrue(switched.isSuccess)
        assertEquals(Comparison.OnBranch, model.comparison)
        assertEquals(listOf("README.md"), model.doc.files.map { it.path })
        assertEquals(0, model.activeFile)
        assertTrue("switching to a different patch resets comments", model.comments.isEmpty())
    }

    @Test
    fun chatModelOpensReusesAttachesAndValidatesReviewDiffs() = runTest {
        val source = ReviewSource(
            mutableMapOf(Comparison.Uncommitted to Result.success(frozenWorking)),
        )
        val chat = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

        val first = chat.openReview().await().getOrThrow()
        val second = chat.openReview().await().getOrThrow()
        assertNotNull(first)
        assertSame("same patch reuses in-progress review", first, second)

        first!!.begin(ReviewLine(0, 0, 1))
        first.comment("Keep the old name.")
        chat.attach(first)
        first.begin(ReviewLine(1, 0, 0))
        first.comment("Say why.")
        chat.attach(first)

        assertEquals("re-attaching replaces earlier token for same patch", 1, chat.attachments.size)
        val reviewToken = chat.attachments.single() as DraftAttachment.Review
        assertEquals(frozenWorking.diff, reviewToken.diff)
        assertEquals(listOf("Keep the old name.", "Say why."), reviewToken.comments.map { it.text })

        chat.send()!!.join()
        assertEquals(listOf(reviewToken), source.sent.single().attachments)

        // Empty diff with no listed files returns null ("no changes").
        source.byComparison[Comparison.Uncommitted] = Result.success(
            FrozenReview(diff = frozenWorking.diff.copy(patch = BlobRef(listOf(1), "p", "text/x-diff", 0L)), patch = ""),
        )
        val emptyResult = chat.openReview().await()
        assertTrue(emptyResult.isSuccess)
        assertNull(emptyResult.getOrThrow())

        // Empty patch when host listed files is a failure, never "no changes".
        val listedDiff = frozenWorking.diff.copy(
            files = listOf(DiffFile("src/pairing/errors.rs", 1, 2, 1, false)),
            patch = BlobRef(listOf(2), "p", "text/x-diff", 0L),
        )
        source.byComparison[Comparison.Uncommitted] = Result.success(FrozenReview(diff = listedDiff, patch = ""))
        val badResult = chat.openReview().await()
        assertTrue(badResult.isFailure)
        assertEquals(ReviewModel.MISSING_LISTED_FILES, badResult.exceptionOrNull()?.failureDescription)
    }

    @Test
    fun loadReviewDoesNotReuseExistingReviewWhenPatchHashIsNull() = runTest {
        val nullPatchWorking = FrozenReview(
            diff = frozenWorking.diff.copy(patch = null),
            patch = samplePatch,
        )
        val nullPatchBranch = FrozenReview(
            diff = frozenBranch.diff.copy(patch = null),
            patch = branchPatch,
        )
        val source = ReviewSource(
            mutableMapOf(
                Comparison.Uncommitted to Result.success(nullPatchWorking),
                Comparison.OnBranch to Result.success(nullPatchBranch),
            ),
        )
        val chat = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

        val first = chat.loadReview().getOrThrow()!!
        assertEquals(listOf("src/pairing/errors.rs", "tests/errors.rs", "docs/PAIRING.md"), first.doc.files.map { it.path })

        chat.compare(Comparison.OnBranch)
        val second = chat.loadReview().getOrThrow()!!
        assertFalse("null patch hashes must not falsely reuse previous review", first === second)
        assertEquals(listOf("README.md"), second.doc.files.map { it.path })
    }
}
