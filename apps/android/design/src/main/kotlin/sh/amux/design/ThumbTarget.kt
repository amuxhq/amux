package sh.amux.design

import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * Android accessibility touch-target floor (48.dp x 48.dp).
 */
object ThumbTarget {
    val MinSize: Dp = 48.dp
}

/**
 * Ensures an interactive container meets at least Android's [ThumbTarget.MinSize]
 * (48.dp) touch target in both axes without forcing small visual icons inside
 * it to scale up.
 */
fun Modifier.thumbTarget(
    minWidth: Dp = ThumbTarget.MinSize,
    minHeight: Dp = ThumbTarget.MinSize,
): Modifier = defaultMinSize(
    minWidth = maxOf(minWidth, ThumbTarget.MinSize),
    minHeight = maxOf(minHeight, ThumbTarget.MinSize),
)
