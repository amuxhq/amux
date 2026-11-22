package sh.amux.design

import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * The steps every gap in the app is taken from.
 *
 * Spacing counts in fours (with a 2.dp hairline-adjacent micro step) so
 * screens agree on rhythm without per-screen eyeballing.
 */
object Space {
    val xxs: Dp = 2.dp
    val xs: Dp = 4.dp
    val s: Dp = 8.dp
    val m: Dp = 12.dp
    val l: Dp = 16.dp
    val xl: Dp = 24.dp
    val xxl: Dp = 32.dp

    /** Leading and trailing screen margin, aligned with app bar content. */
    val margin: Dp = l
}
