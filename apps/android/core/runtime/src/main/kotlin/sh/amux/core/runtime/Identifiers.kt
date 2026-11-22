package sh.amux.core.runtime

import java.nio.ByteBuffer
import java.util.UUID
import kotlinx.serialization.KSerializer
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.descriptors.PrimitiveKind
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import sh.amux.values.AgentKey

/**
 * A machine identifier: a UUID carried across the JNI bridge as sixteen
 * unsigned byte values (`0..255`).
 */
@Serializable(with = HostIdSerializer::class)
data class HostId(val uuid: UUID) {
    val bytes: List<Int>
        get() = byteArray.map { it.toInt() and 0xFF }

    val byteArray: ByteArray
        get() = ByteBuffer.allocate(16)
            .putLong(uuid.mostSignificantBits)
            .putLong(uuid.leastSignificantBits)
            .array()

    override fun toString(): String = uuid.toString().lowercase()

    companion object {
        fun parse(text: String): HostId? = try {
            HostId(UUID.fromString(text.trim()))
        } catch (_: IllegalArgumentException) {
            null
        }

        fun fromBytes(bytes: List<Int>): HostId? {
            if (bytes.size != 16) return null
            val raw = ByteArray(16) { i ->
                val value = bytes[i]
                if (value !in 0..255) return null
                value.toByte()
            }
            return fromByteArray(raw)
        }

        fun fromByteArray(bytes: ByteArray): HostId? {
            if (bytes.size != 16) return null
            val buffer = ByteBuffer.wrap(bytes)
            val msb = buffer.long
            val lsb = buffer.long
            return HostId(UUID(msb, lsb))
        }
    }
}

object HostIdSerializer : KSerializer<HostId> {
    override val descriptor: SerialDescriptor =
        PrimitiveSerialDescriptor("sh.amux.core.runtime.HostId", PrimitiveKind.STRING)

    override fun serialize(encoder: Encoder, value: HostId) {
        encoder.encodeString(value.toString())
    }

    override fun deserialize(decoder: Decoder): HostId {
        val text = decoder.decodeString()
        return HostId.parse(text) ?: throw SerializationException("not a UUID: $text")
    }
}

/** Account identifier (`sub` from the account service). */
@Serializable(with = AccountIdSerializer::class)
data class AccountId(val value: String) {
    override fun toString(): String = value
}

object AccountIdSerializer : KSerializer<AccountId> {
    override val descriptor: SerialDescriptor =
        PrimitiveSerialDescriptor("sh.amux.core.runtime.AccountId", PrimitiveKind.STRING)

    override fun serialize(encoder: Encoder, value: AccountId) {
        encoder.encodeString(value.value)
    }

    override fun deserialize(decoder: Decoder): AccountId = AccountId(decoder.decodeString())
}

/** Encodes a bridge byte list (`0..255`) as lowercase hexadecimal. */
fun hexEncode(bytes: List<Int>): String = buildString(bytes.size * 2) {
    for (b in bytes) {
        append(HEX_DIGITS[(b ushr 4) and 0x0F])
        append(HEX_DIGITS[b and 0x0F])
    }
}

/** Encodes a byte array as lowercase hexadecimal. */
fun hexEncode(bytes: ByteArray): String = buildString(bytes.size * 2) {
    for (b in bytes) {
        val unsigned = b.toInt() and 0xFF
        append(HEX_DIGITS[(unsigned ushr 4) and 0x0F])
        append(HEX_DIGITS[unsigned and 0x0F])
    }
}

private const val HEX_DIGITS = "0123456789abcdef"

fun AgentKey(host: HostId, agent: UUID): AgentKey =
    AgentKey(host = host.bytes, agent = HostId(agent).bytes)

val AgentKey.hostId: HostId?
    get() = HostId.fromBytes(host)

val AgentKey.agentUuid: UUID?
    get() = HostId.fromBytes(agent)?.uuid

fun AgentKey.formattedId(): String =
    agentUuid?.toString()?.lowercase() ?: hexEncode(agent)
