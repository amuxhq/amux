package sh.amux.features

import androidx.compose.runtime.Stable

/**
 * The time a screen counts its elapsed times to: how long an agent has been
 * working, how long a computer has been offline.
 *
 * Only the leaves that print a time call [nowMs], each where it prints it,
 * so the clock's tick recomposes those leaves and nothing around them.
 */
@Stable
fun interface ElapsedClock {
    fun nowMs(): Long

    companion object {
        /** A clock that never moves: elapsed times read as the host last reported them. */
        val Still: ElapsedClock = ElapsedClock { 0L }
    }
}
