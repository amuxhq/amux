package sh.amux.core.runtime

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import java.io.ByteArrayOutputStream
import java.io.File
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import sh.amux.values.ReportSent

/** Frozen picture of the screen a report is about, with logical dimensions. */
data class FrozenFrame(
    val png: ByteArray,
    val widthDp: Double,
    val heightDp: Double,
    val scale: Double,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is FrozenFrame) return false
        return png.contentEquals(other.png) &&
            widthDp == other.widthDp &&
            heightDp == other.heightDp &&
            scale == other.scale
    }

    override fun hashCode(): Int {
        var result = png.contentHashCode()
        result = 31 * result + widthDp.hashCode()
        result = 31 * result + heightDp.hashCode()
        result = 31 * result + scale.hashCode()
        return result
    }
}

/** Why a part of a report is absent. */
class PartAbsent(val why: String) : Exception(why) {
    override fun equals(other: Any?): Boolean =
        this === other || (other is PartAbsent && why == other.why)

    override fun hashCode(): Int = why.hashCode()
}

/** Everything frozen at the instant a screenshot or report was started. */
data class ReportCapture(
    val frame: FrozenFrame,
    val dump: Deferred<Result<File>>? = null,
    val route: String? = null,
    val runtimeFailure: String? = null,
    val account: AccountId? = null,
    val profileId: String? = null,
    val framePng: Deferred<ByteArray>? = null,
)

/** One rectangle drawn on the frozen frame with an optional note and redaction flag. */
@Serializable
data class ReportMark(
    val x: Double,
    val y: Double,
    val width: Double,
    val height: Double,
    val note: String = "",
    val redact: Boolean = false,
)

/**
 * Produces the uploaded PNG bytes with any redacted [ReportMark] rectangles
 * obscured by opaque fills.
 */
fun interface ReportImageRedactor {
    suspend fun redact(frame: FrozenFrame, marks: List<ReportMark>): ByteArray
}

/**
 * Default Android [ReportImageRedactor] that burns solid black rectangles over
 * every redacted [ReportMark] on a background dispatcher.
 */
object BitmapReportImageRedactor : ReportImageRedactor {
    override suspend fun redact(frame: FrozenFrame, marks: List<ReportMark>): ByteArray {
        val redactedMarks = marks.filter { it.redact }
        if (redactedMarks.isEmpty() || frame.png.isEmpty()) return frame.png
        return withContext(Dispatchers.Default) {
            val decoded = BitmapFactory.decodeByteArray(frame.png, 0, frame.png.size)
                ?: throw IllegalStateException("Could not decode screenshot to apply redactions.")
            val mutable = try {
                decoded.copy(Bitmap.Config.ARGB_8888, true)
                    ?: throw IllegalStateException("Could not allocate bitmap to apply redactions.")
            } finally {
                decoded.recycle()
            }
            try {
                val canvas = Canvas(mutable)
                val paint = Paint().apply {
                    style = Paint.Style.FILL
                    color = Color.BLACK
                    isAntiAlias = false
                }
                val scaleX = if (frame.widthDp > 0.0) mutable.width / frame.widthDp else frame.scale
                val scaleY = if (frame.heightDp > 0.0) mutable.height / frame.heightDp else frame.scale
                val maxW = mutable.width.toFloat()
                val maxH = mutable.height.toFloat()
                for (mark in redactedMarks) {
                    val left = (mark.x * scaleX).toFloat().coerceIn(0f, maxW)
                    val top = (mark.y * scaleY).toFloat().coerceIn(0f, maxH)
                    val right = ((mark.x + mark.width) * scaleX).toFloat().coerceIn(0f, maxW)
                    val bottom = ((mark.y + mark.height) * scaleY).toFloat().coerceIn(0f, maxH)
                    if (right > left && bottom > top) {
                        canvas.drawRect(left, top, right, bottom, paint)
                    }
                }
                ByteArrayOutputStream().use { out ->
                    if (!mutable.compress(Bitmap.CompressFormat.PNG, 100, out)) {
                        throw IllegalStateException("Could not encode redacted screenshot.")
                    }
                    out.toByteArray()
                }
            } finally {
                mutable.recycle()
            }
        }
    }
}

/** What the person has written about the frozen frame so far. */
data class ReportDraft(
    val note: String = "",
    val marks: List<ReportMark> = emptyList(),
)

/** Receipt returned by the account service once a report is accepted. */
@Serializable
data class ReportReceipt(
    val id: String,
    val receivedAtMs: Long,
)

/** Delivery state for the active report. */
sealed interface ReportSending {
    data object Ready : ReportSending
    data object Sending : ReportSending
    data class Failed(val reason: String) : ReportSending
    data class Sent(val receipt: ReportReceipt) : ReportSending
}

/** Sealed report bytes handed to [Profile.submitReport]. */
data class ReportUpload(
    val header: ByteArray,
    val frame: ByteArray,
    val path: String,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ReportUpload) return false
        return header.contentEquals(other.header) &&
            frame.contentEquals(other.frame) &&
            path == other.path
    }

    override fun hashCode(): Int {
        var result = header.contentHashCode()
        result = 31 * result + frame.contentHashCode()
        result = 31 * result + path.hashCode()
        return result
    }
}

/** Seam for submitting reports (`Profile` in production, fake in tests). */
fun interface ReportSubmitting {
    suspend fun submitReport(upload: ReportUpload): Result<ReportSent>
}

/** Seam for freezing the active screen and starting its diagnostic dump. */
fun interface ReportFreezing {
    fun freeze(): ReportCapture?
}

/** Resolved availability of the frozen screenshot attached to a report. */
enum class ReportScreenshotState {
    Pending,
    Present,
    Absent,
}

/** Resolved availability of the diagnostic dump attached to a report. */
enum class ReportDumpState {
    Pending,
    Ready,
    Absent,
}

data class ReportState(
    val capture: ReportCapture? = null,
    val offering: Boolean = false,
    val open: Boolean = false,
    val draft: ReportDraft = ReportDraft(),
    val sending: ReportSending = ReportSending.Ready,
    val sealed: ReportUpload? = null,
    val screenshotState: ReportScreenshotState = ReportScreenshotState.Absent,
    val dumpState: ReportDumpState = ReportDumpState.Absent,
)

/**
 * Holds the single in-progress problem report across screenshot preview
 * transitions and retries.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ReportStore(
    initial: ReportState = ReportState(),
) {
    private val stateLock = Any()

    private val _state: MutableStateFlow<ReportState>
    val state: StateFlow<ReportState>

    private var captureId: UUID = UUID.randomUUID()

    init {
        val normalized = if (initial.capture != null) {
            val resolved = resolveInitialCapture(initial.capture)
            initial.copy(
                capture = resolved.capture,
                open = initial.open,
                screenshotState = resolved.screenshotState,
                dumpState = resolved.dumpState,
            )
        } else {
            initial.copy(
                open = false,
                screenshotState = ReportScreenshotState.Absent,
                dumpState = ReportDumpState.Absent,
            )
        }
        _state = MutableStateFlow(normalized)
        state = _state.asStateFlow()
        normalized.capture?.let { observeCapture(it, normalized.screenshotState, normalized.dumpState) }
    }

    private data class ResolvedCapture(
        val capture: ReportCapture,
        val screenshotState: ReportScreenshotState,
        val dumpState: ReportDumpState,
    )

    private fun resolveInitialCapture(capture: ReportCapture): ResolvedCapture {
        var resolvedCapture = capture
        val screenshotState = when {
            capture.frame.png.isNotEmpty() -> ReportScreenshotState.Present
            capture.framePng == null -> ReportScreenshotState.Absent
            capture.framePng.isCompleted -> {
                val png = if (!capture.framePng.isCancelled && capture.framePng.getCompletionExceptionOrNull() == null) {
                    runCatching { capture.framePng.getCompleted() }.getOrDefault(ByteArray(0))
                } else {
                    ByteArray(0)
                }
                if (png.isNotEmpty()) {
                    resolvedCapture = capture.copy(frame = capture.frame.copy(png = png))
                    ReportScreenshotState.Present
                } else {
                    ReportScreenshotState.Absent
                }
            }
            else -> ReportScreenshotState.Pending
        }
        val dumpState = when {
            capture.dump == null -> ReportDumpState.Absent
            capture.dump.isCompleted -> {
                val ok = !capture.dump.isCancelled &&
                    capture.dump.getCompletionExceptionOrNull() == null &&
                    runCatching { capture.dump.getCompleted().isSuccess }.getOrDefault(false)
                if (ok) ReportDumpState.Ready else ReportDumpState.Absent
            }
            else -> ReportDumpState.Pending
        }
        return ResolvedCapture(
            capture = resolvedCapture,
            screenshotState = screenshotState,
            dumpState = dumpState,
        )
    }

    private fun observeCapture(
        capture: ReportCapture,
        screenshotState: ReportScreenshotState,
        dumpState: ReportDumpState,
    ) {
        if (screenshotState == ReportScreenshotState.Pending) {
            val frameDeferred = capture.framePng
            frameDeferred?.invokeOnCompletion { cause ->
                val png = if (cause == null && !frameDeferred.isCancelled) {
                    runCatching { frameDeferred.getCompleted() }.getOrDefault(ByteArray(0))
                } else {
                    ByteArray(0)
                }
                updateFramePng(capture, png)
            }
        }
        if (dumpState == ReportDumpState.Pending) {
            val dumpDeferred = capture.dump
            dumpDeferred?.invokeOnCompletion { cause ->
                val ready = if (cause == null && !dumpDeferred.isCancelled) {
                    runCatching { dumpDeferred.getCompleted().isSuccess }.getOrDefault(false)
                } else {
                    false
                }
                updateDumpState(capture, ready)
            }
        }
    }

    fun offer(freezer: ReportFreezing): Boolean {
        val current = synchronized(stateLock) { _state.value }
        if (current.open || current.capture != null) return false
        val frozen = freezer.freeze() ?: return false
        val resolved = resolveInitialCapture(frozen)
        val installed = synchronized(stateLock) {
            val latest = _state.value
            if (latest.open || latest.capture != null) {
                false
            } else {
                captureId = UUID.randomUUID()
                _state.value = latest.copy(
                    capture = resolved.capture,
                    offering = true,
                    screenshotState = resolved.screenshotState,
                    dumpState = resolved.dumpState,
                )
                true
            }
        }
        if (installed) {
            observeCapture(resolved.capture, resolved.screenshotState, resolved.dumpState)
        }
        return installed
    }

    fun begin(freezer: ReportFreezing): Boolean {
        if (synchronized(stateLock) { _state.value.open }) return false
        val frozen = freezer.freeze() ?: return false
        val resolved = resolveInitialCapture(frozen)
        val installed = synchronized(stateLock) {
            val latest = _state.value
            if (latest.open) {
                false
            } else {
                captureId = UUID.randomUUID()
                _state.value = ReportState(
                    capture = resolved.capture,
                    offering = false,
                    open = true,
                    screenshotState = resolved.screenshotState,
                    dumpState = resolved.dumpState,
                )
                true
            }
        }
        if (installed) {
            observeCapture(resolved.capture, resolved.screenshotState, resolved.dumpState)
        }
        return installed
    }

    fun accept() {
        synchronized(stateLock) {
            val current = _state.value
            if (current.capture == null) return
            _state.value = current.copy(offering = false, open = true)
        }
    }

    fun dismiss() {
        synchronized(stateLock) {
            captureId = UUID.randomUUID()
            _state.value = ReportState()
        }
    }

    fun updateFramePng(target: ReportCapture, png: ByteArray) {
        synchronized(stateLock) {
            val current = _state.value
            val active = current.capture ?: return
            if (active !== target && active.framePng !== target.framePng) return
            if (active.frame.png.isNotEmpty()) {
                if (current.screenshotState != ReportScreenshotState.Present) {
                    _state.value = current.copy(screenshotState = ReportScreenshotState.Present)
                }
                return
            }
            if (png.isNotEmpty()) {
                _state.value = current.copy(
                    capture = active.copy(frame = active.frame.copy(png = png)),
                    screenshotState = ReportScreenshotState.Present,
                )
            } else if (current.screenshotState != ReportScreenshotState.Absent) {
                _state.value = current.copy(screenshotState = ReportScreenshotState.Absent)
            }
        }
    }

    fun updateDumpState(target: ReportCapture, ready: Boolean) {
        synchronized(stateLock) {
            val current = _state.value
            val active = current.capture ?: return
            if (active !== target && active.dump !== target.dump) return
            val next = if (ready) ReportDumpState.Ready else ReportDumpState.Absent
            if (current.dumpState != next) {
                _state.value = current.copy(dumpState = next)
            }
        }
    }

    fun setNote(note: String) {
        synchronized(stateLock) {
            _state.update { it.copy(draft = it.draft.copy(note = note)) }
        }
    }

    fun mark(mark: ReportMark) {
        synchronized(stateLock) {
            _state.update { it.copy(draft = it.draft.copy(marks = it.draft.marks + mark)) }
        }
    }

    fun noteOnMark(index: Int, text: String) {
        synchronized(stateLock) {
            _state.update { current ->
                if (index !in current.draft.marks.indices) return@update current
                val updated = current.draft.marks.toMutableList()
                updated[index] = updated[index].copy(note = text)
                current.copy(draft = current.draft.copy(marks = updated))
            }
        }
    }

    fun setMarkRedacted(index: Int, redacted: Boolean) {
        synchronized(stateLock) {
            _state.update { current ->
                if (index !in current.draft.marks.indices) return@update current
                val updated = current.draft.marks.toMutableList()
                updated[index] = updated[index].copy(redact = redacted)
                current.copy(draft = current.draft.copy(marks = updated))
            }
        }
    }

    fun toggleMarkRedacted(index: Int) {
        synchronized(stateLock) {
            _state.update { current ->
                if (index !in current.draft.marks.indices) return@update current
                val updated = current.draft.marks.toMutableList()
                updated[index] = updated[index].copy(redact = !updated[index].redact)
                current.copy(draft = current.draft.copy(marks = updated))
            }
        }
    }

    fun unmark(index: Int) {
        synchronized(stateLock) {
            _state.update { current ->
                if (index !in current.draft.marks.indices) return@update current
                val updated = current.draft.marks.toMutableList()
                updated.removeAt(index)
                current.copy(draft = current.draft.copy(marks = updated))
            }
        }
    }

    suspend fun send(
        submitter: ReportSubmitting?,
        account: AccountId?,
        build: String,
        gitSha: String = "",
        nowMs: Long = System.currentTimeMillis(),
        redactor: ReportImageRedactor = BitmapReportImageRedactor,
    ) {
        val claimed: Triple<UUID, ReportState, ReportSubmitting> = synchronized(stateLock) {
            val current = _state.value
            val capture = current.capture ?: return
            if (current.sending is ReportSending.Sending || current.sending is ReportSending.Sent) {
                return
            }
            if (account == null) {
                _state.value = current.copy(
                    sending = ReportSending.Failed(
                        "Sign in to the account this report is about, then send it again.",
                    ),
                )
                return
            }
            if (capture.account != null && capture.account != account) {
                _state.value = current.copy(
                    sending = ReportSending.Failed(
                        "This report was captured under a different account. Switch back or dismiss it to start a new one.",
                    ),
                )
                return
            }
            if (submitter == null) {
                _state.value = current.copy(
                    sending = ReportSending.Failed(
                        "amux is not running on this phone yet. Try again in a moment.",
                    ),
                )
                return
            }
            val token = captureId
            _state.value = current.copy(sending = ReportSending.Sending)
            Triple(token, current, submitter)
        }
        val (identity, claimedState, activeSubmitter) = claimed

        val upload = try {
            assembleUpload(
                snapshot = claimedState,
                build = build,
                gitSha = gitSha,
                nowMs = nowMs,
                redactor = redactor,
            )
        } catch (e: CancellationException) {
            synchronized(stateLock) {
                if (captureId == identity && _state.value.sending is ReportSending.Sending) {
                    _state.value = _state.value.copy(sending = ReportSending.Ready)
                }
            }
            throw e
        } catch (t: Throwable) {
            synchronized(stateLock) {
                if (captureId == identity) {
                    _state.value = _state.value.copy(sending = ReportSending.Failed(t.failureDescription))
                }
            }
            return
        } ?: run {
            synchronized(stateLock) {
                if (captureId == identity && _state.value.sending is ReportSending.Sending) {
                    _state.value = _state.value.copy(sending = ReportSending.Ready)
                }
            }
            return
        }

        val stillActive = synchronized(stateLock) {
            if (captureId != identity) {
                false
            } else {
                _state.value = _state.value.copy(sealed = upload, sending = ReportSending.Sending)
                true
            }
        }
        if (!stillActive) return

        val answer = try {
            activeSubmitter.submitReport(upload)
        } catch (e: CancellationException) {
            synchronized(stateLock) {
                if (captureId == identity && _state.value.sending is ReportSending.Sending) {
                    _state.value = _state.value.copy(sending = ReportSending.Ready)
                }
            }
            throw e
        } catch (t: Throwable) {
            Result.failure(t)
        }

        synchronized(stateLock) {
            if (captureId != identity) return
            answer.fold(
                onSuccess = { sent ->
                    val nextSending = if (sent.notSent.isEmpty()) {
                        ReportSending.Sent(
                            ReportReceipt(id = sent.id, receivedAtMs = sent.receivedAtMs),
                        )
                    } else {
                        ReportSending.Failed(sent.notSent)
                    }
                    _state.value = _state.value.copy(
                        sealed = _state.value.sealed?.copy(path = sent.reportPath),
                        sending = nextSending,
                    )
                },
                onFailure = { err ->
                    _state.value = _state.value.copy(
                        sending = ReportSending.Failed(err.failureDescription),
                    )
                },
            )
        }
    }

    suspend fun assembled(
        build: String,
        gitSha: String = "",
        nowMs: Long = System.currentTimeMillis(),
        redactor: ReportImageRedactor = BitmapReportImageRedactor,
    ): ReportUpload? = assembleUpload(
        snapshot = synchronized(stateLock) { _state.value },
        build = build,
        gitSha = gitSha,
        nowMs = nowMs,
        redactor = redactor,
    )

    private suspend fun assembleUpload(
        snapshot: ReportState,
        build: String,
        gitSha: String,
        nowMs: Long,
        redactor: ReportImageRedactor,
    ): ReportUpload? {
        snapshot.sealed?.let { return it }
        val capture = snapshot.capture ?: return null
        val rawPng = if (capture.frame.png.isNotEmpty()) {
            capture.frame.png
        } else {
            val deferred = capture.framePng
            if (deferred == null) {
                ByteArray(0)
            } else {
                try {
                    deferred.await()
                } catch (e: CancellationException) {
                    throw e
                } catch (_: Throwable) {
                    ByteArray(0)
                }
            }
        }
        val resolvedFrame = if (rawPng !== capture.frame.png) {
            capture.frame.copy(png = rawPng)
        } else {
            capture.frame
        }
        val redactedMarks = snapshot.draft.marks.filter { it.redact }
        val finalPng = if (redactedMarks.isNotEmpty() && resolvedFrame.png.isNotEmpty()) {
            redactor.redact(resolvedFrame, redactedMarks)
        } else {
            resolvedFrame.png
        }
        val dump = ReportAssembly.dumped(capture.dump)
        return ReportAssembly.upload(
            capture = capture.copy(frame = resolvedFrame),
            draft = snapshot.draft,
            build = build,
            gitSha = gitSha,
            createdAtMs = nowMs,
            dump = dump,
            framePng = finalPng,
        )
    }
}
