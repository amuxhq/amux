package sh.amux.features

import androidx.compose.material3.ModalBottomSheetProperties

import androidx.compose.material3.SheetValue

import sh.amux.design.Appearance
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.draw.drawBehind
import androidx.compose.material3.ripple
import androidx.compose.foundation.indication
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.ui.draw.shadow
import androidx.compose.material3.LocalTextStyle
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.SegmentedButton
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
import androidx.compose.animation.animateColorAsState
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.SegmentedButtonColors
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.layout.layout
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.LayoutDirection
import kotlin.math.roundToInt
import sh.amux.design.AmuxShapes
import sh.amux.design.Backdrop
import sh.amux.design.ProvideAmuxBackdrop
import sh.amux.design.ThumbTarget
import sh.amux.design.backdrop
import sh.amux.design.edgeTarget
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
    // An accent chip keeps the surface under it: accent text on a pale
    // accent wash falls under 4.5:1, and on the surface itself it passes.
    val background = if (accent) AmuxTheme.backdrop else colors.sunken
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
 * A callout as a card: a quiet sunken surface with its mark, a one-line title
 * and a one-line detail, and the action's words in a small accent pill.
 *
 * The whole card is the button, so the pill is only its label and a press
 * anywhere on the card acts; the card's 60 dp height is well past the 48 dp a
 * thumb needs. A card that can be put away draws its close cross as a second
 * button beside the first rather than inside it, so a press on the cross is
 * never taken as a press on the card. A card with no action is inert, and one
 * whose action is under way ([working]) shows its working mark and ignores a
 * second press.
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
    val shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius)
    val act = onButtonClick ?: onCardClick
    val pill = buttonLabel?.takeIf { onButtonClick != null }
    Row(
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = CalloutMinHeight)
            .clip(shape)
            .background(colors.sunken)
            .height(IntrinsicSize.Min),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        val face = Modifier
            .weight(1f)
            .fillMaxHeight()
            .let { base ->
                if (act == null) {
                    base
                } else {
                    base
                        .clickable(enabled = !working, role = Role.Button, onClick = act)
                        .let { if (buttonTestTag != null && pill != null) it.testTag(buttonTestTag) else it }
                }
            }
            .padding(start = Space.m, end = if (onDismiss != null) 0.dp else Space.m)
        Row(
            modifier = face,
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            Box(modifier = Modifier.width(24.dp), contentAlignment = Alignment.Center) {
                when {
                    working -> AmuxStateMark(state = RowState.Working, size = 12.dp)
                    icon != null -> Icon(
                        imageVector = icon,
                        contentDescription = null,
                        tint = colors.inkMuted,
                        modifier = Modifier.size(24.dp),
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
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (!detail.isNullOrEmpty()) {
                    Text(
                        text = detail,
                        style = AmuxTheme.typography.caption,
                        color = colors.inkMuted,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            when {
                pill != null -> AmuxPill(label = pill)
                act != null && onDismiss == null -> AmuxDisclosureChevron()
            }
        }
        if (onDismiss != null) {
            val dismissLabel = stringResource(R.string.callout_dismiss)
            Box(
                modifier = Modifier
                    .fillMaxHeight()
                    .width(ThumbTarget.MinSize)
                    .clickable(role = Role.Button, onClickLabel = dismissLabel, onClick = onDismiss)
                    .semantics { contentDescription = dismissLabel },
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = Icons.Outlined.Close,
                    contentDescription = null,
                    tint = colors.inkFaint,
                    modifier = Modifier.size(IconSize.Inline),
                )
            }
        }
    }
}

/** A callout card's minimum height, as iOS draws it. */
private val CalloutMinHeight = 60.dp

/**
 * An action's words in a small accent capsule: the label of a larger target
 * (a callout card, a row), never a target of its own.
 */
@Composable
fun AmuxPill(label: String, modifier: Modifier = Modifier) {
    val colors = AmuxTheme.colors
    Box(
        modifier = modifier
            .height(30.dp)
            .clip(AmuxShapes.Pill)
            .background(colors.accentFill)
            .padding(horizontal = Space.m),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = label,
            style = AmuxTheme.typography.caption.copy(fontWeight = FontWeight.SemiBold),
            color = colors.onAccent,
            maxLines = 1,
        )
    }
}

/**
 * Top-right account avatar control: a 32 dp disc in a 48 dp touch area.
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
            // The disc is the screen's account control, so it takes the
            // interactive tint when signed in.
            val fill = if (account.signedIn) colors.accentFill else colors.sunken
            val foreground = if (account.signedIn) colors.onAccent else colors.inkMuted
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

    /** An interactive tint for the one way in, such as Sign In. */
    Accent,
}

/**
 * Consistent amux button preserving the colour rule: primary actions use
 * neutral ink (never flooding the attention accent), destructive uses removed.
 *
 * Drawn 44 dp tall, as iOS draws its buttons; Material's own minimum
 * interactive size gives each one its 48 dp touch area without drawing it.
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
        .heightIn(min = ButtonHeight)
    val padding = PaddingValues(horizontal = Space.l, vertical = Space.s)

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
                contentPadding = padding,
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
                    contentColor = colors.onRemoved,
                    disabledContainerColor = colors.sunken,
                    disabledContentColor = colors.inkFaint,
                ),
                contentPadding = padding,
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
        ActionButtonKind.Accent -> {
            Button(
                onClick = onClick,
                enabled = enabled,
                shape = shape,
                colors = ButtonDefaults.buttonColors(
                    containerColor = colors.accentFill,
                    contentColor = colors.onAccent,
                    disabledContainerColor = colors.sunken,
                    disabledContentColor = colors.inkFaint,
                ),
                contentPadding = padding,
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
                contentPadding = padding,
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
                contentPadding = padding,
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

/** iOS's button height; a larger font grows the button past it. */
private val ButtonHeight = 44.dp

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
    // False for a sheet that asks a question only its buttons answer: a
    // swipe, a tap outside or Back would otherwise answer it by accident.
    dismissible: Boolean = true,
    content: @Composable ColumnScope.() -> Unit,
) {
    val colors = AmuxTheme.colors
    val sheetState = rememberModalBottomSheetState(
        skipPartiallyExpanded = true,
        confirmValueChange = { dismissible || it != SheetValue.Hidden },
    )
    ModalBottomSheet(
        // Material still reports Back as a dismissal when the sheet refuses
        // to hide, so an undismissible sheet ignores it here as well.
        onDismissRequest = { if (dismissible) onDismissRequest() },
        sheetState = sheetState,
        properties = ModalBottomSheetProperties(
            shouldDismissOnBackPress = dismissible,
            shouldDismissOnClickOutside = dismissible,
        ),
        modifier = modifier,
        containerColor = colors.raised,
        contentColor = colors.ink,
        contentWindowInsets = contentWindowInsets,
    ) {
        val sheet = this
        ProvideAmuxBackdrop(Backdrop.Raised) { sheet.content() }
    }
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
            .heightIn(min = minHeight)
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
            .heightIn(min = ThumbTarget.MinSize)
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
    colors: SwitchColors = amuxSwitchColors(),
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
            .heightIn(min = ThumbTarget.MinSize)
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
            modifier = switchModifier,
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

/**
 * Switch colours everywhere: on is the accent track with a white thumb, as on
 * iOS, since a switch is an interactive control and the accent is the
 * interactive tint. Off is a quiet sunken track.
 */
@Composable
internal fun amuxSwitchColors(): SwitchColors = SwitchDefaults.colors(
    checkedThumbColor = AmuxTheme.colors.onAccent,
    checkedTrackColor = AmuxTheme.colors.accent,
    checkedBorderColor = AmuxTheme.colors.accent,
    uncheckedThumbColor = AmuxTheme.colors.inkMuted,
    uncheckedTrackColor = AmuxTheme.colors.sunken,
    uncheckedBorderColor = AmuxTheme.colors.hairline,
)

/**
 * Text field colours: filled with the surface the field sits on, so a field
 * in a card is the card's colour rather than a black hole in a grey card.
 */
@Composable
internal fun amuxTextFieldColors() = OutlinedTextFieldDefaults.colors(
    focusedBorderColor = AmuxTheme.colors.ink,
    unfocusedBorderColor = AmuxTheme.colors.hairline,
    errorBorderColor = AmuxTheme.colors.accent,
    focusedContainerColor = AmuxTheme.backdrop,
    unfocusedContainerColor = AmuxTheme.backdrop,
    disabledContainerColor = AmuxTheme.backdrop,
    errorContainerColor = AmuxTheme.backdrop,
    cursorColor = AmuxTheme.colors.accent,
)

// --- Bars, headers and trailing marks ---------------------------------------

/**
 * The one top bar every screen uses.
 *
 * A pushed screen gets iOS's inline title: one 17 sp semibold line with an
 * optional caption under it. A tab's root screen ([large]) gets the large
 * title instead. Either way the bar grows with its text rather than clipping
 * it, so a two-line title at the largest font scale still fits, and it has no
 * rule under it: once content has scrolled under it ([scrolled]) its fill
 * steps off the page, as Material's scrolled bar does.
 *
 * With [onNavigate], a back arrow (or [navigationIcon]) leads, and the title
 * starts where Material's bar puts it; without one, the title starts on the
 * screen margin. [below] draws under the title row on the same fill, for a
 * search field that belongs to the bar. [subtitleContent] stands in for
 * [subtitle] when the caption line carries coloured counts.
 */
@Composable
fun AmuxTopBar(
    title: String,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    subtitleColor: Color = AmuxTheme.colors.inkMuted,
    subtitleModifier: Modifier = Modifier,
    titleModifier: Modifier = Modifier,
    large: Boolean = false,
    scrolled: Boolean = false,
    onNavigate: (() -> Unit)? = null,
    navigationIcon: ImageVector = Icons.AutoMirrored.Outlined.ArrowBack,
    navigationLabel: String? = null,
    navigationModifier: Modifier = Modifier,
    subtitleContent: (@Composable () -> Unit)? = null,
    below: (@Composable ColumnScope.() -> Unit)? = null,
    actions: @Composable RowScope.() -> Unit = {},
) {
    val colors = AmuxTheme.colors
    val fill by animateColorAsState(
        targetValue = if (scrolled) colors.barScrolled else colors.ground,
        animationSpec = Motion.standardAnimationSpec(),
        label = "barFill",
    )
    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(fill)
            .windowInsetsPadding(TopAppBarDefaults.windowInsets),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = TopBarMinHeight)
                .padding(horizontal = BarEdgeInset),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (onNavigate != null) {
                IconButton(onClick = onNavigate, modifier = navigationModifier) {
                    Icon(
                        imageVector = navigationIcon,
                        contentDescription = navigationLabel ?: stringResource(R.string.action_back),
                        tint = colors.ink,
                    )
                }
            }
            Column(
                modifier = Modifier
                    .weight(1f)
                    .padding(
                        start = if (onNavigate != null) BarEdgeInset else Space.margin - BarEdgeInset,
                        end = BarEdgeInset,
                    )
                    .padding(vertical = Space.s),
            ) {
                Text(
                    text = title,
                    style = if (large) AmuxTheme.typography.screenTitle else AmuxTheme.typography.identifier,
                    color = colors.ink,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                    modifier = titleModifier.semantics { heading() },
                )
                if (subtitleContent != null) {
                    CompositionLocalProvider(LocalTextStyle provides AmuxTheme.typography.caption) {
                        subtitleContent()
                    }
                } else if (!subtitle.isNullOrEmpty()) {
                    Text(
                        text = subtitle,
                        style = AmuxTheme.typography.caption,
                        color = subtitleColor,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                        modifier = subtitleModifier,
                    )
                }
            }
            Row(verticalAlignment = Alignment.CenterVertically, content = actions)
        }
        below?.invoke(this)
    }
}

/** Material's top bar height, kept as the bar's minimum. */
private val TopBarMinHeight = 64.dp

/** Material's inset between a bar's edge and its 48 dp icon buttons. */
private val BarEdgeInset = 4.dp

/**
 * The one header every sheet uses: the title on the content margin in the
 * pushed bar's inline style, an optional caption under it, and a close cross
 * whose glyph sits on the trailing content edge. Like the top bar it draws no
 * rule; it changes fill once the sheet's content has [scrolled] under it.
 */
@Composable
fun AmuxSheetHeader(
    title: String,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    titleModifier: Modifier = Modifier,
    scrolled: Boolean = false,
    onClose: (() -> Unit)? = null,
    closeLabel: String? = null,
    closeModifier: Modifier = Modifier,
    closeEnabled: Boolean = true,
    trailing: @Composable RowScope.() -> Unit = {},
) {
    val colors = AmuxTheme.colors
    val fill by animateColorAsState(
        targetValue = if (scrolled) colors.sunken else AmuxTheme.backdrop,
        animationSpec = Motion.standardAnimationSpec(),
        label = "sheetHeaderFill",
    )
    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(fill)
            .heightIn(min = ThumbTarget.MinSize + Space.s)
            .padding(horizontal = Space.margin, vertical = Space.xs),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = title,
                style = AmuxTheme.typography.identifier,
                color = colors.ink,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                modifier = titleModifier.semantics { heading() },
            )
            if (!subtitle.isNullOrEmpty()) {
                Text(
                    text = subtitle,
                    style = AmuxTheme.typography.caption,
                    color = colors.inkMuted,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        trailing()
        if (onClose != null) {
            AmuxEdgeIconButton(
                icon = Icons.Outlined.Close,
                contentDescription = closeLabel ?: stringResource(R.string.action_close),
                onClick = onClose,
                ink = GlyphInk.Close,
                tint = colors.ink,
                enabled = closeEnabled,
                modifier = closeModifier,
            )
        }
    }
}

/**
 * How much of a Material glyph's 24-unit box its ink fills across, so an
 * edge-aligned button can put the ink, not the box, on the content edge.
 */
object GlyphInk {
    const val Close = 14f / 24f
    const val More = 16f / 24f
    const val MoreVert = 4f / 24f
    const val Stop = 12f / 24f
    const val Default = 20f / 24f
}

/**
 * The least room an edge button takes. A glyph narrower than this would spill
 * its 48 dp target past the screen margin and off the screen's edge; it sits
 * a hair inside the content edge instead.
 */
private val EdgeLeastFootprint = ThumbTarget.MinSize - Space.margin * 2

/**
 * The most an edge button's target spills past the room it takes, on each
 * side: a neighbouring target this far away meets it without overlapping.
 */
val AmuxEdgeSpill = (ThumbTarget.MinSize - EdgeLeastFootprint) / 2

/**
 * An icon button whose glyph sits on the content edge it shares with the text
 * around it, while its touch area stays 48 dp and spills into the padding.
 * See [edgeTarget]. [ink] is the share of the icon box the glyph fills across
 * ([GlyphInk]); [modifier] lands on the 48 dp target, so test tags go there.
 */
@Composable
fun AmuxEdgeIconButton(
    icon: ImageVector,
    contentDescription: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    size: Dp = IconSize.Row,
    ink: Float = GlyphInk.Default,
    tint: Color = AmuxTheme.colors.inkMuted,
    enabled: Boolean = true,
) {
    AmuxEdgeTarget(
        footprint = maxOf(size * ink, EdgeLeastFootprint),
        footprintHeight = size,
        onClick = onClick,
        contentDescription = contentDescription,
        modifier = modifier,
        enabled = enabled,
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            tint = tint,
            modifier = Modifier.size(size),
        )
    }
}

/**
 * A control drawn [footprint] wide (a disc, a glyph) that takes only that
 * room in its row and centres a 48 dp touch target on it. See [edgeTarget].
 */
@Composable
fun AmuxEdgeTarget(
    footprint: Dp,
    onClick: () -> Unit,
    contentDescription: String,
    modifier: Modifier = Modifier,
    footprintHeight: Dp = footprint,
    enabled: Boolean = true,
    content: @Composable () -> Unit,
) {
    Box(
        modifier = Modifier
            .edgeTarget(footprint, footprintHeight)
            .then(modifier)
            .clip(CircleShape)
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .semantics { this.contentDescription = contentDescription },
        contentAlignment = Alignment.Center,
    ) {
        content()
    }
}

/**
 * The one disclosure chevron: auto-mirrored, so it points the way the screen
 * reads, and laid out by its ink rather than its icon box, so its tip sits on
 * the same trailing edge as a switch or a check mark in the next row.
 */
@Composable
fun AmuxDisclosureChevron(
    modifier: Modifier = Modifier,
    tint: Color = AmuxTheme.colors.inkFaint,
) {
    InkBox(icon = Icons.AutoMirrored.Outlined.KeyboardArrowRight, inkStart = 8.59f, inkEnd = 16f, tint = tint, modifier = modifier)
}

/**
 * The chosen-row check mark, in the accent as iOS draws it, laid out by its
 * ink so it ends on the card's trailing edge.
 */
@Composable
fun AmuxCheckMark(
    modifier: Modifier = Modifier,
    contentDescription: String? = null,
    tint: Color = AmuxTheme.colors.accent,
) {
    InkBox(
        icon = Icons.Outlined.Check,
        inkStart = 3.41f,
        inkEnd = 21f,
        tint = tint,
        modifier = if (contentDescription != null) {
            modifier.semantics { this.contentDescription = contentDescription }
        } else {
            modifier
        },
    )
}

/**
 * Draws [icon] at [IconSize.Row] but reports only its ink, from [inkStart]
 * to [inkEnd] in the icon's 24-unit box, as its width. Placement is relative,
 * so in a right-to-left layout the mirrored glyph's ink lands on the
 * mirrored edge.
 */
@Composable
private fun InkBox(icon: ImageVector, inkStart: Float, inkEnd: Float, tint: Color, modifier: Modifier) {
    Icon(
        imageVector = icon,
        contentDescription = null,
        tint = tint,
        modifier = modifier
            .layout { measurable, _ ->
                val box = IconSize.Row.roundToPx()
                val placeable = measurable.measure(Constraints.fixed(box, box))
                val start = (box * inkStart / 24f).roundToInt()
                val end = (box * inkEnd / 24f).roundToInt()
                layout(end - start, box) {
                    // A mirrored glyph's ink sits as far from the box's end as
                    // the unmirrored one's does from its start.
                    val offset = if (layoutDirection == LayoutDirection.Rtl) box - end else start
                    placeable.place(-offset, 0)
                }
            },
    )
}

/**
 * Draws a segmented choice's thumb on the chosen segment. In light mode the
 * white thumb sits on a pale track, so a shadow and an edge set it off; in
 * dark mode its fill is clearly lighter than the track, as on iOS.
 */
@Composable
internal fun Modifier.amuxSegmentThumb(chosen: Boolean, shape: Shape): Modifier {
    if (!chosen) return this
    val colors = AmuxTheme.colors
    val light = AmuxTheme.appearance == Appearance.Light
    return this
        .then(if (light) Modifier.shadow(elevation = 2.dp, shape = shape) else Modifier)
        .clip(shape)
        .background(colors.selection)
        .then(if (light) Modifier.border(AmuxTheme.metrics.hairline, colors.hairline, shape) else Modifier)
}

/**
 * The one text field: outlined, filled with the surface it sits on, about
 * 44 dp tall with one line, and its text inset by the same 16 dp as a card's
 * rows, so a field in a card lines up with the card's text.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AmuxField(
    value: String,
    onValueChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    placeholder: String? = null,
    enabled: Boolean = true,
    readOnly: Boolean = false,
    singleLine: Boolean = true,
    minLines: Int = 1,
    maxLines: Int = if (singleLine) 1 else Int.MAX_VALUE,
    isError: Boolean = false,
    textStyle: TextStyle = AmuxTheme.typography.body,
    keyboardOptions: KeyboardOptions = KeyboardOptions.Default,
    keyboardActions: KeyboardActions = KeyboardActions.Default,
    visualTransformation: VisualTransformation = VisualTransformation.None,
    leadingIcon: (@Composable () -> Unit)? = null,
    trailingIcon: (@Composable () -> Unit)? = null,
    interactionSource: MutableInteractionSource? = null,
) {
    val source = interactionSource ?: remember { MutableInteractionSource() }
    val fieldColors = amuxTextFieldColors()
    val colors = AmuxTheme.colors
    val shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius)
    BasicTextField(
        value = value,
        onValueChange = onValueChange,
        modifier = modifier
            .thumbTarget()
            .defaultMinSize(minHeight = AmuxFieldHeight),
        enabled = enabled,
        readOnly = readOnly,
        textStyle = textStyle.copy(color = if (enabled) colors.ink else colors.inkFaint),
        cursorBrush = SolidColor(colors.accent),
        keyboardOptions = keyboardOptions,
        keyboardActions = keyboardActions,
        singleLine = singleLine,
        minLines = minLines,
        maxLines = maxLines,
        visualTransformation = visualTransformation,
        interactionSource = source,
        decorationBox = { inner ->
            OutlinedTextFieldDefaults.DecorationBox(
                value = value,
                innerTextField = inner,
                enabled = enabled,
                singleLine = singleLine,
                visualTransformation = visualTransformation,
                interactionSource = source,
                isError = isError,
                placeholder = placeholder?.let {
                    {
                        Text(
                            text = it,
                            style = textStyle,
                            color = colors.inkFaint,
                            maxLines = if (singleLine) 1 else Int.MAX_VALUE,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                },
                leadingIcon = leadingIcon,
                trailingIcon = trailingIcon,
                colors = fieldColors,
                contentPadding = PaddingValues(horizontal = Space.l, vertical = FieldVerticalPadding),
                container = {
                    OutlinedTextFieldDefaults.Container(
                        enabled = enabled,
                        isError = isError,
                        interactionSource = source,
                        colors = fieldColors,
                        shape = shape,
                        focusedBorderThickness = AmuxTheme.metrics.hairline * 1.5f,
                        unfocusedBorderThickness = AmuxTheme.metrics.hairline,
                    )
                },
            )
        },
    )
}

/**
 * The one text field holding where its caret is, for a field that opens on
 * text already written and should put the caret after it rather than
 * before. Drawn as the [String] one is.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AmuxField(
    value: TextFieldValue,
    onValueChange: (TextFieldValue) -> Unit,
    modifier: Modifier = Modifier,
    placeholder: String? = null,
    enabled: Boolean = true,
    readOnly: Boolean = false,
    singleLine: Boolean = true,
    minLines: Int = 1,
    maxLines: Int = if (singleLine) 1 else Int.MAX_VALUE,
    isError: Boolean = false,
    textStyle: TextStyle = AmuxTheme.typography.body,
    keyboardOptions: KeyboardOptions = KeyboardOptions.Default,
    keyboardActions: KeyboardActions = KeyboardActions.Default,
    visualTransformation: VisualTransformation = VisualTransformation.None,
    leadingIcon: (@Composable () -> Unit)? = null,
    trailingIcon: (@Composable () -> Unit)? = null,
    interactionSource: MutableInteractionSource? = null,
) {
    val source = interactionSource ?: remember { MutableInteractionSource() }
    val fieldColors = amuxTextFieldColors()
    val colors = AmuxTheme.colors
    val shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius)
    BasicTextField(
        value = value,
        onValueChange = onValueChange,
        modifier = modifier
            .thumbTarget()
            .defaultMinSize(minHeight = AmuxFieldHeight),
        enabled = enabled,
        readOnly = readOnly,
        textStyle = textStyle.copy(color = if (enabled) colors.ink else colors.inkFaint),
        cursorBrush = SolidColor(colors.accent),
        keyboardOptions = keyboardOptions,
        keyboardActions = keyboardActions,
        singleLine = singleLine,
        minLines = minLines,
        maxLines = maxLines,
        visualTransformation = visualTransformation,
        interactionSource = source,
        decorationBox = { inner ->
            OutlinedTextFieldDefaults.DecorationBox(
                value = value.text,
                innerTextField = inner,
                enabled = enabled,
                singleLine = singleLine,
                visualTransformation = visualTransformation,
                interactionSource = source,
                isError = isError,
                placeholder = placeholder?.let {
                    {
                        Text(
                            text = it,
                            style = textStyle,
                            color = colors.inkFaint,
                            maxLines = if (singleLine) 1 else Int.MAX_VALUE,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                },
                leadingIcon = leadingIcon,
                trailingIcon = trailingIcon,
                colors = fieldColors,
                contentPadding = PaddingValues(horizontal = Space.l, vertical = FieldVerticalPadding),
                container = {
                    OutlinedTextFieldDefaults.Container(
                        enabled = enabled,
                        isError = isError,
                        interactionSource = source,
                        colors = fieldColors,
                        shape = shape,
                        focusedBorderThickness = AmuxTheme.metrics.hairline * 1.5f,
                        unfocusedBorderThickness = AmuxTheme.metrics.hairline,
                    )
                },
            )
        },
    )
}

/** With the body line, the padding that draws a one-line field 44 dp tall. */
private val FieldVerticalPadding = 11.dp
/** The one field height, as iOS draws its fields; a larger font grows it. */
internal val AmuxFieldHeight = 44.dp

/**
 * Holds [content]'s text at no more than [maxScale] of its size, for a bar
 * whose height cannot grow with the text (the chat's header capsules). iOS
 * caps the same header at its extra-large text size, about 1.15×.
 */
@Composable
fun CappedFontScale(maxScale: Float = 1.15f, content: @Composable () -> Unit) {
    val density = LocalDensity.current
    if (density.fontScale <= maxScale) {
        content()
    } else {
        CompositionLocalProvider(
            LocalDensity provides Density(density.density, maxScale),
            content = content,
        )
    }
}

/**
 * Filter chip colours: a chosen chip is filled with the accent, the
 * interactive tint, rather than with ink, which made it the brightest slab on
 * a dark screen. It is a solid fill, not a wash: accent text on a pale accent
 * wash falls under 4.5:1 on every surface a chip sits on.
 */
@Composable
internal fun amuxFilterChipColors() = FilterChipDefaults.filterChipColors(
    containerColor = AmuxTheme.backdrop,
    labelColor = AmuxTheme.colors.inkMuted,
    iconColor = AmuxTheme.colors.inkMuted,
    selectedContainerColor = AmuxTheme.colors.accentFill,
    selectedLabelColor = AmuxTheme.colors.onAccent,
    selectedLeadingIconColor = AmuxTheme.colors.onAccent,
    selectedTrailingIconColor = AmuxTheme.colors.onAccent,
)

/** A chip's edge: a hairline round a chip at rest, none round a chosen one. */
@Composable
internal fun amuxFilterChipBorder(selected: Boolean) = FilterChipDefaults.filterChipBorder(
    enabled = true,
    selected = selected,
    borderColor = AmuxTheme.colors.hairline,
    selectedBorderColor = Color.Transparent,
)

/**
 * One choice of a few, side by side, drawn as iOS's segmented control: a
 * track with the chosen segment on a thumb and its label in semibold. Each
 * segment answers to a 48 dp-tall target while the track is drawn
 * [SegmentTrackHeight] tall, and segments meet edge to edge, so no two
 * targets overlap. TalkBack reads it as a group of radio buttons. [tag] gives
 * each segment its test tag.
 */
@Composable
fun <T> AmuxSegmentedChoice(
    options: List<T>,
    selected: T?,
    label: @Composable (T) -> String,
    onSelect: (T) -> Unit,
    modifier: Modifier = Modifier,
    tag: (T) -> String? = { null },
    enabled: Boolean = true,
) {
    val colors = AmuxTheme.colors
    val track = colors.segmentTrack
    Row(
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = ThumbTarget.MinSize)
            .drawBehind {
                val height = SegmentTrackHeight.toPx()
                val radius = SegmentTrackRadius.toPx()
                drawRoundRect(
                    color = track,
                    topLeft = Offset(0f, (size.height - height) / 2),
                    size = Size(size.width, height),
                    cornerRadius = CornerRadius(radius, radius),
                )
            }
            .padding(horizontal = SegmentInset)
            .selectableGroup(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        options.forEach { option ->
            val chosen = option == selected
            val source = remember { MutableInteractionSource() }
            val thumbShape = RoundedCornerShape(SegmentTrackRadius - SegmentInset)
            Box(
                modifier = Modifier
                    .weight(1f)
                    .heightIn(min = ThumbTarget.MinSize)
                    .selectable(
                        selected = chosen,
                        enabled = enabled,
                        role = Role.RadioButton,
                        interactionSource = source,
                        indication = null,
                        onClick = { onSelect(option) },
                    )
                    .then(tag(option)?.let { Modifier.testTag(it) } ?: Modifier),
                contentAlignment = Alignment.Center,
            ) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(SegmentTrackHeight - SegmentInset * 2)
                        .amuxSegmentThumb(chosen, thumbShape)
                        .clip(thumbShape)
                        .indication(source, ripple())
                        .padding(horizontal = Space.s),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        text = label(option),
                        style = AmuxTheme.typography.detail.copy(
                            fontWeight = if (chosen) FontWeight.SemiBold else FontWeight.Normal,
                        ),
                        color = when {
                            !enabled -> colors.inkFaint
                            chosen -> colors.ink
                            else -> colors.inkMuted
                        },
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

/** iOS's segmented control: a 32 pt track (36 dp here, for Android's type). */
private val SegmentTrackHeight = 36.dp

/** The track's corner radius, and the thumb's inset from the track's edge. */
private val SegmentTrackRadius = 9.dp
private val SegmentInset = 2.dp
