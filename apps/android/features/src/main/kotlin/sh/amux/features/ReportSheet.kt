package sh.amux.features

import sh.amux.design.spillTarget

import androidx.compose.ui.platform.LocalConfiguration

import sh.amux.design.backdrop
import sh.amux.design.ThumbTarget
import sh.amux.design.IconSize
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.BugReport
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import kotlin.math.abs
import kotlin.math.roundToInt
import sh.amux.design.AmuxChrome
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.words.RowState

/**
 * Local test tags for [ReportOfferBanner], [ReportOverlay], and [ReportOverlayContent].
 */
object ReportTags {
    const val OFFER_BANNER: String = "report.prompt"
    const val OFFER_OPEN: String = "report.prompt.open"
    const val OFFER_DISMISS: String = "report.prompt.dismiss"

    const val OVERLAY: String = "report.screen"
    const val CLOSE: String = "report.cancel"
    const val SEND: String = "report.send"
    const val SENDING: String = "report.sending"
    const val SIGNED_OUT: String = "report.signed-out"
    const val FRAME: String = "report.frame"
    const val FRAME_PREPARING: String = "report.frame.preparing"
    const val FRAME_FALLBACK: String = "report.frame.fallback"
    const val NOTE: String = "report.note"
    const val DUMP_STATUS: String = "report.dump-status"
    const val REFUSAL: String = "report.refusal"
    const val RETRY: String = "report.retry"
    const val SENT: String = "report.sent"
    const val RECEIPT_ID: String = "report.receipt.id"
    const val COPY_RECEIPT: String = "report.receipt.copy"
    const val DONE: String = "report.done"

    fun markRow(index: Int): String = "report.mark.$index"
    fun markNote(index: Int): String = "report.mark.$index.note"
    fun markRedact(index: Int): String = "report.mark.$index.redact"
    fun markRemove(index: Int): String = "report.mark.$index.remove"
}

/**
 * One normalized `[0..1]` rectangle drawn on the captured screenshot with an
 * optional short note and redaction toggle.
 */
@Immutable
data class ReportMarkUi(
    val left: Float,
    val top: Float,
    val right: Float,
    val bottom: Float,
    val note: String = "",
    val redact: Boolean = false,
) {
    val width: Float get() = (right - left).coerceAtLeast(0f)
    val height: Float get() = (bottom - top).coerceAtLeast(0f)

    fun containsNormalized(nx: Float, ny: Float): Boolean =
        nx in left..right && ny in top..bottom

    companion object {
        fun normalized(
            x1: Float,
            y1: Float,
            x2: Float,
            y2: Float,
            note: String = "",
            redact: Boolean = false,
        ): ReportMarkUi = ReportMarkUi(
            left = minOf(x1, x2).coerceIn(0f, 1f),
            top = minOf(y1, y2).coerceIn(0f, 1f),
            right = maxOf(x1, x2).coerceIn(0f, 1f),
            bottom = maxOf(y1, y2).coerceIn(0f, 1f),
            note = note,
            redact = redact,
        )
    }
}

/** Delivery state rendered by [ReportOverlayContent]. */
@Immutable
sealed interface ReportDeliveryUi {
    data object Ready : ReportDeliveryUi
    data object Sending : ReportDeliveryUi
    data class Failed(val reason: String) : ReportDeliveryUi
    data class Sent(val referenceId: String) : ReportDeliveryUi
}

/** Pure UI state for the screenshot-to-report overlay. */
@Immutable
data class ReportUiState(
    val imageBytes: ByteArray? = null,
    val frameAspectRatio: Float = DEFAULT_FRAME_ASPECT_RATIO,
    val note: String = "",
    val marks: List<ReportMarkUi> = emptyList(),
    val signedIn: Boolean = true,
    val screenshotPending: Boolean = false,
    val screenshotPresent: Boolean = !screenshotPending && imageBytes?.isNotEmpty() == true,
    val dumpPending: Boolean = false,
    val dumpReady: Boolean = true,
    val locked: Boolean = false,
    val delivery: ReportDeliveryUi = ReportDeliveryUi.Ready,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ReportUiState) return false
        return imageBytes.contentEquals(other.imageBytes) &&
            frameAspectRatio == other.frameAspectRatio &&
            note == other.note &&
            marks == other.marks &&
            signedIn == other.signedIn &&
            screenshotPending == other.screenshotPending &&
            screenshotPresent == other.screenshotPresent &&
            dumpPending == other.dumpPending &&
            dumpReady == other.dumpReady &&
            locked == other.locked &&
            delivery == other.delivery
    }

    override fun hashCode(): Int {
        var result = imageBytes?.contentHashCode() ?: 0
        result = 31 * result + frameAspectRatio.hashCode()
        result = 31 * result + note.hashCode()
        result = 31 * result + marks.hashCode()
        result = 31 * result + signedIn.hashCode()
        result = 31 * result + screenshotPending.hashCode()
        result = 31 * result + screenshotPresent.hashCode()
        result = 31 * result + dumpPending.hashCode()
        result = 31 * result + dumpReady.hashCode()
        result = 31 * result + locked.hashCode()
        result = 31 * result + delivery.hashCode()
        return result
    }

    companion object {
        const val DEFAULT_FRAME_ASPECT_RATIO: Float = 9f / 19.5f
    }
}

/** Actions emitted by [ReportOverlay] / [ReportOverlayContent]. */
sealed interface ReportAction {
    data object Dismiss : ReportAction
    data class SetNote(val note: String) : ReportAction
    data class AddMark(val mark: ReportMarkUi) : ReportAction
    data class UpdateMarkNote(val index: Int, val note: String) : ReportAction
    data class ToggleMarkRedact(val index: Int) : ReportAction
    data class RemoveMark(val index: Int) : ReportAction
    data object Send : ReportAction
    data class CopyReference(val referenceId: String) : ReportAction
}

/**
 * Compact, dismissible banner shown after Android notifies of a screenshot so
 * taking an ordinary screenshot never interrupts what the person is doing.
 */
@Composable
fun ReportOfferBanner(
    onOpen: () -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val dismissLabel = stringResource(R.string.report_offer_dismiss)

    AmuxChrome(
        modifier = modifier
            .fillMaxWidth()
            .testTag(ReportTags.OFFER_BANNER),
        shape = RoundedCornerShape(AmuxTheme.metrics.cardRadius),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = Space.m, end = Space.xs, top = Space.xs, bottom = Space.xs),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            Icon(
                imageVector = Icons.Outlined.BugReport,
                contentDescription = null,
                tint = colors.inkMuted,
                modifier = Modifier.size(IconSize.Row),
            )
            Column(
                modifier = Modifier
                    .weight(1f)
                    .clickable(role = Role.Button, onClick = onOpen)
                    .padding(vertical = Space.xs),
                verticalArrangement = Arrangement.spacedBy(Space.xxs),
            ) {
                Text(
                    text = stringResource(R.string.report_offer_title),
                    style = typography.bodyEmphasis,
                    color = colors.ink,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    text = stringResource(R.string.report_offer_detail),
                    style = typography.caption,
                    color = colors.inkMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            TextButton(
                onClick = onOpen,
                modifier = Modifier.testTag(ReportTags.OFFER_OPEN),
            ) {
                Text(
                    text = stringResource(R.string.report_offer_open),
                    style = typography.bodyEmphasis,
                    color = colors.accent,
                    maxLines = 1,
                )
            }
            IconButton(
                onClick = onDismiss,
                modifier = Modifier.testTag(ReportTags.OFFER_DISMISS),
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

/**
 * Full-screen problem report overlay handling system Back dismissal.
 */
@Composable
fun ReportOverlay(
    state: ReportUiState,
    onAction: (ReportAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    BackHandler {
        onAction(ReportAction.Dismiss)
    }
    ReportOverlayContent(
        state = state,
        onAction = onAction,
        modifier = modifier,
    )
}

/**
 * Pure Compose content for reporting a problem over a frozen screenshot.
 * Exposed separately from [ReportOverlay] for headless Storybook and tests.
 */
@Composable
fun ReportOverlayContent(
    state: ReportUiState,
    onAction: (ReportAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val isSent = state.delivery is ReportDeliveryUi.Sent
    val isSending = state.delivery is ReportDeliveryUi.Sending
    val editable = !state.locked && !isSending && !isSent
    var selectedMarkIndex by remember { mutableStateOf<Int?>(null) }
    val activeSelectedMark = selectedMarkIndex?.takeIf { it in state.marks.indices }

    val closeLabel = stringResource(
        if (isSent) R.string.action_close else R.string.action_cancel,
    )

    AmuxGround(
        modifier = modifier
            .fillMaxSize()
            .testTag(ReportTags.OVERLAY),
    ) {
        Column(modifier = Modifier.fillMaxSize()) {
            // A full-screen task, so its close sits where a pushed screen's
            // back arrow does.
            AmuxTopBar(
                title = stringResource(R.string.report_title),
                onNavigate = { onAction(ReportAction.Dismiss) },
                navigationIcon = Icons.Outlined.Close,
                navigationLabel = closeLabel,
                navigationModifier = Modifier.testTag(ReportTags.CLOSE),
            )

            when (val delivery = state.delivery) {
                is ReportDeliveryUi.Sent -> {
                    ReportSentContent(
                        referenceId = delivery.referenceId,
                        onCopyReference = { onAction(ReportAction.CopyReference(delivery.referenceId)) },
                        modifier = Modifier
                            .weight(1f)
                            .fillMaxWidth(),
                    )
                }
                else -> {
                    Column(
                        modifier = Modifier
                            .weight(1f)
                            .fillMaxWidth()
                            .verticalScroll(rememberScrollState())
                            .padding(horizontal = Space.margin, vertical = Space.m),
                        verticalArrangement = Arrangement.spacedBy(Space.m),
                    ) {
                        if (!state.signedIn) {
                            AmuxPanel(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .testTag(ReportTags.SIGNED_OUT),
                            ) {
                                Text(
                                    text = stringResource(R.string.report_signed_out),
                                    style = typography.detail,
                                    color = colors.inkMuted,
                                    modifier = Modifier.padding(Space.m),
                                )
                            }
                        }

                        Box(
                            modifier = Modifier.fillMaxWidth(),
                            contentAlignment = Alignment.Center,
                        ) {
                            ReportFrameCanvas(
                                imageBytes = state.imageBytes,
                                screenshotPending = state.screenshotPending,
                                aspectRatio = state.frameAspectRatio.coerceIn(0.3f, 2.5f),
                                marks = state.marks,
                                selectedIndex = activeSelectedMark,
                                editable = editable,
                                onAddMark = { mark ->
                                    selectedMarkIndex = state.marks.size
                                    onAction(ReportAction.AddMark(mark))
                                },
                                onSelectMark = { index ->
                                    selectedMarkIndex = index
                                },
                                // Big enough to draw a box on with a thumb:
                                // most of the screen, leaving the hint and
                                // the first mark in view under it.
                                modifier = Modifier
                                    .padding(horizontal = Space.xl)
                                    .heightIn(max = LocalConfiguration.current.screenHeightDp.dp * 0.6f),
                            )
                        }

                        Text(
                            text = stringResource(R.string.report_guidance_marks),
                            style = typography.caption,
                            color = colors.inkFaint,
                        )

                        if (state.marks.isNotEmpty()) {
                            ReportMarksList(
                                marks = state.marks,
                                selectedIndex = activeSelectedMark,
                                editable = editable,
                                onSelectMark = { selectedMarkIndex = it },
                                onUpdateNote = { index, note ->
                                    onAction(ReportAction.UpdateMarkNote(index, note))
                                },
                                onToggleRedact = { index ->
                                    onAction(ReportAction.ToggleMarkRedact(index))
                                },
                                onRemoveMark = { index ->
                                    val currentSelected = selectedMarkIndex
                                    selectedMarkIndex = when {
                                        currentSelected == null -> null
                                        currentSelected == index -> null
                                        currentSelected > index -> currentSelected - 1
                                        else -> currentSelected
                                    }
                                    onAction(ReportAction.RemoveMark(index))
                                },
                            )
                        }
                    }

                    ReportComposerDock(
                        state = state,
                        editable = editable,
                        onAction = onAction,
                    )
                }
            }
        }
    }
}

@Composable
internal fun rememberReportImageBitmap(imageBytes: ByteArray?): ImageBitmap? =
    rememberDecodedImage(
        bytes = imageBytes?.takeIf { it.isNotEmpty() },
        maxPx = rememberScreenMaxPx(),
    ).bitmapOrNull

internal fun computeFittedImageRect(
    containerWidth: Float,
    containerHeight: Float,
    imageWidth: Float,
    imageHeight: Float,
): Rect {
    val safeContainerW = containerWidth.coerceAtLeast(1f)
    val safeContainerH = containerHeight.coerceAtLeast(1f)
    if (imageWidth <= 0f || imageHeight <= 0f) {
        return Rect(0f, 0f, safeContainerW, safeContainerH)
    }
    val scale = minOf(safeContainerW / imageWidth, safeContainerH / imageHeight)
    val fittedW = (imageWidth * scale).coerceAtLeast(1f)
    val fittedH = (imageHeight * scale).coerceAtLeast(1f)
    val left = (safeContainerW - fittedW) / 2f
    val top = (safeContainerH - fittedH) / 2f
    return Rect(left, top, left + fittedW, top + fittedH)
}

@Composable
private fun ReportFrameCanvas(
    imageBytes: ByteArray?,
    screenshotPending: Boolean,
    aspectRatio: Float,
    marks: List<ReportMarkUi>,
    selectedIndex: Int?,
    editable: Boolean,
    onAddMark: (ReportMarkUi) -> Unit,
    onSelectMark: (Int?) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val bitmap = rememberReportImageBitmap(imageBytes)
    val density = LocalDensity.current
    val minDragPx = with(density) { 6.dp.toPx() }
    val shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius)
    val markedSummary = stringResource(R.string.report_frame_marked_count, marks.size)

    var dragStart by remember { mutableStateOf<Offset?>(null) }
    var dragCurrent by remember { mutableStateOf<Offset?>(null) }
    var canvasSize by remember { mutableStateOf(IntSize.Zero) }

    BoxWithConstraints(
        modifier = modifier
            .aspectRatio(aspectRatio, matchHeightConstraintsFirst = aspectRatio < 1f)
            .clip(shape)
            .background(colors.sunken)
            .border(AmuxTheme.metrics.hairline, colors.hairline, shape)
            .semantics { stateDescription = markedSummary }
            .testTag(ReportTags.FRAME),
    ) {
        val frameWidthPx = (canvasSize.width.takeIf { it > 0 } ?: constraints.maxWidth)
            .toFloat()
            .coerceAtLeast(1f)
        val frameHeightPx = (canvasSize.height.takeIf { it > 0 } ?: constraints.maxHeight)
            .toFloat()
            .coerceAtLeast(1f)
        val imageWidthPx = bitmap?.width?.toFloat() ?: 0f
        val imageHeightPx = bitmap?.height?.toFloat() ?: 0f
        val fittedRect = computeFittedImageRect(
            containerWidth = frameWidthPx,
            containerHeight = frameHeightPx,
            imageWidth = imageWidthPx,
            imageHeight = imageHeightPx,
        )

        val currentMarks by rememberUpdatedState(marks)
        val currentOnAddMark by rememberUpdatedState(onAddMark)
        val currentOnSelectMark by rememberUpdatedState(onSelectMark)
        val currentFittedRect by rememberUpdatedState(fittedRect)

        if (bitmap != null) {
            Image(
                bitmap = bitmap,
                contentDescription = null,
                contentScale = ContentScale.Fit,
                modifier = Modifier.fillMaxSize(),
            )
        } else if (screenshotPending) {
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(Space.l)
                    .testTag(ReportTags.FRAME_PREPARING),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                AmuxStateMark(state = RowState.Working, size = 12.dp)
                Spacer(modifier = Modifier.height(Space.s))
                Text(
                    text = stringResource(R.string.report_frame_preparing),
                    style = typography.detail,
                    color = colors.inkMuted,
                    textAlign = TextAlign.Center,
                )
            }
        } else {
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(Space.l)
                    .testTag(ReportTags.FRAME_FALLBACK),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                Icon(
                    imageVector = Icons.Outlined.Image,
                    contentDescription = null,
                    tint = colors.inkFaint,
                    modifier = Modifier.size(28.dp),
                )
                Spacer(modifier = Modifier.height(Space.s))
                Text(
                    text = stringResource(R.string.report_frame_unavailable),
                    style = typography.detail,
                    color = colors.inkMuted,
                    textAlign = TextAlign.Center,
                )
            }
        }

        val gestureModifier = if (editable && !screenshotPending) {
            Modifier
                .pointerInput(Unit) {
                    detectTapGestures { offset ->
                        val rect = currentFittedRect
                        if (offset.x !in rect.left..rect.right || offset.y !in rect.top..rect.bottom) {
                            currentOnSelectMark(null)
                            return@detectTapGestures
                        }
                        val nx = ((offset.x - rect.left) / rect.width).coerceIn(0f, 1f)
                        val ny = ((offset.y - rect.top) / rect.height).coerceIn(0f, 1f)
                        val hit = currentMarks.indexOfLast { it.containsNormalized(nx, ny) }
                        currentOnSelectMark(if (hit >= 0) hit else null)
                    }
                }
                .pointerInput(minDragPx) {
                    detectDragGestures(
                        onDragStart = { start ->
                            val rect = currentFittedRect
                            val clamped = Offset(
                                x = start.x.coerceIn(rect.left, rect.right),
                                y = start.y.coerceIn(rect.top, rect.bottom),
                            )
                            dragStart = clamped
                            dragCurrent = clamped
                        },
                        onDrag = { change, _ ->
                            change.consume()
                            val rect = currentFittedRect
                            dragCurrent = Offset(
                                x = change.position.x.coerceIn(rect.left, rect.right),
                                y = change.position.y.coerceIn(rect.top, rect.bottom),
                            )
                        },
                        onDragCancel = {
                            dragStart = null
                            dragCurrent = null
                        },
                        onDragEnd = {
                            val start = dragStart
                            val end = dragCurrent
                            dragStart = null
                            dragCurrent = null
                            val rect = currentFittedRect
                            if (start != null && end != null && rect.width > 0f && rect.height > 0f) {
                                val startX = start.x.coerceIn(rect.left, rect.right)
                                val startY = start.y.coerceIn(rect.top, rect.bottom)
                                val endX = end.x.coerceIn(rect.left, rect.right)
                                val endY = end.y.coerceIn(rect.top, rect.bottom)
                                val dx = abs(endX - startX)
                                val dy = abs(endY - startY)
                                if (dx > minDragPx && dy > minDragPx) {
                                    currentOnAddMark(
                                        ReportMarkUi.normalized(
                                            x1 = (startX - rect.left) / rect.width,
                                            y1 = (startY - rect.top) / rect.height,
                                            x2 = (endX - rect.left) / rect.width,
                                            y2 = (endY - rect.top) / rect.height,
                                        ),
                                    )
                                }
                            }
                        },
                    )
                }
        } else {
            Modifier
        }

        Canvas(
            modifier = Modifier
                .fillMaxSize()
                .onSizeChanged { canvasSize = it }
                .then(gestureModifier),
        ) {
            val radiusPx = 6.dp.toPx()
            val strokePx = 2.dp.toPx()
            val selectedStrokePx = 3.dp.toPx()
            val drawRect = computeFittedImageRect(
                containerWidth = size.width,
                containerHeight = size.height,
                imageWidth = imageWidthPx,
                imageHeight = imageHeightPx,
            )

            marks.forEachIndexed { index, mark ->
                val isSelected = index == selectedIndex
                val left = drawRect.left + mark.left * drawRect.width
                val top = drawRect.top + mark.top * drawRect.height
                val width = (mark.width * drawRect.width).coerceAtLeast(1f)
                val height = (mark.height * drawRect.height).coerceAtLeast(1f)
                if (mark.redact) {
                    drawRect(
                        color = Color.Black,
                        topLeft = Offset(left, top),
                        size = Size(width, height),
                    )
                    drawRect(
                        color = if (isSelected) colors.accent else colors.hairline,
                        topLeft = Offset(left, top),
                        size = Size(width, height),
                        style = Stroke(width = if (isSelected) selectedStrokePx else strokePx),
                    )
                } else {
                    drawRoundRect(
                        color = colors.accent.copy(alpha = if (isSelected) 0.16f else 0.08f),
                        topLeft = Offset(left, top),
                        size = Size(width, height),
                        cornerRadius = CornerRadius(radiusPx, radiusPx),
                    )
                    drawRoundRect(
                        color = colors.accent,
                        topLeft = Offset(left, top),
                        size = Size(width, height),
                        cornerRadius = CornerRadius(radiusPx, radiusPx),
                        style = Stroke(width = if (isSelected) selectedStrokePx else strokePx),
                    )
                }
            }

            val start = dragStart
            val current = dragCurrent
            if (start != null && current != null) {
                val left = minOf(start.x, current.x).coerceIn(drawRect.left, drawRect.right)
                val top = minOf(start.y, current.y).coerceIn(drawRect.top, drawRect.bottom)
                val right = maxOf(start.x, current.x).coerceIn(drawRect.left, drawRect.right)
                val bottom = maxOf(start.y, current.y).coerceIn(drawRect.top, drawRect.bottom)
                val w = (right - left).coerceAtLeast(1f)
                val h = (bottom - top).coerceAtLeast(1f)
                drawRoundRect(
                    color = colors.accent.copy(alpha = 0.12f),
                    topLeft = Offset(left, top),
                    size = Size(w, h),
                    cornerRadius = CornerRadius(radiusPx, radiusPx),
                )
                drawRoundRect(
                    color = colors.accent,
                    topLeft = Offset(left, top),
                    size = Size(w, h),
                    cornerRadius = CornerRadius(radiusPx, radiusPx),
                    style = Stroke(width = strokePx),
                )
            }
        }

        marks.forEachIndexed { index, mark ->
            val xPx = (fittedRect.left + mark.left * fittedRect.width).roundToInt()
            val yPx = (fittedRect.top + mark.top * fittedRect.height).roundToInt()
            Box(
                modifier = Modifier
                    .offset { IntOffset(xPx, yPx) }
                    .padding(Space.xxs)
                    .size(20.dp)
                    .clip(CircleShape)
                    .background(if (mark.redact) colors.ink else colors.accentFill),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = stringResource(R.string.report_mark_number, index + 1),
                    style = typography.monoSmall,
                    color = if (mark.redact) colors.ground else colors.onAccent,
                    maxLines = 1,
                )
            }
        }
    }
}

@Composable
private fun ReportMarksList(
    marks: List<ReportMarkUi>,
    selectedIndex: Int?,
    editable: Boolean,
    onSelectMark: (Int) -> Unit,
    onUpdateNote: (Int, String) -> Unit,
    onToggleRedact: (Int) -> Unit,
    onRemoveMark: (Int) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography

    AmuxPanel(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.fillMaxWidth()) {
            marks.forEachIndexed { index, mark ->
                if (index > 0) {
                    AmuxInsetHairline(leadingInset = 44.dp)
                }
                val isSelected = index == selectedIndex
                val rowLabel = stringResource(R.string.report_mark_label, index + 1)
                val removeLabel = stringResource(R.string.report_mark_remove, index + 1)
                val redactSpoken = stringResource(
                    if (mark.redact) R.string.report_mark_show_spoken else R.string.report_mark_hide_spoken,
                    index + 1,
                )
                val placeholder = stringResource(R.string.report_mark_note_placeholder)

                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .background(
                            if (isSelected) colors.accent.copy(alpha = 0.08f) else colors.raised,
                        )
                        .clickable(enabled = editable) { onSelectMark(index) }
                        .padding(start = Space.m, end = Space.xs, top = Space.xs, bottom = Space.xs)
                        .semantics {
                            contentDescription = rowLabel
                            selected = isSelected
                        }
                        .testTag(ReportTags.markRow(index)),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    Box(
                        modifier = Modifier
                            .size(22.dp)
                            .clip(CircleShape)
                            .background(if (mark.redact) colors.ink else colors.accent),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(
                            text = stringResource(R.string.report_mark_number, index + 1),
                            style = typography.monoSmall,
                            color = if (mark.redact) colors.ground else colors.onAccent,
                            maxLines = 1,
                        )
                    }

                    Box(
                        modifier = Modifier
                            .weight(1f)
                            .padding(vertical = Space.s),
                    ) {
                        if (mark.note.isEmpty()) {
                            Text(
                                text = placeholder,
                                style = typography.body,
                                color = colors.inkFaint,
                            )
                        }
                        BasicTextField(
                            value = mark.note,
                            onValueChange = { updated ->
                                if (editable) onUpdateNote(index, updated)
                            },
                            enabled = editable,
                            textStyle = typography.body.copy(color = colors.ink),
                            cursorBrush = SolidColor(colors.accent),
                            modifier = Modifier
                                .fillMaxWidth()
                                .semantics { contentDescription = placeholder }
                                .testTag(ReportTags.markNote(index)),
                        )
                    }

                    TextButton(
                        onClick = { onToggleRedact(index) },
                        enabled = editable,
                        modifier = Modifier
                            .semantics { contentDescription = redactSpoken }
                            .testTag(ReportTags.markRedact(index)),
                    ) {
                        Text(
                            text = stringResource(
                                if (mark.redact) R.string.report_mark_hidden else R.string.report_mark_hide,
                            ),
                            style = typography.caption,
                            color = if (mark.redact) colors.ink else colors.inkMuted,
                            maxLines = 1,
                        )
                    }

                    IconButton(
                        onClick = { onRemoveMark(index) },
                        enabled = editable,
                        modifier = Modifier.testTag(ReportTags.markRemove(index)),
                    ) {
                        Icon(
                            imageVector = Icons.Outlined.Close,
                            contentDescription = removeLabel,
                            tint = colors.inkMuted,
                            modifier = Modifier.size(IconSize.Control),
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun ReportComposerDock(
    state: ReportUiState,
    editable: Boolean,
    onAction: (ReportAction) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val isSending = state.delivery is ReportDeliveryUi.Sending
    val failed = state.delivery as? ReportDeliveryUi.Failed
    val canSend = state.signedIn && !state.screenshotPending
    val notePlaceholder = stringResource(R.string.report_note_placeholder)
    val noteShape = RoundedCornerShape(AmuxTheme.metrics.controlRadius)

    Surface(
        modifier = Modifier.fillMaxWidth(),
        color = colors.raised,
        contentColor = colors.ink,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .windowInsetsPadding(WindowInsets.navigationBars.union(WindowInsets.ime)),
        ) {
            AmuxHairline(color = colors.hairline.copy(alpha = 0.7f))

            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.margin, vertical = Space.m),
                verticalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                if (failed != null) {
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(noteShape)
                            .background(colors.sunken)
                            .border(AmuxTheme.metrics.hairline, colors.removed.copy(alpha = 0.45f), noteShape)
                            .padding(start = Space.m, end = Space.xs, top = Space.xs, bottom = Space.xs)
                            .testTag(ReportTags.REFUSAL),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(Space.s),
                    ) {
                        Text(
                            text = stringResource(R.string.report_not_sent, failed.reason),
                            style = typography.detail,
                            color = colors.removed,
                            modifier = Modifier
                                .weight(1f)
                                .padding(vertical = Space.xs),
                        )
                        TextButton(
                            onClick = { onAction(ReportAction.Send) },
                            enabled = canSend && !isSending,
                            modifier = Modifier.testTag(ReportTags.RETRY),
                        ) {
                            Text(
                                text = stringResource(R.string.report_retry),
                                style = typography.bodyEmphasis,
                                color = colors.accent,
                            )
                        }
                    }
                }

                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = 72.dp)
                        .clip(noteShape)
                        .background(AmuxTheme.backdrop)
                        .border(AmuxTheme.metrics.hairline, colors.hairline, noteShape)
                        .padding(Space.m),
                ) {
                    if (state.note.isEmpty()) {
                        Text(
                            text = notePlaceholder,
                            style = typography.body,
                            color = colors.inkFaint,
                        )
                    }
                    BasicTextField(
                        value = state.note,
                        onValueChange = { updated ->
                            if (editable) onAction(ReportAction.SetNote(updated))
                        },
                        enabled = editable,
                        textStyle = typography.body.copy(color = colors.ink),
                        cursorBrush = SolidColor(colors.accent),
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(min = ThumbTarget.MinSize)
                            .semantics { contentDescription = notePlaceholder }
                            .testTag(ReportTags.NOTE),
                    )
                }

                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag(ReportTags.DUMP_STATUS),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.Description,
                        contentDescription = null,
                        tint = colors.inkFaint,
                        modifier = Modifier.size(IconSize.Inline),
                    )
                    val disclosureRes = when {
                        state.screenshotPending -> R.string.report_frame_preparing
                        state.screenshotPresent && (state.dumpReady || state.dumpPending) ->
                            R.string.report_contents_both
                        state.screenshotPresent -> R.string.report_contents_screenshot_only
                        state.dumpReady || state.dumpPending -> R.string.report_contents_dump_only
                        else -> R.string.report_contents_none
                    }
                    Text(
                        text = stringResource(disclosureRes),
                        style = typography.caption,
                        color = colors.inkFaint,
                    )
                }

                if (isSending) {
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(min = ThumbTarget.MinSize)
                            .clip(noteShape)
                            .background(colors.sunken)
                            .padding(horizontal = Space.l, vertical = Space.m)
                            .testTag(ReportTags.SENDING),
                        horizontalArrangement = Arrangement.Center,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        AmuxStateMark(state = RowState.Working, size = 10.dp)
                        Spacer(modifier = Modifier.size(Space.s))
                        Text(
                            text = stringResource(R.string.report_sending),
                            style = typography.bodyEmphasis,
                            color = colors.inkMuted,
                        )
                    }
                } else {
                    AmuxActionButton(
                        label = stringResource(
                            if (failed != null) R.string.report_retry else R.string.report_send,
                        ),
                        kind = ActionButtonKind.Primary,
                        fillWidth = true,
                        enabled = canSend,
                        onClick = { onAction(ReportAction.Send) },
                        modifier = Modifier.testTag(ReportTags.SEND),
                    )
                }
            }
        }
    }
}

/**
 * It arrived. The page says so, gives the reference somebody quotes when they
 * ask about it with one way to copy it, and leaves closing to the top bar.
 */
@Composable
private fun ReportSentContent(
    referenceId: String,
    onCopyReference: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    var copied by remember(referenceId) { mutableStateOf(false) }
    val copyLabel = stringResource(if (copied) R.string.report_copied else R.string.report_copy_reference)

    Column(
        modifier = modifier
            .padding(horizontal = Space.margin, vertical = Space.l)
            .windowInsetsPadding(WindowInsets.navigationBars)
            .testTag(ReportTags.SENT),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Text(
            text = stringResource(R.string.report_sent_title),
            style = typography.screenTitle,
            color = colors.ink,
        )
        Text(
            text = stringResource(R.string.report_sent_detail),
            style = typography.body,
            color = colors.inkMuted,
        )
        // The reference and the press that copies it are separate, so the
        // reference stays text of its own to read or select.
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = ThumbTarget.MinSize),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            SelectionContainer(modifier = Modifier.weight(1f, fill = false)) {
                Text(
                    text = referenceId,
                    style = typography.mono,
                    color = colors.ink,
                    modifier = Modifier.testTag(ReportTags.RECEIPT_ID),
                )
            }
            Row(
                modifier = Modifier
                    .spillTarget()
                    .clickable(role = Role.Button) {
                        copied = true
                        onCopyReference()
                    }
                    .testTag(ReportTags.COPY_RECEIPT),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.xs),
            ) {
                Icon(
                    imageVector = if (copied) Icons.Outlined.Check else Icons.Outlined.ContentCopy,
                    contentDescription = null,
                    tint = colors.accent,
                    modifier = Modifier.size(IconSize.Inline),
                )
                Text(
                    text = copyLabel,
                    style = typography.detail,
                    color = colors.accent,
                )
            }
        }
    }
}
