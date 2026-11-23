package sh.amux.core.runtime

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNotSame
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.ActOutcome
import sh.amux.values.BlobRef
import sh.amux.values.ChatDelta
import sh.amux.values.Comparison
import sh.amux.values.Draft
import sh.amux.values.FileStatus
import sh.amux.values.FormValue
import sh.amux.values.InputState
import sh.amux.values.KeysMoved
import sh.amux.values.LineKind
import sh.amux.values.Overview
import sh.amux.values.PageOutcome
import sh.amux.values.QuestionResponse
import sh.amux.values.ReviewAnchor
import sh.amux.values.ReviewBanner
import sh.amux.values.ReviewChange
import sh.amux.values.ReviewChoice
import sh.amux.values.ReviewCodeLine
import sh.amux.values.ReviewContextAsk
import sh.amux.values.ReviewFileHead
import sh.amux.values.ReviewHeader
import sh.amux.values.ReviewOutcome
import sh.amux.values.ReviewPage
import sh.amux.values.ReviewRow
import sh.amux.values.ReviewRowOf
import sh.amux.values.ReviewView
import sh.amux.values.ReviewViewTab
import sh.amux.values.Row
import sh.amux.values.SendOutcome
import sh.amux.values.SettingChange

/**
 * A chat whose agent has a review, as the runtime's review page answers:
 * what each act on the page was asked, the page open on the chat, and where
 * a selection of its rows would be commented on.
 */
internal class ReviewChat : ChatSource {
    /** The page the runtime holds open on the chat, null with none open. */
    var open: ReviewPage? = null

    /** What every act on the review answers. */
    var answering: ReviewOutcome? = ReviewOutcome.Done

    /** What `reviewFiles` answers, when it differs from the rest. */
    var filesAnswer: ReviewOutcome? = null

    /** Where a selection of rows would be commented on, by the indices asked; null for any other. */
    var anchors: Map<Pair<Int, Int>, ReviewAnchor> = emptyMap()
    val opens = mutableListOf<Pair<String?, Boolean>>()
    val fetches = mutableListOf<List<String>>()
    val contexts = mutableListOf<ReviewContextAsk>()
    val edits = mutableListOf<List<ReviewChange>>()
    val choices = mutableListOf<ReviewChoice>()
    val anchorsAsked = mutableListOf<Pair<Int, Int>>()
    var closes = 0

    override suspend fun openReview(baseBranch: String?, refresh: Boolean): ReviewOutcome? {
        opens += baseBranch to refresh
        return answering
    }

    override suspend fun reviewFiles(paths: List<String>): ReviewOutcome? {
        fetches += paths
        return filesAnswer ?: answering
    }

    override suspend fun reviewContext(ask: ReviewContextAsk): ReviewOutcome? {
        contexts += ask
        return answering
    }

    override suspend fun editReview(changes: List<ReviewChange>): ReviewOutcome? {
        edits += changes
        return answering
    }

    override fun chooseInReview(choice: ReviewChoice) {
        choices += choice
    }

    override fun reviewPage(): ReviewPage? = open

    override fun reviewAnchor(from: Int, to: Int): ReviewAnchor? {
        anchorsAsked += from to to
        return anchors[from to to]
    }

    override fun closeReview() {
        closes += 1
        open = null
    }

    // The chat around the review, which these tests do not read.

    override fun opening(): ChatDelta = ChatDelta(chat = 0L, rows = emptyList(), keys = KeysMoved.Reload(emptyList()), review = open)
    override fun follow(following: Boolean) {}
    override fun rowsFor(keys: List<String>): List<Row> = emptyList()
    override suspend fun send(draft: Draft): Result<SendOutcome> =
        Result.success(SendOutcome(inputId = listOf(1), state = InputState.Sent))
    override suspend fun answer(ask: String, choice: Int, note: String?): ActOutcome? = ActOutcome.Done
    override suspend fun answerQuestions(ask: String, responses: List<QuestionResponse>): ActOutcome? = ActOutcome.Done
    override suspend fun replyInstead(ask: String, text: String, soFar: List<QuestionResponse>): ActOutcome? =
        ActOutcome.Done
    override suspend fun answerForm(ask: String, values: List<FormValue>): ActOutcome? = ActOutcome.Done
    override fun toggleRun(member: String) {}
    override fun toggleGroup(member: String) {}
    override fun foldTurns(on: Boolean) {}
    override fun toggleTurn(end: String) {}
    override suspend fun withdraw(inputId: List<Int>): ActOutcome? = ActOutcome.Done
    override fun draftOf(inputId: List<Int>): Draft? = null
    override suspend fun sendNow(inputId: List<Int>): ActOutcome? = ActOutcome.Done
    override suspend fun resend(inputId: List<Int>): SendOutcome? = null
    override fun discard(inputId: List<Int>) {}
    override suspend fun interrupt(): ActOutcome? = ActOutcome.Done
    override suspend fun stopJob(key: String): ActOutcome? = ActOutcome.Done
    override suspend fun changeSetting(setting: SettingChange): ActOutcome? = ActOutcome.Done
    override suspend fun resume(draft: Draft): ActOutcome? = ActOutcome.Done
    override suspend fun pageOlder(rows: Int): PageOutcome? = PageOutcome.Arrived(0)
    override suspend fun putBlob(data: ByteArray, name: String, mime: String): Result<BlobRef> =
        Result.failure(RuntimeFailure("no blobs"))
    override fun blob(hash: List<Int>): ByteArray? = null
    override suspend fun openOverview(comparison: Comparison): Result<Overview> =
        Result.failure(RuntimeFailure("no overview"))
}

/** Review pages as the shared view composes them. */
internal object ReviewPages {
    const val SNAPSHOT = "5555555555555555555555555555555555555555"
    const val BASE = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

    fun header(snapshot: String = SNAPSHOT, view: ReviewView = ReviewView.All, drafts: Int = 0): ReviewHeader =
        ReviewHeader(
            baseBranch = "main", baseCommit = BASE, snapshot = snapshot, view = view, comparedWith = BASE,
            comparedBase = "", views = listOf(ReviewViewTab(view = ReviewView.All, files = 2)), rounds = 0,
            files = 2, added = 3, removed = 1, drafts = drafts, sent = 0, discardDrafts = drafts > 0, startOver = true,
        )

    fun file(path: String): ReviewRow = ReviewRow(
        key = "file:$path",
        of = ReviewRowOf.File(
            ReviewFileHead(
                path = path, status = FileStatus.Modified, added = 2, removed = 1, binary = false, language = "rust",
                flags = emptyList(), folded = false, changedSinceLast = false, inChange = true, comments = 0,
                outline = emptyList(), oldPath = null,
            ),
        ),
    )

    fun line(path: String, number: Int): ReviewRow = ReviewRow(
        key = "line:$path:$number",
        of = ReviewRowOf.Line(
            ReviewCodeLine(
                path = path, kind = LineKind.Added, text = "let x = $number;", runs = emptyList(),
                commented = false, extra = false, newLine = number, oldLine = null,
            ),
        ),
    )

    fun loading(path: String): ReviewRow = ReviewRow(key = "loading:$path", of = ReviewRowOf.Loading(path = path))

    fun page(
        rows: List<ReviewRow>,
        toFetch: List<String> = emptyList(),
        banners: List<ReviewBanner> = emptyList(),
        header: ReviewHeader? = header(),
    ): ReviewPage = ReviewPage(banners = banners, rows = rows, toFetch = toFetch, opening = false, empty = null, header = header)

    val anchor = ReviewAnchor(commit = SNAPSHOT, path = "src/lib.rs", start = 11, end = 12)
}

@OptIn(ExperimentalCoroutinesApi::class)
class ReviewSessionTest {
    private val p = ReviewPages

    private fun TestScope.session(chat: ReviewChat): ReviewSession = ReviewSession(source = chat, scope = this)

    private val ReviewSession.notice: String? get() = state.value.notice
    private val ReviewSession.selected: IntRange? get() = state.value.selected
    private val ReviewSession.anchor: ReviewAnchor? get() = state.value.anchor
    private val ReviewSession.waiting: Int get() = state.value.waiting

    @Test
    fun aPageStartsAsTheOneOpenOnTheChatOrOpening() = runTest {
        val none = session(ReviewChat())
        assertEquals(ReviewSession.OPENING, none.page)
        assertTrue(none.page.opening)
        assertNull(none.page.header)

        val chat = ReviewChat()
        val open = p.page(listOf(p.file("src/lib.rs")))
        chat.open = open
        assertEquals("a page already open is drawn at once", open, session(chat).page)
    }

    @Test
    fun openingRefreshingAndPickingABaseAskTheHost() = runTest {
        val chat = ReviewChat()
        val session = session(chat)
        session.open()?.join()
        session.refresh()?.join()
        session.open(base = "release")?.join()
        assertEquals(listOf(null, null, "release"), chat.opens.map { it.first })
        assertEquals(listOf(false, true, false), chat.opens.map { it.second })
        assertNull(session.notice)
        assertEquals(0, session.waiting)
    }

    /** The page is drawn as it was handed: a later page replaces it whole. */
    @Test
    fun thePageIsTheOneHandedOverLast() = runTest {
        val session = session(ReviewChat())
        val first = p.page(listOf(p.file("a.rs")))
        val second = p.page(listOf(p.file("a.rs"), p.line("a.rs", 1)), banners = listOf(ReviewBanner.ChangedSince))
        session.apply(first)
        assertEquals(first, session.page)
        session.apply(second)
        assertEquals(second, session.page)
    }

    /**
     * What the page still needs is asked for once, in page order, however
     * often the page moves before it lands; and asked again if the page still
     * lists it after the answer.
     */
    @Test
    fun theFilesThePageNeedsAreAskedForOnce() = runTest {
        val chat = ReviewChat()
        val session = session(chat)
        val waiting = p.page(
            listOf(p.file("a.rs"), p.loading("a.rs"), p.file("b.rs"), p.loading("b.rs")),
            toFetch = listOf("a.rs", "b.rs"),
        )
        session.apply(waiting)
        val asked = session.latest
        assertNotNull(asked)
        session.apply(waiting)
        assertSame("a page that moves before they land asks nothing twice", asked, session.latest)
        // The page that says they landed comes before the answer.
        session.apply(p.page(listOf(p.file("a.rs"), p.line("a.rs", 1), p.file("b.rs"), p.line("b.rs", 1))))
        asked?.join()
        assertEquals(listOf(listOf("a.rs", "b.rs")), chat.fetches)

        // They landed under another view than the page showed, so a page
        // handed over now still needs one: it is asked for again.
        session.apply(p.page(listOf(p.file("b.rs"), p.loading("b.rs")), toFetch = listOf("b.rs")))
        assertNotSame(asked, session.latest)
        session.latest?.join()
        assertEquals(listOf(listOf("a.rs", "b.rs"), listOf("b.rs")), chat.fetches)
    }

    @Test
    fun aNewSnapshotOrViewAsksForItsOwnBodies() = runTest {
        val chat = ReviewChat()
        val session = session(chat)
        // Failing, so nothing asked is let go.
        chat.filesAnswer = ReviewOutcome.Unreachable
        session.apply(p.page(listOf(p.file("a.rs")), toFetch = listOf("a.rs")))
        session.latest?.join()
        session.apply(p.page(listOf(p.file("a.rs")), toFetch = listOf("a.rs"), header = p.header(view = ReviewView.SinceLast)))
        session.latest?.join()
        session.apply(
            p.page(listOf(p.file("a.rs")), toFetch = listOf("a.rs"), header = p.header(snapshot = "6666666666666666666666666666666666666666")),
        )
        session.latest?.join()
        assertEquals(listOf(listOf("a.rs"), listOf("a.rs"), listOf("a.rs")), chat.fetches)
    }

    /**
     * A fetch that fails is not asked again by every page that follows, which
     * would ask a host out of reach in a loop: it is asked again when the host
     * answers again, or the page is opened again.
     */
    @Test
    fun aFailedFetchIsAskedAgainOnlyWhenItCanAnswer() = runTest {
        val chat = ReviewChat()
        val session = session(chat)
        chat.filesAnswer = ReviewOutcome.Unreachable
        val needing = p.page(listOf(p.file("a.rs"), p.loading("a.rs")), toFetch = listOf("a.rs"))
        session.apply(needing)
        session.latest?.join()
        val first = session.latest
        session.apply(p.page(needing.rows, toFetch = listOf("a.rs"), banners = listOf(ReviewBanner.Unreachable)))
        session.apply(p.page(needing.rows, toFetch = listOf("a.rs"), banners = listOf(ReviewBanner.Unreachable)))
        assertSame("a failing host is not asked in a loop", first, session.latest)
        assertEquals(1, chat.fetches.size)
        assertNull("the page's banner says the host is out of reach", session.notice)

        chat.filesAnswer = null
        session.apply(needing)
        session.latest?.join()
        assertEquals("the host answers again, so the file is asked for again", 2, chat.fetches.size)

        chat.filesAnswer = ReviewOutcome.Unreachable
        val elsewhere = p.page(needing.rows, toFetch = listOf("a.rs"), header = p.header(snapshot = "77"))
        session.apply(elsewhere)
        session.latest?.join()
        session.apply(elsewhere)
        session.latest?.join()
        assertEquals(3, chat.fetches.size)
        session.open()?.join()
        session.apply(elsewhere)
        session.latest?.join()
        assertEquals("opening the page again asks again", 4, chat.fetches.size)
    }

    @Test
    fun aFetchThatFailsInWordsSaysWhy() = runTest {
        val chat = ReviewChat()
        chat.filesAnswer = ReviewOutcome.Failed("the agent's host answered none of the files asked for")
        val session = session(chat)
        session.apply(p.page(listOf(p.file("a.rs")), toFetch = listOf("a.rs")))
        session.latest?.join()
        assertEquals("the agent's host answered none of the files asked for", session.notice)
    }

    @Test
    fun moreContextAsksForTheGapsLines() = runTest {
        val chat = ReviewChat()
        val session = session(chat)
        val ask = ReviewContextAsk(path = "a.rs", commit = p.SNAPSHOT, start = 4, end = 24, definition = "fn open")
        session.moreContext(ask)?.join()
        assertEquals(listOf(ask), chat.contexts)
    }

    @Test
    fun choicesGoToTheRuntimeWhichDrawsThem() = runTest {
        val chat = ReviewChat()
        val session = session(chat)
        session.show(ReviewView.SinceLast)
        session.toggleFile("Cargo.lock")
        session.toggleComment(listOf(7))
        assertEquals(
            listOf(
                ReviewChoice.View(ReviewView.SinceLast),
                ReviewChoice.ToggleFile("Cargo.lock"),
                ReviewChoice.ToggleComment(listOf(7)),
            ),
            chat.choices,
        )
        assertEquals("nothing is drawn here until the page is handed over", ReviewSession.OPENING, session.page)
    }

    /**
     * A selection is rows of the page, turned into lines by the runtime
     * against the page it handed over; as the page moves, the selection stays
     * on its rows.
     */
    @Test
    fun aSelectionStaysOnItsRowsAsThePageMoves() = runTest {
        val chat = ReviewChat()
        val session = session(chat)
        session.apply(p.page(listOf(p.file("src/lib.rs"), p.line("src/lib.rs", 11), p.line("src/lib.rs", 12))))
        chat.anchors = mapOf((2 to 1) to p.anchor, (3 to 2) to p.anchor)
        session.select(2)
        assertEquals(2..2, session.selected)
        assertNull(session.anchor)
        session.extend(1)
        assertEquals(1..2, session.selected)
        assertEquals(p.anchor, session.anchor)
        assertTrue(session.state.value.canComment)
        assertTrue(session.state.value.isSelected(1))
        assertFalse(session.state.value.isSelected(0))

        // A row above: the same rows, one further down.
        session.apply(
            p.page(listOf(p.file("other.rs"), p.file("src/lib.rs"), p.line("src/lib.rs", 11), p.line("src/lib.rs", 12))),
        )
        assertEquals(2..3, session.selected)
        assertEquals("the runtime is asked again for the page it handed", 3 to 2, chat.anchorsAsked.last())
        assertEquals(p.anchor, session.anchor)

        // A selected row leaves the page: the selection goes with it.
        session.apply(p.page(listOf(p.file("other.rs"), p.file("src/lib.rs"), p.line("src/lib.rs", 11))))
        assertNull(session.selected)
        assertNull(session.anchor)
        assertFalse(session.state.value.canComment)

        session.select(99)
        assertNull("a row the page does not hold selects nothing", session.selected)
        session.extend(1)
        assertNull("nothing is extended without a start", session.selected)
    }

    @Test
    fun aCommentGoesOnTheSelectionsLinesAndEndsIt() = runTest {
        val chat = ReviewChat()
        val session = session(chat)
        session.apply(p.page(listOf(p.file("src/lib.rs"), p.line("src/lib.rs", 11))))
        assertNull("with no selection nothing is written", session.comment("No lines selected."))
        chat.anchors = mapOf((1 to 1) to p.anchor)
        session.select(1)
        assertNull("blank words write nothing", session.comment("  \n "))
        val written = session.comment("  Say why.\n")
        assertEquals(1, session.waiting)
        written?.join()
        assertEquals(listOf(listOf(ReviewChange.Comment(anchor = p.anchor, text = "Say why."))), chat.edits)
        assertNull("the host has it, so the selection ends", session.selected)
        assertEquals(0, session.waiting)
    }

    @Test
    fun aCommentTheHostDidNotTakeKeepsTheSelectionAndSaysWhy() = runTest {
        val chat = ReviewChat()
        val session = session(chat)
        session.apply(p.page(listOf(p.file("src/lib.rs"), p.line("src/lib.rs", 11))))
        chat.anchors = mapOf((1 to 1) to p.anchor)
        session.select(1)
        chat.answering = ReviewOutcome.Unreachable
        session.comment("Say why.")?.join()
        assertEquals(1..1, session.selected)
        assertEquals("This agent’s computer can’t be reached, so nothing changed.", session.notice)

        chat.answering = ReviewOutcome.Failed("the review holds 500 comments already")
        session.comment("Say why.")?.join()
        assertEquals("the review holds 500 comments already", session.notice)

        chat.answering = ReviewOutcome.Done
        session.comment("Say why.")?.join()
        assertNull("the next act clears what the last said", session.notice)
    }

    /** Opening, refreshing and more context change nothing at the host: a host out of reach is the page's banner to say. */
    @Test
    fun readingFromAHostOutOfReachLeavesItToTheBanner() = runTest {
        val chat = ReviewChat()
        chat.answering = ReviewOutcome.Unreachable
        val session = session(chat)
        session.open()?.join()
        assertNull(session.notice)
        chat.answering = ReviewOutcome.Unavailable
        session.refresh()?.join()
        assertNull("the page says why there is no review", session.notice)
        chat.answering = ReviewOutcome.NotConfirmed
        session.open()?.join()
        assertEquals(
            "The connection dropped before the agent’s computer answered. The page shows what it says next.",
            session.notice,
        )
    }

    @Test
    fun rewordingDeletingDiscardingAndStartingOverChangeTheReview() = runTest {
        val chat = ReviewChat()
        val session = session(chat)
        assertNull("blank words change nothing", session.reword(listOf(1), "  "))
        session.reword(listOf(1), " Sharper words. ")?.join()
        session.delete(listOf(2))?.join()
        session.discardDrafts()?.join()
        session.startOver()?.join()
        assertEquals(
            listOf(
                listOf(ReviewChange.Reword(id = listOf(1), text = "Sharper words.")),
                listOf(ReviewChange.Delete(listOf(2))),
                listOf(ReviewChange.DiscardDrafts),
                listOf(ReviewChange.StartOver),
            ),
            chat.edits,
        )
    }

    @Test
    fun aClosedPageAsksTheHostNothingMore() = runTest {
        val chat = ReviewChat()
        val session = session(chat)
        session.apply(p.page(listOf(p.file("src/lib.rs"), p.line("src/lib.rs", 11))))
        chat.anchors = mapOf((1 to 1) to p.anchor)
        session.select(1)
        session.close()
        session.close()
        assertEquals(1, chat.closes)
        assertTrue(session.closed)
        assertNull(session.selected)
        assertFalse(session.state.value.canComment)
        assertNull(session.open())
        assertNull(session.discardDrafts())
        session.show(ReviewView.All)
        session.apply(p.page(listOf(p.file("a.rs")), toFetch = listOf("a.rs")))
        runCurrent()
        assertTrue(chat.choices.isEmpty())
        assertTrue(chat.fetches.isEmpty())
        assertTrue(chat.opens.isEmpty())
    }

    /** A chat closed under the page answers nothing, which the page says rather than swallowing. */
    @Test
    fun anActOnAClosedChatSaysSo() = runTest {
        val chat = ReviewChat()
        chat.answering = null
        val session = session(chat)
        session.startOver()?.join()
        assertEquals("This chat is closed.", session.notice)
    }
}
