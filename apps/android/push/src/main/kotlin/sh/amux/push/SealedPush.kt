package sh.amux.push

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.util.Base64

/**
 * What a push says once it is opened: which agent on which host, its name,
 * and what it wants of the person.
 */
@Serializable
data class OpenedPush(
    val host: String,
    val agent: String,
    val name: String,
    val notice: Said,
)

/**
 * Opening what a daemon sealed to this phone.
 *
 * HPKE in base mode: the 32-byte encapsulated key, then the ciphertext, as
 * base64. A push the wrong key or a stale key cannot open comes back null,
 * and the plain alert the service sent stays on show.
 */
object SealedPush {
    /**
     * Binds a sealed push to what it is for. The daemon seals with the same
     * words.
     */
    private val INFO = "amux push 1".toByteArray(Charsets.UTF_8)
    private const val ENCAPSULATED_KEY_SIZE = Hpke.KEY_SIZE

    private val json = Json {
        ignoreUnknownKeys = true
    }

    fun open(sealed: String, key: PushPrivateKey): OpenedPush? {
        val plain = plaintext(sealed, key) ?: return null
        return runCatching {
            json.decodeFromString(OpenedPush.serializer(), plain.toString(Charsets.UTF_8))
        }.getOrNull()
    }

    /** The sealed JSON bytes, undecoded. */
    fun plaintext(sealed: String, key: PushPrivateKey): ByteArray? {
        val bytes = runCatching { Base64.getDecoder().decode(sealed) }.getOrNull() ?: return null
        if (bytes.size <= ENCAPSULATED_KEY_SIZE) return null
        val encapsulatedKey = bytes.copyOfRange(0, ENCAPSULATED_KEY_SIZE)
        val ciphertext = bytes.copyOfRange(ENCAPSULATED_KEY_SIZE, bytes.size)
        return runCatching {
            Hpke.openBase(
                encapsulatedKey = encapsulatedKey,
                recipientPrivateKey = key.rawRepresentation,
                info = INFO,
                ciphertext = ciphertext,
            )
        }.getOrNull()
    }

    /**
     * Opens the sealed text of a push's nested payload (`payload["amux"]`),
     * with the key of the profile it is addressed to. A key that cannot be
     * read, as before the first unlock after a restart, leaves it closed.
     */
    fun open(payload: Map<String, Any?>, keys: PushKeyStore): OpenedPush? {
        val amux = payload["amux"] as? Map<*, *> ?: return null
        val to = amux["to"] as? String ?: return null
        val sealed = amux["sealed"] as? String ?: return null
        return openForProfile(to = to, sealed = sealed, keys = keys)
    }

    /**
     * Opens [sealed] with the key stored for profile [to].
     */
    fun openForProfile(to: String, sealed: String, keys: PushKeyStore): OpenedPush? {
        val key = runCatching { keys.key(to) }.getOrNull() ?: return null
        return open(sealed, key)
    }
}
