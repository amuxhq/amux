package sh.amux.push

import java.security.MessageDigest
import java.util.Locale
import java.util.concurrent.ConcurrentHashMap

/**
 * A 32-byte X25519 private key used to open pushes sealed to this phone.
 */
class PushPrivateKey(rawBytes: ByteArray) {
    private val bytes: ByteArray = rawBytes.copyOf()

    init {
        require(bytes.size == Hpke.KEY_SIZE) {
            "X25519 private key must be ${Hpke.KEY_SIZE} bytes, got ${bytes.size}"
        }
    }

    /** Defensive copy of the 32-byte raw X25519 private key scalar. */
    val rawRepresentation: ByteArray
        get() = bytes.copyOf()

    /** The corresponding 32-byte RFC 7748 X25519 public key. */
    val publicKey: ByteArray
        get() = Hpke.deriveX25519PublicKey(bytes)

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is PushPrivateKey) return false
        return MessageDigest.isEqual(bytes, other.bytes)
    }

    override fun hashCode(): Int = bytes.contentHashCode()

    override fun toString(): String = "PushPrivateKey(REDACTED)"

    companion object {
        fun generate(): PushPrivateKey = PushPrivateKey(Hpke.generatePrivateKey())
    }
}

/**
 * The key store could not read or write a push key.
 */
class PushKeyFailure(
    message: String,
    cause: Throwable? = null,
) : Exception(message, cause)

/**
 * The private keys pushes are sealed to, one per profile, keyed by the
 * profile's host id (which a push's `to` names, case-insensitively).
 */
interface PushKeyStore {
    /** The key for [host], if this phone holds one. */
    @Throws(PushKeyFailure::class)
    fun key(host: String): PushPrivateKey?

    /** Creates and keeps a new key for [host], replacing any it held. */
    @Throws(PushKeyFailure::class)
    fun makeKey(host: String): PushPrivateKey

    /** Forgets [host]'s key. One that was never there is no failure. */
    @Throws(PushKeyFailure::class)
    fun removeKey(host: String)
}

/**
 * Thread-safe in-memory [PushKeyStore] for tests and non-Android environments.
 */
class InMemoryPushKeyStore(
    initial: Map<String, PushPrivateKey> = emptyMap(),
) : PushKeyStore {
    private val keys = ConcurrentHashMap<String, PushPrivateKey>()

    init {
        for ((host, key) in initial) {
            keys[normalize(host)] = key
        }
    }

    override fun key(host: String): PushPrivateKey? = keys[normalize(host)]

    override fun makeKey(host: String): PushPrivateKey {
        val created = PushPrivateKey.generate()
        keys[normalize(host)] = created
        return created
    }

    override fun removeKey(host: String) {
        keys.remove(normalize(host))
    }

    private fun normalize(host: String): String = host.lowercase(Locale.ROOT)
}
