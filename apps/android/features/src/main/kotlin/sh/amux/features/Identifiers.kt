package sh.amux.features

import java.nio.ByteBuffer
import java.util.UUID

/**
 * An agent or device id as text: the lowercase 8-4-4-4-12 form the daemon,
 * the other clients and the tests all print. The bridge carries ids as
 * sixteen unsigned byte values; anything else (which no bridge sends) is
 * spelled byte by byte so it still names one thing.
 */
fun uuidText(bytes: List<Int>): String {
    if (bytes.size != 16 || bytes.any { it !in 0..255 }) {
        return bytes.joinToString("-")
    }
    val buffer = ByteBuffer.wrap(ByteArray(16) { bytes[it].toByte() })
    return UUID(buffer.long, buffer.long).toString().lowercase()
}
