package sh.amux.core.runtime

import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock
import kotlinx.coroutines.CancellationException
import kotlinx.serialization.encodeToString
import sh.amux.values.ActOutcome
import sh.amux.values.BlobRef
import sh.amux.values.ChatAct
import sh.amux.values.ChatDelta
import sh.amux.values.CommandOutcome
import sh.amux.values.Comparison
import sh.amux.values.Draft
import sh.amux.values.FormValue
import sh.amux.values.KeysMoved
import sh.amux.values.Overview
import sh.amux.values.PageOutcome
import sh.amux.values.QuestionResponse
import sh.amux.values.ReviewAnchor
import sh.amux.values.ReviewChange
import sh.amux.values.ReviewChoice
import sh.amux.values.ReviewContextAsk
import sh.amux.values.ReviewOutcome
import sh.amux.values.ReviewPage
import sh.amux.values.Row
import sh.amux.values.SendOutcome
import sh.amux.values.SentReviewRef
import sh.amux.values.SettingChange

/**
 * What a chat screen reads and acts on: either a live [Chat] on the embedded
 * runtime or a scripted test double.
 */
interface ChatSource {
    fun opening(): ChatDelta
    fun follow(following: Boolean)
    fun rowsFor(keys: List<String>): List<Row>
    suspend fun send(draft: Draft): Result<SendOutcome>
    suspend fun answer(ask: String, choice: Int, note: String? = null): ActOutcome?
    suspend fun answerQuestions(ask: String, responses: List<QuestionResponse>): ActOutcome?
    suspend fun replyInstead(ask: String, text: String, soFar: List<QuestionResponse>): ActOutcome?
    suspend fun answerForm(ask: String, values: List<FormValue>): ActOutcome?
    fun toggleRun(member: String)
    fun toggleGroup(member: String)
    fun foldTurns(on: Boolean)
    fun toggleTurn(end: String)
    suspend fun withdraw(inputId: List<Int>): ActOutcome?
    fun draftOf(inputId: List<Int>): Draft?
    suspend fun sendNow(inputId: List<Int>): ActOutcome?
    suspend fun resend(inputId: List<Int>): SendOutcome?
    fun discard(inputId: List<Int>)
    suspend fun interrupt(): ActOutcome?
    suspend fun stopJob(key: String): ActOutcome?
    suspend fun changeSetting(setting: SettingChange): ActOutcome?
    suspend fun resume(draft: Draft): ActOutcome?
    suspend fun pageOlder(rows: Int): PageOutcome?
    suspend fun putBlob(data: ByteArray, name: String, mime: String): Result<BlobRef>
    fun blob(hash: List<Int>): ByteArray?
    fun blobFailed(hash: List<Int>): Boolean = false
    fun fetchBlob(hash: List<Int>) {}
    suspend fun openOverview(comparison: Comparison): Result<Overview>
    suspend fun command(draft: Draft): CommandOutcome? = CommandOutcome.Send
    suspend fun switchBack(): ActOutcome? = ActOutcome.Done
    fun keptDraft(): Draft? = null
    fun keep(draft: Draft) {}

    // The agent's review, read and changed at the agent's host. The page
    // open on the chat moves in the deltas' `review`. A source whose agent
    // has no review (its host offers none) keeps these defaults: there is
    // never a page, a chip or drafts to send.

    /**
     * Sends the draft with the review's drafts after its words, naming
     * exactly those comments; nothing goes when the review holds none.
     */
    suspend fun sendWithReview(draft: Draft): Result<SendOutcome> =
        Result.failure(RuntimeFailure("This agent has no review to send."))

    /**
     * Opens the review page on the chat, or opens the open one again:
     * [baseBranch] picks the base anew, [refresh] takes a snapshot even with
     * drafts, moving them onto it.
     */
    suspend fun openReview(baseBranch: String?, refresh: Boolean): ReviewOutcome? = ReviewOutcome.Unavailable

    /** Fetches these files' bodies for the view the page shows. */
    suspend fun reviewFiles(paths: List<String>): ReviewOutcome? = ReviewOutcome.Unavailable

    /** Loads the lines one of a gap's asks names. */
    suspend fun reviewContext(ask: ReviewContextAsk): ReviewOutcome? = ReviewOutcome.Unavailable

    /** Makes changes to the review, all or none; with no page open too. */
    suspend fun editReview(changes: List<ReviewChange>): ReviewOutcome? = ReviewOutcome.Unavailable

    /** A choice made on the page; the next delta carries the page drawn so. */
    fun chooseInReview(choice: ReviewChoice) {}

    /** The page open on the chat, as handed over from now; null with none. */
    fun reviewPage(): ReviewPage? = null

    /**
     * The lines a selection of the handed page's rows, from one index to
     * another, would be commented on; null for a selection with no line or
     * across files.
     */
    fun reviewAnchor(from: Int, to: Int): ReviewAnchor? = null

    /** Closes the page: no delta carries it again. */
    fun closeReview() {}

    /**
     * The page of the round a review sent into the chat closed, read from
     * the agent's host apart from any page open on the chat; null once the
     * host holds none of it, when the text the agent read is what shows.
     */
    suspend fun reviewRound(sent: SentReviewRef): ReviewPage? = null
}

/** An open chat session tracked by [StoreBundle]. */
interface OpenChat : ChatSource, AutoCloseable {
    val id: Long
}

/** One open chat session backed by the embedded Rust runtime. */
class Chat internal constructor(
    private val handle: Long,
    private val loop: CoreLoop,
) : OpenChat {
    override val id: Long = NativeBridge.sessionId(handle)

    private val lock = ReentrantLock()
    private var open = true

    override fun close() {
        val shouldClose = lock.withLock {
            if (!open) return
            open = false
            true
        }
        if (shouldClose) {
            NativeBridge.sessionClose(handle)
        }
    }

    private inline fun <T> withOpen(closed: T, body: (Long) -> T): T = lock.withLock {
        if (open) body(handle) else closed
    }

    override fun opening(): ChatDelta {
        val empty = ChatDelta(
            chat = id,
            rows = emptyList(),
            agents = null,
            ask = null,
            frame = null,
            keys = KeysMoved.Reload(emptyList()),
            overview = null,
            settings = null,
        )
        return withOpen(empty) { live ->
            NativeBridge.sessionOpening(live)?.let { Bridge.decodeNullable<ChatDelta>(it) } ?: empty
        }
    }

    override fun follow(following: Boolean) {
        withOpen(Unit) { live -> NativeBridge.sessionFollow(live, following) }
    }

    override fun keptDraft(): Draft? = withOpen(null) { live ->
        NativeBridge.sessionKeptDraft(live)?.let { Bridge.decodeNullable<Draft>(it) }
    }

    override fun keep(draft: Draft) {
        val draftJson = AmuxJson.json.encodeToString(draft)
        withOpen(Unit) { live -> NativeBridge.sessionKeepDraft(live, draftJson) }
    }

    override fun rowsFor(keys: List<String>): List<Row> {
        val keysJson = AmuxJson.json.encodeToString(keys)
        return withOpen(emptyList()) { live ->
            NativeBridge.sessionRowsFor(live, keysJson)?.let { Bridge.decodeNullable<List<Row>>(it) }
                ?: emptyList()
        }
    }

    override fun toggleRun(member: String) {
        withOpen(Unit) { live -> NativeBridge.sessionToggleRun(live, member) }
    }

    override fun toggleGroup(member: String) {
        withOpen(Unit) { live -> NativeBridge.sessionToggleGroup(live, member) }
    }

    override fun foldTurns(on: Boolean) {
        withOpen(Unit) { live -> NativeBridge.sessionFoldTurns(live, on) }
    }

    override fun toggleTurn(end: String) {
        withOpen(Unit) { live -> NativeBridge.sessionToggleTurn(live, end) }
    }

    override suspend fun send(draft: Draft): Result<SendOutcome> {
        val actJson = AmuxJson.json.encodeToString<ChatAct>(ChatAct.Send(draft))
        return act { live -> NativeBridge.sessionAct(live, actJson) }
    }

    override suspend fun answer(ask: String, choice: Int, note: String?): ActOutcome? =
        valueAct(ChatAct.Answer(ask = ask, choice = choice, note = note.orEmpty()))

    override suspend fun answerQuestions(
        ask: String,
        responses: List<QuestionResponse>,
    ): ActOutcome? = valueAct(ChatAct.AnswerQuestions(ask = ask, responses = responses))

    override suspend fun replyInstead(
        ask: String,
        text: String,
        soFar: List<QuestionResponse>,
    ): ActOutcome? = valueAct(ChatAct.ReplyInstead(ask = ask, reply = text, soFar = soFar))

    override suspend fun answerForm(ask: String, values: List<FormValue>): ActOutcome? =
        valueAct(ChatAct.AnswerForm(ask = ask, values = values))

    override suspend fun withdraw(inputId: List<Int>): ActOutcome? =
        valueAct(ChatAct.Withdraw(inputId))

    override suspend fun sendNow(inputId: List<Int>): ActOutcome? =
        valueAct(ChatAct.SendNow(inputId))

    override suspend fun resend(inputId: List<Int>): SendOutcome? =
        valueAct(ChatAct.Resend(inputId))

    override fun draftOf(inputId: List<Int>): Draft? {
        val inputJson = AmuxJson.json.encodeToString(inputId)
        return withOpen(null) { live ->
            NativeBridge.sessionDraftOf(live, inputJson)?.let { Bridge.decodeNullable<Draft>(it) }
        }
    }

    override fun discard(inputId: List<Int>) {
        val inputJson = AmuxJson.json.encodeToString(inputId)
        withOpen(Unit) { live -> NativeBridge.sessionDiscard(live, inputJson) }
    }

    override suspend fun interrupt(): ActOutcome? = valueAct(ChatAct.Interrupt)

    override suspend fun stopJob(key: String): ActOutcome? = valueAct(ChatAct.StopJob(key))

    override suspend fun switchBack(): ActOutcome? = valueAct(ChatAct.SwitchBack)

    override suspend fun command(draft: Draft): CommandOutcome? =
        valueAct(ChatAct.Command(draft))

    override suspend fun changeSetting(setting: SettingChange): ActOutcome? =
        valueAct(ChatAct.ChangeSetting(setting))

    override suspend fun resume(draft: Draft): ActOutcome? = valueAct(ChatAct.Resume(draft))

    override suspend fun pageOlder(rows: Int): PageOutcome? = valueAct(ChatAct.PageOlder(rows))

    override suspend fun openOverview(comparison: Comparison): Result<Overview> {
        val compJson = AmuxJson.json.encodeToString(comparison)
        return act { live -> NativeBridge.sessionOpenOverview(live, compJson) }
    }

    override suspend fun putBlob(data: ByteArray, name: String, mime: String): Result<BlobRef> =
        act { live -> NativeBridge.sessionPutBlob(live, data, name, mime) }

    override fun blob(hash: List<Int>): ByteArray? {
        val hashJson = AmuxJson.json.encodeToString(hash)
        return withOpen(null) { live -> NativeBridge.sessionBlob(live, hashJson) }
    }

    override fun blobFailed(hash: List<Int>): Boolean {
        val hashJson = AmuxJson.json.encodeToString(hash)
        return withOpen(false) { live -> NativeBridge.sessionBlobFailed(live, hashJson) }
    }

    override fun fetchBlob(hash: List<Int>) {
        val hashJson = AmuxJson.json.encodeToString(hash)
        withOpen(Unit) { live -> NativeBridge.sessionFetchBlob(live, hashJson) }
    }

    override suspend fun sendWithReview(draft: Draft): Result<SendOutcome> {
        val actJson = AmuxJson.json.encodeToString<ChatAct>(ChatAct.SendWithReview(draft))
        return act { live -> NativeBridge.sessionAct(live, actJson) }
    }

    override suspend fun openReview(baseBranch: String?, refresh: Boolean): ReviewOutcome? =
        reviewAct { live -> NativeBridge.reviewOpen(live, baseBranch, refresh) }

    override suspend fun reviewFiles(paths: List<String>): ReviewOutcome? {
        val pathsJson = AmuxJson.json.encodeToString(paths)
        return reviewAct { live -> NativeBridge.reviewFiles(live, pathsJson) }
    }

    override suspend fun reviewContext(ask: ReviewContextAsk): ReviewOutcome? {
        val askJson = AmuxJson.json.encodeToString(ask)
        return reviewAct { live -> NativeBridge.reviewContext(live, askJson) }
    }

    override suspend fun editReview(changes: List<ReviewChange>): ReviewOutcome? {
        val changesJson = AmuxJson.json.encodeToString(changes)
        return reviewAct { live -> NativeBridge.reviewEdit(live, changesJson) }
    }

    override fun chooseInReview(choice: ReviewChoice) {
        val choiceJson = AmuxJson.json.encodeToString(choice)
        withOpen(Unit) { live -> NativeBridge.reviewChoose(live, choiceJson) }
    }

    override fun reviewPage(): ReviewPage? = withOpen(null) { live ->
        NativeBridge.reviewPage(live)?.let { Bridge.decodeNullable<ReviewPage>(it) }
    }

    override fun reviewAnchor(from: Int, to: Int): ReviewAnchor? {
        if (from < 0 || to < 0) return null
        return withOpen(null) { live ->
            NativeBridge.reviewAnchor(live, from, to)?.let { Bridge.decodeNullable<ReviewAnchor>(it) }
        }
    }

    override fun closeReview() {
        withOpen(Unit) { live -> NativeBridge.reviewClose(live) }
    }

    override suspend fun reviewRound(sent: SentReviewRef): ReviewPage? {
        val sentJson = AmuxJson.json.encodeToString(sent)
        return reviewValue { live -> NativeBridge.reviewRound(live, sentJson) }
    }

    /** A review act's `ReviewOutcome`; null once the chat or the runtime has closed. */
    private suspend fun reviewAct(start: (Long) -> Long): ReviewOutcome? = reviewValue(start)

    private suspend inline fun <reified T> reviewValue(noinline start: (Long) -> Long): T? {
        val raw = try {
            loop.awaitAct { withOpen(0L, start) }
        } catch (c: CancellationException) {
            throw c
        } catch (_: Throwable) {
            return null
        }
        if (Bridge.isErrAnswer(raw)) return null
        return Bridge.decodeNullable<T>(raw)
    }

    private suspend inline fun <reified T> act(
        noinline start: (Long) -> Long,
    ): Result<T> {
        val raw = try {
            loop.awaitAct { withOpen(0L, start) }
        } catch (c: CancellationException) {
            throw c
        } catch (t: Throwable) {
            return Result.failure(RuntimeFailure(t.failureDescription))
        }
        return Bridge.decodeAnswer(raw)
    }

    private suspend inline fun <reified T> valueAct(act: ChatAct): T? {
        val actJson = AmuxJson.json.encodeToString<ChatAct>(act)
        val raw = try {
            loop.awaitAct { withOpen(0L) { live -> NativeBridge.sessionAct(live, actJson) } }
        } catch (c: CancellationException) {
            throw c
        } catch (_: Throwable) {
            return null
        }
        // A closed chat or stopped runtime answers an error, which callers
        // hear as null: the chat is closed.
        if (Bridge.isErrAnswer(raw)) return null
        return Bridge.decodeNullable<T>(raw)
    }
}
