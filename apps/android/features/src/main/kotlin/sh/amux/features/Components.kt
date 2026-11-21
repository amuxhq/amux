package sh.amux.features

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Person
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.MoreHoriz
import androidx.compose.material3.BottomSheetDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.RadioButton
import androidx.compose.material3.RadioButtonDefaults
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchColors
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.rememberModalBottomSheetState
import sh.amux.design.IconSize
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Motion
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.words.RowState

/**
 * The single chromatic attention dot spent strictly on "needs you".
 */
@Composable
fun AmuxAttentionDot(
    modifier: Modifier = Modifier,
    size: Dp = 8.dp,
) {
    val accent = AmuxTheme.colors.accent
    Canvas(modifier = modifier.size(size)) {
        drawCircle(color = accent)
    }
}

/**
 * Leading state indicator for an agent row or checking notice.
 *
 * Needs you is the sole filled chromatic mark; working breathes gently unless
 * reduced motion or deterministic photography is active; idle is an open ring;
 * unreachable hosts draw a dashed ring; settled/exited draw a quiet dot.
 */
@Composable
fun AmuxStateMark(
    state: RowState,
    modifier: Modifier = Modifier,
    size: Dp = 10.dp,
) {
    val colors = AmuxTheme.colors
    val still = Motion.isReducedMotion()
    when (state) {
        RowState.NeedsYou -> {
            Canvas(modifier = modifier.size(size)) {
                drawCircle(color = colors.accent)
            }
        }
        RowState.Working, RowState.Starting -> {
            // The breath is read in the draw block only, so a working row
            // redraws its mark each frame without recomposing the row.
            val phase: State<Float>? = if (still) {
                null
            } else {
                val transition = rememberInfiniteTransition(label = "workingMark")
                transition.animateFloat(
                    initialValue = 0f,
                    targetValue = 1f,
                    animationSpec = infiniteRepeatable(
                        animation = tween(durationMillis = 1100),
                        repeatMode = RepeatMode.Reverse,
                    ),
                    label = "breath",
                )
            }
            Canvas(modifier = modifier.size(size)) {
                val now = phase?.value
                val scale = if (now == null) 0.78f else 0.55f + 0.45f * now
                val alpha = if (now == null) 0.65f else 0.35f + 0.5f * now
                drawCircle(
                    color = colors.ink.copy(alpha = alpha),
                    radius = (this.size.minDimension / 2f) * scale,
                )
            }
        }
        RowState.Idle -> {
            Canvas(modifier = modifier.size(size)) {
                val stroke = 1.5.dp.toPx()
                drawCircle(
                    color = colors.inkFaint,
                    radius = (this.size.minDimension - stroke) / 2f,
                    style = Stroke(width = stroke),
                )
            }
        }
        is RowState.HostAway, is RowState.HostOffline -> {
            Canvas(modifier = modifier.size(size)) {
                val stroke = 1.5.dp.toPx()
                val dash = 2.dp.toPx()
                drawCircle(
                    color = colors.inkFaint,
                    radius = (this.size.minDimension - stroke) / 2f,
                    style = Stroke(
                        width = stroke,
                        pathEffect = PathEffect.dashPathEffect(floatArrayOf(dash, dash)),
                    ),
                )
            }
        }
        is RowState.Exited, RowState.Settled, RowState.Unsupported -> {
            Canvas(modifier = modifier.size(size)) {
                drawCircle(color = colors.hairline)
            }
        }
    }
}

/**
 * Quiet neutral status chip; only adopts accent styling for attention/failure.
 */
@Composable
fun AmuxStatusChip(
    text: String,
    modifier: Modifier = Modifier,
    accent: Boolean = false,
) {
    val colors = AmuxTheme.colors
    val background = if (accent) colors.accent.copy(alpha = 0.14f) else colors.sunken
    val foreground = if (accent) colors.accent else colors.inkMuted
    Surface(
        modifier = modifier,
        shape = RoundedCornerShape(Space.s),
        color = background,
        contentColor = foreground,
        border = BorderStroke(AmuxTheme.metrics.hairline, if (accent) colors.accent.copy(alpha = 0.35f) else colors.hairline),
    ) {
        Text(
            text = text,
            style = AmuxTheme.typography.caption,
            color = foreground,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(horizontal = Space.s, vertical = Space.xs),
        )
    }
}

/**
 * Quiet uppercase section header with accessibility heading semantics.
 */
@Composable
fun AmuxSectionHeader(
    title: String,
    modifier: Modifier = Modifier,
    trailing: String? = null,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = modifier
            .fillMaxWidth()
            .semantics { heading() }
            .padding(horizontal = Space.margin, vertical = Space.s),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = title.uppercase(),
            style = AmuxTheme.typography.sectionTitle,
            color = colors.inkFaint,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        if (trailing != null) {
            Spacer(modifier = Modifier.weight(1f))
            Text(
                text = trailing,
                style = AmuxTheme.typography.caption,
                color = colors.inkFaint,
                maxLines = 1,
            )
        }
    }
}

/**
 * Grouped neutral panel card for stacked rows separated by crisp hairlines.
 */
@Composable
fun AmuxRowCard(
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    AmuxPanel(modifier = modifier.fillMaxWidth()) {
        Column(modifier = Modifier.fillMaxWidth(), content = content)
    }
}

/**
 * Neutral empty-state / callout card built on [AmuxPanel].
 */
@Composable
fun AmuxEmptyCard(
    title: String,
    detail: String?,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
    working: Boolean = false,
    buttonLabel: String? = null,
    buttonTestTag: String? = null,
    onButtonClick: (() -> Unit)? = null,
    onCardClick: (() -> Unit)? = null,
    onDismiss: (() -> Unit)? = null,
) {
    val colors = AmuxTheme.colors
    val cardModifier = if (onCardClick != null && buttonLabel == null) {
        modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(AmuxTheme.metrics.cardRadius))
            .clickable(role = Role.Button, onClick = onCardClick)
    } else {
        modifier.fillMaxWidth()
    }
    AmuxPanel(modifier = cardModifier) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.l, vertical = Space.m),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            when {
                working -> {
                    AmuxStateMark(state = RowState.Working, size = 12.dp)
                }
                icon != null -> {
                    Icon(
                        imageVector = icon,
                        contentDescription = null,
                        tint = colors.inkMuted,
                        modifier = Modifier.size(IconSize.Row),
                    )
                }
            }
            Column(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(Space.xxs),
            ) {
                Text(
                    text = title,
                    style = AmuxTheme.typography.bodyEmphasis,
                    color = colors.ink,
                )
                if (!detail.isNullOrEmpty()) {
                    Text(
                        text = detail,
                        style = AmuxTheme.typography.detail,
                        color = colors.inkMuted,
                    )
                }
            }
            if (buttonLabel != null && onButtonClick != null) {
                AmuxActionButton(
                    label = buttonLabel,
                    kind = ActionButtonKind.Primary,
                    onClick = onButtonClick,
                    modifier = if (buttonTestTag != null) Modifier.testTag(buttonTestTag) else Modifier,
                )
            }
            if (onDismiss != null) {
                val dismissLabel = stringResource(R.string.callout_dismiss)
                IconButton(
                    onClick = onDismiss,
                    modifier = Modifier.thumbTarget(),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.Close,
                        contentDescription = dismissLabel,
                        tint = colors.inkMuted,
                        modifier = Modifier.size(IconSize.Control),
                    )
                }
            }
        }
    }
}

/**
 * Top-right account avatar control meeting Android's 48.dp touch target floor.
 */
@Composable
fun AmuxAccountAvatar(
    account: AccountUi?,
    onClick: (() -> Unit)?,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val description = stringResource(R.string.accounts_switch_label)
    val interactiveModifier = if (onClick != null) {
        Modifier
            .thumbTarget()
            .clip(CircleShape)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics { contentDescription = description }
    } else {
        Modifier
    }
    Box(
        modifier = modifier.then(interactiveModifier),
        contentAlignment = Alignment.Center,
    ) {
        if (account != null) {
            val fill = if (account.signedIn) colors.ink else colors.sunken
            val foreground = if (account.signedIn) colors.ground else colors.inkMuted
            Box(
                modifier = Modifier
                    .size(32.dp)
                    .clip(CircleShape)
                    .background(fill)
                    .border(
                        width = if (account.signedIn) 0.dp else AmuxTheme.metrics.hairline,
                        color = colors.hairline,
                        shape = CircleShape,
                    ),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = account.initials,
                    style = AmuxTheme.typography.badge,
                    color = foreground,
                    maxLines = 1,
                )
            }
        } else {
            Box(
                modifier = Modifier
                    .size(32.dp)
                    .clip(CircleShape)
                    .background(colors.raised)
                    .border(AmuxTheme.metrics.hairline, colors.hairline, CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = Icons.Outlined.Person,
                    contentDescription = null,
                    tint = colors.inkMuted,
                    modifier = Modifier.size(IconSize.Control),
                )
            }
        }
    }
}

enum class ActionButtonKind {
    Primary,
    Destructive,
    Outline,
    Quiet,
}

/**
 * Consistent amux button preserving the colour rule: primary actions use
 * neutral ink (never flooding the attention accent), destructive uses removed.
 */
@Composable
fun AmuxActionButton(
    label: String,
    kind: ActionButtonKind = ActionButtonKind.Primary,
    fillWidth: Boolean = false,
    enabled: Boolean = true,
    modifier: Modifier = Modifier,
    onClick: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius)
    val baseModifier = modifier
        .let { if (fillWidth) it.fillMaxWidth() else it }
        .thumbTarget()

    when (kind) {
        ActionButtonKind.Primary -> {
            Button(
                onClick = onClick,
                enabled = enabled,
                shape = shape,
                colors = ButtonDefaults.buttonColors(
                    containerColor = colors.ink,
                    contentColor = colors.ground,
                    disabledContainerColor = colors.sunken,
                    disabledContentColor = colors.inkFaint,
                ),
                contentPadding = PaddingValues(horizontal = Space.l, vertical = Space.m),
                modifier = baseModifier,
            ) {
                Text(
                    text = label,
                    style = AmuxTheme.typography.bodyEmphasis,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        ActionButtonKind.Destructive -> {
            Button(
                onClick = onClick,
                enabled = enabled,
                shape = shape,
                colors = ButtonDefaults.buttonColors(
                    containerColor = colors.removed,
                    contentColor = Color.White,
                    disabledContainerColor = colors.sunken,
                    disabledContentColor = colors.inkFaint,
                ),
                contentPadding = PaddingValues(horizontal = Space.l, vertical = Space.m),
                modifier = baseModifier,
            ) {
                Text(
                    text = label,
                    style = AmuxTheme.typography.bodyEmphasis,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        ActionButtonKind.Quiet -> {
            Button(
                onClick = onClick,
                enabled = enabled,
                shape = shape,
                colors = ButtonDefaults.buttonColors(
                    containerColor = colors.sunken,
                    contentColor = colors.ink,
                    disabledContainerColor = colors.sunken,
                    disabledContentColor = colors.inkFaint,
                ),
                contentPadding = PaddingValues(horizontal = Space.l, vertical = Space.m),
                modifier = baseModifier,
            ) {
                Text(
                    text = label,
                    style = AmuxTheme.typography.bodyEmphasis,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        ActionButtonKind.Outline -> {
            OutlinedButton(
                onClick = onClick,
                enabled = enabled,
                shape = shape,
                border = BorderStroke(AmuxTheme.metrics.hairline, colors.hairline),
                colors = ButtonDefaults.outlinedButtonColors(
                    contentColor = colors.ink,
                    disabledContentColor = colors.inkFaint,
                ),
                contentPadding = PaddingValues(horizontal = Space.l, vertical = Space.m),
                modifier = baseModifier,
            ) {
                Text(
                    text = label,
                    style = AmuxTheme.typography.bodyEmphasis,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

/**
 * Formats a 64-character hex fingerprint in groups of four so a person can
 * compare it cleanly against their computer.
 */
object FingerprintFormat {
    fun grouped(fingerprint: String): String {
        val clean = fingerprint.replace(" ", "")
        if (clean.isEmpty()) return ""
        return clean.chunked(4).joinToString(" ")
    }
}

/** Shared hairline inset separator inside list rows. */
@Composable
fun AmuxInsetHairline(
    leadingInset: Dp = Space.margin,
    modifier: Modifier = Modifier,
) {
    AmuxHairline(
        modifier = modifier.padding(start = leadingInset, end = Space.margin),
        color = AmuxTheme.colors.hairline.copy(alpha = 0.65f),
    )
}

/**
 * The one bottom sheet setup every sheet in the app goes through: expanded at
 * once, on the raised surface so it reads apart from the page behind it in
 * dark mode, with cards inside it on sunken.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AmuxSheet(
    onDismissRequest: () -> Unit,
    modifier: Modifier = Modifier,
    contentWindowInsets: @Composable () -> WindowInsets = { BottomSheetDefaults.windowInsets },
    content: @Composable ColumnScope.() -> Unit,
) {
    val colors = AmuxTheme.colors
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(
        onDismissRequest = onDismissRequest,
        sheetState = sheetState,
        modifier = modifier,
        containerColor = colors.raised,
        contentColor = colors.ink,
        contentWindowInsets = contentWindowInsets,
        content = content,
    )
}

/** One action in a sheet or menu list: a glyph and its label, red when it destroys. */
@Composable
fun AmuxActionRow(
    icon: ImageVector,
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    destructive: Boolean = false,
    minHeight: Dp = 52.dp,
) {
    val colors = AmuxTheme.colors
    val tint = if (destructive) colors.removed else colors.ink
    Row(
        modifier = modifier
            .fillMaxWidth()
            .thumbTarget(minHeight = minHeight)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = Space.margin, vertical = Space.m),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            tint = tint,
            modifier = Modifier.size(IconSize.Row),
        )
        Text(
            text = label,
            style = AmuxTheme.typography.body,
            color = tint,
        )
    }
}

/**
 * One choice of several, picked by tapping anywhere on the row.
 *
 * The row is the selectable element and its radio only draws the state, so
 * TalkBack reads the row once, says whether it is selected, and offers one
 * stop instead of two. Its parent groups its rows with
 * `Modifier.selectableGroup()`.
 */
@Composable
fun AmuxRadioRow(
    title: String,
    selected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    detail: String = "",
    warn: Boolean = false,
    enabled: Boolean = true,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = modifier
            .fillMaxWidth()
            .selectable(
                selected = selected,
                enabled = enabled || selected,
                role = Role.RadioButton,
                onClick = onClick,
            )
            .thumbTarget()
            .padding(horizontal = Space.s, vertical = Space.s),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        RadioButton(
            selected = selected,
            onClick = null,
            enabled = enabled || selected,
            colors = RadioButtonDefaults.colors(
                selectedColor = colors.ink,
                unselectedColor = colors.inkMuted,
                disabledSelectedColor = colors.inkMuted,
                disabledUnselectedColor = colors.inkFaint,
            ),
        )
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(Space.xxs),
        ) {
            Text(
                text = title,
                style = if (selected) AmuxTheme.typography.bodyEmphasis else AmuxTheme.typography.body,
                color = when {
                    warn -> colors.removed
                    !enabled && !selected -> colors.inkFaint
                    else -> colors.ink
                },
            )
            if (detail.isNotEmpty() && detail != title) {
                Text(
                    text = detail,
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

/**
 * A setting turned on or off by tapping anywhere on the row.
 *
 * The row is the toggleable element and its switch only draws the state, so
 * TalkBack reads the row's words once with its on or off state as one stop.
 * [modifier] shapes the row before it is made toggleable (fill, clip,
 * background, touch target); [contentPadding] goes inside it.
 */
@Composable
fun AmuxSwitchRow(
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
    contentPadding: PaddingValues = PaddingValues(horizontal = Space.l, vertical = Space.m),
    enabled: Boolean = true,
    colors: SwitchColors = amuxInkSwitchColors(),
    switchModifier: Modifier = Modifier,
    spacing: Dp = Space.m,
    labels: @Composable RowScope.() -> Unit,
) {
    Row(
        modifier = modifier
            .toggleable(
                value = checked,
                enabled = enabled,
                role = Role.Switch,
                onValueChange = onCheckedChange,
            )
            .padding(contentPadding),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(spacing),
    ) {
        labels()
        Switch(
            checked = checked,
            onCheckedChange = null,
            enabled = enabled,
            colors = colors,
            modifier = switchModifier.thumbTarget(),
        )
    }
}

/**
 * What an action does, one line each: a check for what stays and a cross
 * for what goes, or an ellipsis while it is still being found out.
 */
@Composable
internal fun ConsequenceRow(
    kept: Boolean,
    text: String,
    modifier: Modifier = Modifier,
    pending: Boolean = false,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = modifier.fillMaxWidth(),
        verticalAlignment = Alignment.Top,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Icon(
            imageVector = when {
                pending -> Icons.Outlined.MoreHoriz
                kept -> Icons.Outlined.Check
                else -> Icons.Outlined.Close
            },
            contentDescription = null,
            tint = if (kept || pending) colors.inkMuted else colors.removed,
            modifier = Modifier
                .padding(top = 2.dp)
                .size(IconSize.Inline),
        )
        Text(
            text = text,
            style = AmuxTheme.typography.detail,
            color = if (kept || pending) colors.inkMuted else colors.ink,
            modifier = Modifier.weight(1f),
        )
    }
}

/** Neutral ink switch colours for settings and choices that are not asks. */
@Composable
internal fun amuxInkSwitchColors(): SwitchColors = SwitchDefaults.colors(
    checkedThumbColor = AmuxTheme.colors.ground,
    checkedTrackColor = AmuxTheme.colors.ink,
    checkedBorderColor = AmuxTheme.colors.ink,
    uncheckedThumbColor = AmuxTheme.colors.inkMuted,
    uncheckedTrackColor = AmuxTheme.colors.sunken,
    uncheckedBorderColor = AmuxTheme.colors.hairline,
)

/** Switch colours for an ask, where turning one on is what the agent waits on. */
@Composable
internal fun amuxSwitchColors(): SwitchColors = SwitchDefaults.colors(
    checkedThumbColor = AmuxTheme.colors.onAccent,
    checkedTrackColor = AmuxTheme.colors.accent,
    uncheckedThumbColor = AmuxTheme.colors.inkMuted,
    uncheckedTrackColor = AmuxTheme.colors.sunken,
    uncheckedBorderColor = AmuxTheme.colors.hairline,
)

@Composable
internal fun amuxTextFieldColors() = OutlinedTextFieldDefaults.colors(
    focusedBorderColor = AmuxTheme.colors.ink,
    unfocusedBorderColor = AmuxTheme.colors.hairline,
    errorBorderColor = AmuxTheme.colors.accent,
    focusedContainerColor = AmuxTheme.colors.ground,
    unfocusedContainerColor = AmuxTheme.colors.ground,
    cursorColor = AmuxTheme.colors.ink,
)
