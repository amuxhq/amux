package sh.amux.core.runtime

import sh.amux.values.CallPhase
import sh.amux.values.Draft
import sh.amux.values.DraftAttachment
import sh.amux.values.DraftPart
import sh.amux.values.QuestionResponse
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Segment

/**
 * A photo or file the person picked, ready for [ChatModel.attach].
 *
 * An attachment is marked [image] only when its MIME type is one every agent
 * kind reads directly (PNG, JPEG, GIF, or WebP).
 */
data class PickedAttachment(
    val data: ByteArray,
    val name: String,
    val mime: String,
    val image: Boolean,
) {
    val size: Long
        get() = data.size.toLong()

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is PickedAttachment) return false
        return data.contentEquals(other.data) &&
            name == other.name &&
            mime == other.mime &&
            image == other.image
    }

    override fun hashCode(): Int {
        var result = data.contentHashCode()
        result = 31 * result + name.hashCode()
        result = 31 * result + mime.hashCode()
        result = 31 * result + image.hashCode()
        return result
    }

    companion object {
        private val NATIVE_IMAGE_MIMES = setOf(
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/webp",
        )

        private val MIME_BY_EXTENSION = mapOf(
            "png" to "image/png",
            "jpg" to "image/jpeg",
            "jpeg" to "image/jpeg",
            "gif" to "image/gif",
            "webp" to "image/webp",
            "pdf" to "application/pdf",
            "txt" to "text/plain",
            "md" to "text/markdown",
            "json" to "application/json",
            "csv" to "text/csv",
        )

        fun isNativeImageMime(mime: String): Boolean = mime.lowercase() in NATIVE_IMAGE_MIMES

        /**
         * Returns the native image MIME type (`image/png`, `image/jpeg`,
         * `image/gif`, or `image/webp`) detected from magic bytes, or `null`
         * for any other content. Matches `attachments::native_image` in Rust.
         */
        fun sniffNativeImageMime(bytes: ByteArray): String? = when {
            bytes.size >= 8 &&
                bytes[0] == 0x89.toByte() &&
                bytes[1] == 0x50.toByte() &&
                bytes[2] == 0x4E.toByte() &&
                bytes[3] == 0x47.toByte() &&
                bytes[4] == 0x0D.toByte() &&
                bytes[5] == 0x0A.toByte() &&
                bytes[6] == 0x1A.toByte() &&
                bytes[7] == 0x0A.toByte() -> "image/png"
            bytes.size >= 3 &&
                bytes[0] == 0xFF.toByte() &&
                bytes[1] == 0xD8.toByte() &&
                bytes[2] == 0xFF.toByte() -> "image/jpeg"
            bytes.size >= 6 &&
                bytes[0] == 'G'.code.toByte() &&
                bytes[1] == 'I'.code.toByte() &&
                bytes[2] == 'F'.code.toByte() &&
                bytes[3] == '8'.code.toByte() &&
                (bytes[4] == '7'.code.toByte() || bytes[4] == '9'.code.toByte()) &&
                bytes[5] == 'a'.code.toByte() -> "image/gif"
            bytes.size >= 12 &&
                bytes[0] == 'R'.code.toByte() &&
                bytes[1] == 'I'.code.toByte() &&
                bytes[2] == 'F'.code.toByte() &&
                bytes[3] == 'F'.code.toByte() &&
                bytes[8] == 'W'.code.toByte() &&
                bytes[9] == 'E'.code.toByte() &&
                bytes[10] == 'B'.code.toByte() &&
                bytes[11] == 'P'.code.toByte() -> "image/webp"
            else -> null
        }

        fun fromFile(
            data: ByteArray,
            name: String,
            mime: String? = null,
        ): PickedAttachment {
            val sniffed = sniffNativeImageMime(data)
            if (sniffed != null) {
                return PickedAttachment(
                    data = data,
                    name = name,
                    mime = sniffed,
                    image = true,
                )
            }
            val ext = name.substringAfterLast('.', "").lowercase()
            val resolvedMime = mime?.takeIf { it.isNotBlank() }
                ?: MIME_BY_EXTENSION[ext]
                ?: "application/octet-stream"
            return PickedAttachment(
                data = data,
                name = name,
                mime = resolvedMime,
                image = false,
            )
        }
    }
}

/**
 * One dictation session over a chat's draft.
 *
 * Speech hands over cumulative hypotheses, each the whole of what was heard
 * so far, corrections included. The session remembers the draft it started
 * from and rebuilds the draft from it with each hypothesis, stopping if the
 * person edits the draft directly.
 */
data class DictationState(
    val phase: Phase = Phase.Idle,
    private val original: String? = null,
    private val expected: String? = null,
) {
    enum class Permission {
        NotAsked,
        Allowed,
        Denied,
    }

    enum class Phase {
        Idle,
        RequestingPermission,
        Starting,
        Listening,
        Denied,
        Unavailable,
    }

    val active: Boolean
        get() = phase == Phase.RequestingPermission ||
            phase == Phase.Starting ||
            phase == Phase.Listening

    val sentence: String?
        get() = when (phase) {
            Phase.Idle -> null
            Phase.RequestingPermission ->
                "Allow microphone and speech access to dictate. Your draft stays here."
            Phase.Starting -> "Getting ready to listen. Your draft stays here."
            Phase.Listening -> "Listening. Tap Stop Dictation when you’re done."
            Phase.Denied ->
                "Dictation needs microphone and speech access. You can allow them in Settings."
            Phase.Unavailable -> "Dictation isn’t available right now. You can keep typing."
        }

    fun prepare(
        speech: Permission,
        microphone: Permission,
        available: Boolean,
    ): DictationState {
        val nextPhase = when {
            speech == Permission.Denied || microphone == Permission.Denied -> Phase.Denied
            !available -> Phase.Unavailable
            speech == Permission.NotAsked || microphone == Permission.NotAsked ->
                Phase.RequestingPermission
            else -> Phase.Starting
        }
        return copy(phase = nextPhase)
    }

    fun began(draft: String): DictationState {
        if (phase != Phase.Starting) return this
        return copy(phase = Phase.Listening, original = draft, expected = draft)
    }

    /**
     * Rebuilds the draft from where the session started and the whole of what
     * has been heard. A draft someone edited ends the session instead.
     */
    fun receive(heard: String, currentDraft: String): Pair<DictationState, String> {
        if (phase != Phase.Listening) return this to currentDraft
        val base = original
        if (currentDraft != expected || base == null) {
            return stop() to currentDraft
        }
        val joiner = if (base.isEmpty() || base.last().isWhitespace() || heard.isEmpty()) "" else " "
        val updated = base + joiner + heard
        return copy(expected = updated) to updated
    }

    fun stop(): DictationState = DictationState(phase = Phase.Idle)

    fun failed(): DictationState = DictationState(phase = Phase.Unavailable)
}

/**
 * A question card part way through: where it stands, what is picked, typed,
 * skipped and noted per question, and a reply being written.
 */
data class QuestionDraft(
    val step: Int = 0,
    val picks: List<Set<Int>>,
    val others: List<String?>,
    val highlighted: List<Int?>,
    val skipped: List<Boolean> = List(picks.size) { false },
    val notes: List<String> = List(picks.size) { "" },
    val noting: List<Boolean> = List(picks.size) { false },
    val reviewing: Boolean = false,
    val replying: Boolean = false,
    val reply: String = "",
) {
    /** Whether this draft was kept for a card with [count] questions. */
    fun fits(count: Int): Boolean =
        picks.size == count &&
            others.size == count &&
            highlighted.size == count &&
            skipped.size == count &&
            notes.size == count &&
            noting.size == count

    /**
     * Encodes each question's picks, typed "Something else" words, skip state,
     * and optional note into [QuestionResponse] entries in question order.
     */
    fun toResponses(): List<QuestionResponse> = picks.indices.map { i ->
        val note = notes.getOrNull(i)?.trim()?.takeIf { it.isNotEmpty() }
        if (skipped.getOrNull(i) == true) {
            QuestionResponse(note = note, other = null, picked = emptyList())
        } else {
            val chosen = picks[i].sorted()
            val typedOther = others.getOrNull(i)?.trim()?.takeIf { it.isNotEmpty() }
            QuestionResponse(
                note = note,
                other = typedOther,
                picked = if (chosen.isNotEmpty() || typedOther == null) chosen else null,
            )
        }
    }
}

/**
 * An attachment carried by a composer draft along with its optional character
 * insertion offset inside the draft's words (`null` places it after all text).
 */
data class PositionedAttachment(
    val attachment: DraftAttachment,
    val offset: Int? = null,
)

/** Words followed by attachments, matching the composer's chip-above-field shape. */
fun draftOf(words: String, attachments: List<DraftAttachment> = emptyList()): Draft =
    positionedDraftOf(
        words = words,
        attachments = attachments.map { PositionedAttachment(it, offset = null) },
    )

/**
 * Builds a positioned [Draft] placing each attachment with a non-null offset
 * at its character position in [words], and any unpositioned (`offset == null`)
 * attachments at the end in order.
 */
fun positionedDraftOf(
    words: String,
    attachments: List<PositionedAttachment> = emptyList(),
): Draft {
    val positioned = attachments
        .filter { it.offset != null }
        .sortedBy { it.offset!!.coerceIn(0, words.length) }
    val trailing = attachments.filter { it.offset == null }
    val parts = buildList {
        var cursor = 0
        for (entry in positioned) {
            val pos = entry.offset!!.coerceIn(cursor, words.length)
            if (pos > cursor) {
                add(DraftPart.Text(words.substring(cursor, pos)))
                cursor = pos
            }
            add(DraftPart.Attachment(entry.attachment))
        }
        if (cursor < words.length) {
            add(DraftPart.Text(words.substring(cursor)))
        }
        for (entry in trailing) {
            add(DraftPart.Attachment(entry.attachment))
        }
    }
    return Draft(parts = parts)
}

/**
 * Extracts each attachment with its character offset in [Draft.words], keeping
 * trailing attachments (after all text) unpositioned (`offset = null`) so later
 * typing stays ahead of picked files while mid-text pastes preserve their place.
 */
fun Draft.positionedAttachments(): List<PositionedAttachment> {
    val lastTextIndex = parts.indexOfLast { it is DraftPart.Text && it.value.isNotEmpty() }
    var cursor = 0
    return buildList {
        parts.forEachIndexed { index, part ->
            when (part) {
                is DraftPart.Text -> cursor += part.value.length
                is DraftPart.Attachment -> {
                    val offset = if (index < lastTextIndex) cursor else null
                    add(PositionedAttachment(part.value, offset = offset))
                }
            }
        }
    }
}

/**
 * The prompt's text as its host receives it: each attachment's place marked
 * with U+FFFC so providers read attachments at their placeholder positions.
 */
val Draft.text: String
    get() = parts.joinToString(separator = "") { part ->
        when (part) {
            is DraftPart.Text -> part.value
            is DraftPart.Attachment -> "\uFFFC"
        }
    }

/** The words alone, as they return to the composer field. */
val Draft.words: String
    get() = parts.mapNotNull { (it as? DraftPart.Text)?.value }.joinToString(separator = "")

/** The draft's attachments in order. */
val Draft.attachments: List<DraftAttachment>
    get() = parts.mapNotNull { (it as? DraftPart.Attachment)?.value }

/**
 * Inside a run of tool steps, thinking, retry notices, and hidden items
 * neither count nor end the run.
 */
val RowKind.passesThroughRuns: Boolean
    get() = when (this) {
        is RowKind.Thinking,
        is RowKind.Error,
        RowKind.Hidden,
        -> true
        else -> false
    }

/** A step the agent is still at: what a live run draws bright. */
val RowKind.inFlight: Boolean
    get() {
        val phase: CallPhase? = when (this) {
            is RowKind.Command -> state
            is RowKind.Explore -> state
            is RowKind.FileChange -> state
            is RowKind.ToolCall -> state
            is RowKind.Child -> state
            is RowKind.Subagent -> state
            else -> null
        }
        return phase == CallPhase.Pending || phase == CallPhase.Running
    }

/** Whether a row shows an attachment whose bytes it may be waiting on. */
internal val Row.holdsAttachment: Boolean
    get() = when (val k = kind) {
        is RowKind.Image -> true
        is RowKind.Prompt -> k.text.any { it is Segment.Attachment }
        is RowKind.Prose -> k.text.any { it is Segment.Attachment }
        else -> false
    }
