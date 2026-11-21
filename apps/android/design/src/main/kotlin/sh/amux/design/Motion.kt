package sh.amux.design

import android.provider.Settings
import androidx.compose.animation.core.Easing
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.FiniteAnimationSpec
import androidx.compose.animation.core.LinearOutSlowInEasing
import androidx.compose.animation.core.snap
import androidx.compose.animation.core.tween
import androidx.compose.runtime.Composable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.platform.LocalContext

/**
 * Set to `true` during deterministic storybook/golden captures so blinking
 * carets and animated transitions settle immediately into their resting state.
 */
val LocalAmuxPhotographed = staticCompositionLocalOf { false }

/**
 * Optional override for reduced motion. When `null`, [Motion.isReducedMotion]
 * reads Android's system animator duration scale.
 */
val LocalAmuxReducedMotion = staticCompositionLocalOf<Boolean?> { null }

/**
 * House motion timings and curves.
 *
 * Two speeds cover the app: [StandardDurationMs] for state transitions and
 * [QuickDurationMs] for movement that follows a reader's own scroll or tap.
 * Both collapse to [snap] when reduced motion or capture photography is active.
 */
object Motion {
    const val StandardDurationMs: Int = 280
    const val QuickDurationMs: Int = 180

    val StandardEasing: Easing = FastOutSlowInEasing
    val QuickEasing: Easing = LinearOutSlowInEasing

    fun <T> standardSpec(reducedMotion: Boolean = false): FiniteAnimationSpec<T> =
        if (reducedMotion) {
            snap()
        } else {
            tween(durationMillis = StandardDurationMs, easing = StandardEasing)
        }

    fun <T> quickSpec(reducedMotion: Boolean = false): FiniteAnimationSpec<T> =
        if (reducedMotion) {
            snap()
        } else {
            tween(durationMillis = QuickDurationMs, easing = QuickEasing)
        }

    @Composable
    fun isReducedMotion(): Boolean {
        if (LocalAmuxPhotographed.current) return true
        val override = LocalAmuxReducedMotion.current
        if (override != null) return override
        val resolver = LocalContext.current.contentResolver
        val scale = runCatching {
            Settings.Global.getFloat(resolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f)
        }.getOrDefault(1f)
        return scale == 0f
    }

    @Composable
    fun <T> standardAnimationSpec(): FiniteAnimationSpec<T> =
        standardSpec(reducedMotion = isReducedMotion())

    @Composable
    fun <T> quickAnimationSpec(): FiniteAnimationSpec<T> =
        quickSpec(reducedMotion = isReducedMotion())
}
