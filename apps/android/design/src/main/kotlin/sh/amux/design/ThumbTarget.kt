package sh.amux.design

import androidx.compose.material3.minimumInteractiveComponentSize
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.layout
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * Android accessibility touch-target floor (48.dp x 48.dp).
 */
object ThumbTarget {
    val MinSize: Dp = 48.dp
}

/**
 * Reserves Android's 48 dp touch area around a control without drawing the
 * control any bigger.
 *
 * Plenty of controls are meant to be small: a chip, a pill, a close cross. What
 * has to be 48 dp is what answers to a thumb, and that is not the rectangle a
 * person sees. The control is measured at its own size and centred in the
 * reserved square; whatever draws after this modifier (its fill, border,
 * ripple) draws at the control's size, and the platform routes a touch
 * anywhere in the reserved area to it. This is Material's own
 * `minimumInteractiveComponentSize`, so Material controls, which already
 * apply it, never need this.
 *
 * Put it before the control's drawing modifiers. Full-width rows don't need
 * it: give them a minimum height instead, so their own padding is the target.
 */
fun Modifier.thumbTarget(): Modifier = minimumInteractiveComponentSize()

/**
 * A 48 dp target that takes only its glyph's room in the layout.
 *
 * A trailing close cross or a leading attach disc belongs on the content edge
 * its row shares with the text above and below it. A plain 48 dp icon button
 * puts the glyph (48 - [width]) / 2 inside that edge, so it reads as pushed
 * in. This reports [width] by [height] as its size, so the parent lays out the
 * glyph where it is drawn, and centres the full 48 dp target on it, spilling
 * into the padding around it. The parent's padding must be at least the
 * spill, or the target is cut by the parent's clip.
 *
 * Apply it first, before the control's clickable and its drawing: everything
 * after it is the 48 dp target, which is what TalkBack and the touch system
 * see.
 */
fun Modifier.edgeTarget(width: Dp, height: Dp = width): Modifier = layout { measurable, constraints ->
    val target = ThumbTarget.MinSize.roundToPx()
    val placeable = measurable.measure(Constraints.fixed(target, target))
    val footprintWidth = width.roundToPx().coerceIn(constraints.minWidth, constraints.maxWidth)
    val footprintHeight = height.roundToPx().coerceIn(constraints.minHeight, constraints.maxHeight)
    layout(footprintWidth, footprintHeight) {
        placeable.place((footprintWidth - target) / 2, (footprintHeight - target) / 2)
    }
}

/**
 * Like [edgeTarget], for a small control whose size is its content's: a
 * count and a chevron, a text link. It takes its content's own room in the
 * layout and grows what comes after it to at least 48 dp each way, centred on
 * that room, so a row of text is not pushed apart by a target it cannot see.
 * Centre the content in what follows (a Box with centred content), since it
 * is drawn inside the grown target.
 *
 * [spill] says which way the target grows past the content's height: about
 * the centre by default, or [Alignment.Bottom] to keep the target's bottom on
 * the content's and grow it upward only, when a row of other targets sits
 * close below.
 */
fun Modifier.spillTarget(
    spill: Alignment.Vertical = Alignment.CenterVertically,
): Modifier = layout { measurable, constraints ->
    val target = ThumbTarget.MinSize.roundToPx()
    val width = measurable.maxIntrinsicWidth(constraints.maxHeight).coerceIn(constraints.minWidth, constraints.maxWidth)
    val height = measurable.minIntrinsicHeight(width).coerceIn(constraints.minHeight, constraints.maxHeight)
    val placeable = measurable.measure(
        Constraints(
            minWidth = maxOf(width, target),
            maxWidth = maxOf(width, target),
            minHeight = maxOf(height, target),
            maxHeight = maxOf(height, target),
        ),
    )
    layout(width, height) {
        placeable.place((width - placeable.width) / 2, spill.align(placeable.height, height))
    }
}
