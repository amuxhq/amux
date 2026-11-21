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
import sh.amux.values.Row
import sh.amux.values.SendOutcome
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
