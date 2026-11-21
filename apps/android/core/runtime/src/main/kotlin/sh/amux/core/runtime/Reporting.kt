package sh.amux.core.runtime

import java.io.File
import java.util.UUID
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
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
)

/** One rectangle drawn on the frozen frame with an optional note. */
@Serializable
data class ReportMark(
    val x: Double,
    val y: Double,
    val width: Double,
    val height: Double,
    val note: String = "",
)

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

data class ReportState(
    val capture: ReportCapture? = null,
    val offering: Boolean = false,
    val open: Boolean = false,
    val draft: ReportDraft = ReportDraft(),
    val sending: ReportSending = ReportSending.Ready,
    val sealed: ReportUpload? = null,
)

/**
 * Holds the single in-progress problem report across screenshot preview
 * transitions and retries.
 */
class ReportStore(
    initial: ReportState = ReportState(),
) {
    private val _state = MutableStateFlow(
        initial.copy(open = initial.open && initial.capture != null),
    )
    val state: StateFlow<ReportState> = _state.asStateFlow()

    private var captureId: UUID = UUID.randomUUID()

    fun offer(freezer: ReportFreezing): Boolean {
        val current = _state.value
        if (current.open || current.capture != null) return false
        val frozen = freezer.freeze() ?: return false
        _state.update { it.copy(capture = frozen, offering = true) }
        return true
    }

    fun begin(freezer: ReportFreezing): Boolean {
        if (_state.value.open) return false
        val frozen = freezer.freeze() ?: return false
        _state.update { it.copy(capture = frozen, offering = false, open = true) }
        return true
    }

    fun accept() {
        if (_state.value.capture == null) return
        _state.update { it.copy(offering = false, open = true) }
    }

    fun dismiss() {
        captureId = UUID.randomUUID()
        _state.value = ReportState()
    }

    fun setNote(note: String) {
        _state.update { it.copy(draft = it.draft.copy(note = note)) }
    }

    fun mark(mark: ReportMark) {
        _state.update { it.copy(draft = it.draft.copy(marks = it.draft.marks + mark)) }
    }

    fun noteOnMark(index: Int, text: String) {
        _state.update { current ->
            if (index !in current.draft.marks.indices) return@update current
            val updated = current.draft.marks.toMutableList()
            updated[index] = updated[index].copy(note = text)
            current.copy(draft = current.draft.copy(marks = updated))
        }
    }

    fun unmark(index: Int) {
        _state.update { current ->
            if (index !in current.draft.marks.indices) return@update current
            val updated = current.draft.marks.toMutableList()
            updated.removeAt(index)
            current.copy(draft = current.draft.copy(marks = updated))
        }
    }

    suspend fun send(
        submitter: ReportSubmitting?,
        account: AccountId?,
        build: String,
        gitSha: String = "",
        nowMs: Long = System.currentTimeMillis(),
    ) {
        val current = _state.value
        if (current.capture == null) return
        if (current.sending is ReportSending.Sending || current.sending is ReportSending.Sent) {
            return
        }
        if (account == null) {
            _state.update {
                it.copy(
                    sending = ReportSending.Failed(
                        "Sign in to the account this report is about, then send it again.",
                    ),
                )
            }
            return
        }
        if (submitter == null) {
            _state.update {
                it.copy(
                    sending = ReportSending.Failed(
                        "amux is not running on this phone yet. Try again in a moment.",
                    ),
                )
            }
            return
        }
        val upload = assembled(build = build, gitSha = gitSha, nowMs = nowMs) ?: return
        val identity = captureId
        _state.update { it.copy(sealed = upload, sending = ReportSending.Sending) }

        val answer = submitter.submitReport(upload)
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
                _state.update {
                    it.copy(
                        sealed = it.sealed?.copy(path = sent.reportPath),
                        sending = nextSending,
                    )
                }
            },
            onFailure = { err ->
                _state.update {
                    it.copy(sending = ReportSending.Failed(err.failureDescription))
                }
            },
        )
    }

    suspend fun assembled(
        build: String,
        gitSha: String = "",
        nowMs: Long = System.currentTimeMillis(),
    ): ReportUpload? {
        val current = _state.value
        current.sealed?.let { return it }
        val capture = current.capture ?: return null
        val dump = ReportAssembly.dumped(capture.dump)
        return ReportAssembly.upload(
            capture = capture,
            draft = current.draft,
            build = build,
            gitSha = gitSha,
            createdAtMs = nowMs,
            dump = dump,
        )
    }
}
