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
import androidx.compose.runtime.getValue
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
            val scale: Float
            val alpha: Float
            if (still) {
                scale = 0.78f
                alpha = 0.65f
            } else {
                val transition = rememberInfiniteTransition(label = "workingMark")
                val phase by transition.animateFloat(
                    initialValue = 0f,
                    targetValue = 1f,
                    animationSpec = infiniteRepeatable(
                        animation = tween(durationMillis = 1100),
                        repeatMode = RepeatMode.Reverse,
                    ),
                    label = "breath",
                )
                scale = 0.55f + 0.45f * phase
                alpha = 0.35f + 0.5f * phase
            }
            Canvas(modifier = modifier.size(size)) {
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
                        modifier = Modifier.size(20.dp),
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
                        modifier = Modifier.size(18.dp),
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
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val description = stringResource(R.string.accounts_switch_label)
    Box(
        modifier = modifier
            .thumbTarget()
            .clip(CircleShape)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics { contentDescription = description },
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
                    modifier = Modifier.size(18.dp),
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
