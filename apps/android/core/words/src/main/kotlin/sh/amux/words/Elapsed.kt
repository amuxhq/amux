package sh.amux.words

import java.time.Duration

/**
 * A duration as a row has room for it.
 */
object Elapsed {
    fun spelled(seconds: Double): String {
        val clamped = seconds.coerceAtLeast(0.0)
        return when {
            clamped < 60.0 -> "${clamped.toLong()}s"
            clamped < 3_600.0 -> "${(clamped / 60.0).toLong()}m"
            clamped < 86_400.0 -> "${(clamped / 3_600.0).toLong()}h"
            else -> "${(clamped / 86_400.0).toLong()}d"
        }
    }

    fun spelled(duration: Duration): String = spelled(duration.toMillis() / 1_000.0)
}
