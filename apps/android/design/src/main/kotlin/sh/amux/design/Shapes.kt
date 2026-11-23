package sh.amux.design

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/** Shapes shared by more than one control, so they round alike. */
object AmuxShapes {
    /** Fully rounded ends: chips, pills and capsule buttons. */
    val Pill: Shape = RoundedCornerShape(percent = 50)
}

/**
 * The sizes a glyph is drawn at. Icons keep to these steps so a row's glyph,
 * a chip's glyph and a control's glyph each read the same wherever they sit.
 */
object IconSize {
    /** Inside a chip or beside a caption. */
    val Small: Dp = 14.dp

    /** Beside detail text and in banners. */
    val Inline: Dp = 16.dp

    /** In a field or a compact control. */
    val Control: Dp = 18.dp

    /** Leading a list row or an action. */
    val Row: Dp = 20.dp
}
