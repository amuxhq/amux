package sh.amux.core.runtime

import java.net.URI
import java.util.Base64
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import sh.amux.values.HostView
import sh.amux.values.PairRequest
import sh.amux.values.PendingPair

/**
 * A validated QR pairing invitation (`https://<cloud>/p#<invitation>`).
 *
 * Parsed only to reject malformed links before presenting a confirmation sheet;
 * the raw [link] is handed whole to the runtime for authentication.
 */
data class PairingInvitation(
    val host: HostId,
    val addrs: List<String>,
    val link: String,
) {
    val needsAnAccount: Boolean
        get() = addrs.isEmpty()

    /** Never leaks the embedded one-shot secret into logs or reports. */
    override fun toString(): String = "invitation from $host"

    companion object {
        const val PAGE_PATH: String = "/p"
        const val VERSION: Byte = 1
        private const val HEADER_BYTES: Int = 1 + 16 + 32 + 2

        fun parse(link: String): PairingInvitation? {
            val uri = try {
                URI(link.trim())
            } catch (_: Exception) {
                return null
            }
            val scheme = uri.scheme?.lowercase()
            if (scheme != "https" && scheme != "http") return null
            if (uri.host.isNullOrEmpty()) return null
            if (uri.rawPath != PAGE_PATH) return null
            val fragment = uri.rawFragment?.takeIf { it.isNotEmpty() } ?: return null
            val bytes = decodeBase64Url(fragment) ?: return null
            return parseBytes(bytes, link.trim())
        }

        internal fun parseBytes(bytes: ByteArray, link: String): PairingInvitation? {
            if (bytes.size < HEADER_BYTES) return null
            if (bytes[0] != VERSION) return null
            if ((bytes.size - HEADER_BYTES) % 4 != 0) return null
            val host = HostId.fromByteArray(bytes.copyOfRange(1, 17)) ?: return null
            val port = ((bytes[HEADER_BYTES - 2].toInt() and 0xFF) shl 8) or
                (bytes[HEADER_BYTES - 1].toInt() and 0xFF)
            val addrs = buildList {
                var offset = HEADER_BYTES
                while (offset < bytes.size) {
                    val a = bytes[offset].toInt() and 0xFF
                    val b = bytes[offset + 1].toInt() and 0xFF
                    val c = bytes[offset + 2].toInt() and 0xFF
                    val d = bytes[offset + 3].toInt() and 0xFF
                    add("$a.$b.$c.$d:$port")
                    offset += 4
                }
            }
            if (port == 0 && addrs.isNotEmpty()) return null
            return PairingInvitation(host = host, addrs = addrs, link = link)
        }

        private fun decodeBase64Url(text: String): ByteArray? {
            if (text.any { it == '+' || it == '/' || it == '=' || it.isWhitespace() }) {
                return null
            }
            return try {
                Base64.getUrlDecoder().decode(text)
            } catch (_: IllegalArgumentException) {
                null
            }
        }
    }
}

/** Immutable UI snapshot of an in-progress pairing flow. */
data class PairingState(
    val digits: String = "",
    val phase: PairingStore.Phase = PairingStore.Phase.Entering,
    val machine: HostView? = null,
    val refusal: String? = null,
    val told: String? = null,
) {
    val taking: Boolean
        get() = phase == PairingStore.Phase.Entering || phase == PairingStore.Phase.Refused

    /** Builds a PIN [PairRequest] once all six digits have been entered. */
    val request: PairRequest?
        get() = if (digits.length == PairingStore.CODE_LENGTH) {
            PairRequest.Pin(
                pin = digits,
                addrs = machine?.addrs.orEmpty(),
                hostId = machine?.hostId,
            )
        } else {
            null
        }
}

/**
 * Observable store driving two-step PIN and QR link pairing, refusal masking,
 * lockout prose pass-through, and stale-attempt cancellation.
 */
class PairingStore(
    private val clock: () -> Long = System::currentTimeMillis,
) {
    sealed interface Phase {
        data object Entering : Phase
        data object Checking : Phase
        data class Confirming(val pending: PendingPair) : Phase
        data class Trusted(val name: String) : Phase
        data object Refused : Phase
        data object NeedsSubscription : Phase
    }

    private val _state = MutableStateFlow(PairingState())
    val state: StateFlow<PairingState> = _state.asStateFlow()

    val snapshot: PairingState
        get() = _state.value

    private var attempt: Int = 0

    val nowMs: Long
        get() = clock()

    fun open(machine: HostView? = null) {
        attempt += 1
        _state.value = PairingState(
            digits = "",
            phase = Phase.Entering,
            machine = machine,
            refusal = null,
            told = null,
        )
    }

    fun enter(typed: String) {
        val current = _state.value
        if (!current.taking) return
        val filtered = typed.filter { it.isDigit() }.take(CODE_LENGTH)
        _state.update {
            it.copy(
                digits = filtered,
                phase = if (filtered.isNotEmpty()) Phase.Entering else it.phase,
            )
        }
    }

    fun checking(): Int {
        attempt += 1
        val currentAttempt = attempt
        _state.update { it.copy(phase = Phase.Checking, told = null) }
        return currentAttempt
    }

    fun reached(result: Result<PendingPair>, attempt: Int, relayOnly: Boolean = false) {
        if (attempt != this.attempt) return
        result.fold(
            onSuccess = { pending ->
                _state.update { it.copy(phase = Phase.Confirming(pending)) }
            },
            onFailure = { err ->
                val reason = err.failureDescription
                _state.update {
                    it.copy(
                        refusal = reason,
                        told = told(reason),
                        digits = "",
                        phase = if (relayOnly) Phase.NeedsSubscription else Phase.Refused,
                    )
                }
            },
        )
    }

    fun paired(result: Result<String>, attempt: Int) {
        if (attempt != this.attempt) return
        result.fold(
            onSuccess = { name ->
                _state.update { it.copy(phase = Phase.Trusted(name)) }
            },
            onFailure = { err ->
                val reason = err.failureDescription
                _state.update {
                    it.copy(
                        refusal = reason,
                        told = told(reason),
                        digits = "",
                        phase = Phase.Refused,
                    )
                }
            },
        )
    }

    fun abandoned() {
        attempt += 1
        _state.update {
            it.copy(
                digits = "",
                told = null,
                phase = Phase.Entering,
            )
        }
    }

    fun parseInvitation(link: String): PairingInvitation? = PairingInvitation.parse(link)

    companion object {
        const val CODE_LENGTH: Int = 6
        const val OFFER_WINDOW: String = "5 minutes"

        /**
         * Returns the core's message verbatim only when the core wrote it as a
         * user-facing sentence (capitalized, followed by lowercase, ending in a
         * period — e.g. lockout or already-paired prose). Codes like
         * `INVALID_PIN` or lowercase fragments return `null` so the UI shows
         * uniform copy that leaks nothing to a guesser.
         */
        fun told(reason: String): String? {
            val text = reason.trim()
            if (text.length < 3) return null
            if (!text[0].isUpperCase() || !text[1].isLowerCase() || !text.endsWith('.')) {
                return null
            }
            return text
        }
    }
}
