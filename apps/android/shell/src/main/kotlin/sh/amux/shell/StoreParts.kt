package sh.amux.shell

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.State
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.remember
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import sh.amux.features.ElapsedClock

/**
 * The [part] of a store's state a screen draws, given out only when that
 * part changes.
 *
 * The fleet moves on every agent's every turn; a screen that reads one row
 * of it, or whether it is empty, collects that part so a change elsewhere in
 * the fleet does not recompose it. [keys] are what [part] reads besides the
 * state (the agent whose row it is).
 */
@Composable
internal fun <T, R> StateFlow<T>.collectPart(vararg keys: Any?, part: (T) -> R): State<R> {
    val parts = remember(this, *keys) { map(part).distinctUntilChanged() }
    val initial = remember(this, *keys) { part(value) }
    return parts.collectAsState(initial = initial)
}

/**
 * A clock that ticks once a second while [ticking], for the leaves that
 * print an elapsed time; still otherwise, so a screen with nothing counting
 * does no work.
 */
@Composable
internal fun rememberTickingClock(now: () -> Long, ticking: Boolean): ElapsedClock {
    val currentNow by rememberUpdatedState(now)
    val time = remember { mutableLongStateOf(now()) }
    LaunchedEffect(ticking) {
        time.longValue = currentNow()
        if (!ticking) return@LaunchedEffect
        while (true) {
            delay(1_000L)
            time.longValue = currentNow()
        }
    }
    return remember(time) { ElapsedClock { time.longValue } }
}
