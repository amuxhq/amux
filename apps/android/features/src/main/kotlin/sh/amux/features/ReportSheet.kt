package sh.amux.features

import android.graphics.BitmapFactory
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
import androidx.compose.foundation.layout.statusBarsPadding
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
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import kotlin.math.abs
import kotlin.math.roundToInt
import sh.amux.design.AmuxChrome
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
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
    val diagnosticDumpAttached: Boolean = true,
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
            diagnosticDumpAttached == other.diagnosticDumpAttached &&
            locked == other.locked &&
            delivery == other.delivery
    }

    override fun hashCode(): Int {
        var result = imageBytes?.contentHashCode() ?: 0
        result = 31 * result + frameAspectRatio.hashCode()
        result = 31 * result + note.hashCode()
        result = 31 * result + marks.hashCode()
        result = 31 * result + signedIn.hashCode()
        result = 31 * result + diagnosticDumpAttached.hashCode()
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
                modifier = Modifier.size(20.dp),
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
                modifier = Modifier
                    .thumbTarget()
                    .testTag(ReportTags.OFFER_OPEN),
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
                modifier = Modifier
                    .thumbTarget()
                    .testTag(ReportTags.OFFER_DISMISS),
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
        Column(
            modifier = Modifier
                .fillMaxSize()
                .statusBarsPadding(),
        ) {
            // Header bar
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.xs, vertical = Space.xs),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                IconButton(
                    onClick = { onAction(ReportAction.Dismiss) },
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(ReportTags.CLOSE),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.Close,
                        contentDescription = closeLabel,
                        tint = colors.inkMuted,
                    )
                }
                Text(
                    text = stringResource(R.string.report_title),
                    style = typography.identifier,
                    color = colors.ink,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier
                        .weight(1f)
                        .semantics { heading() }
                        .padding(end = Space.l),
                )
            }
            AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))

            when (val delivery = state.delivery) {
                is ReportDeliveryUi.Sent -> {
                    ReportSentContent(
                        referenceId = delivery.referenceId,
                        onCopyReference = { onAction(ReportAction.CopyReference(delivery.referenceId)) },
                        onDone = { onAction(ReportAction.Dismiss) },
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
                                modifier = Modifier
                                    .padding(horizontal = Space.xl)
                                    .heightIn(max = 320.dp),
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
    remember(imageBytes) {
        imageBytes
            ?.takeIf { it.isNotEmpty() }
            ?.let { bytes ->
                runCatching {
                    BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
                }.getOrNull()
            }
    }

@Composable
private fun ReportFrameCanvas(
    imageBytes: ByteArray?,
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

    BoxWithConstraints(
        modifier = modifier
            .aspectRatio(aspectRatio, matchHeightConstraintsFirst = aspectRatio < 1f)
            .clip(shape)
            .background(colors.sunken)
            .border(AmuxTheme.metrics.hairline, colors.hairline, shape)
            .semantics { stateDescription = markedSummary }
            .testTag(ReportTags.FRAME),
    ) {
        val frameWidthPx = constraints.maxWidth.toFloat().coerceAtLeast(1f)
        val frameHeightPx = constraints.maxHeight.toFloat().coerceAtLeast(1f)

        if (bitmap != null) {
            Image(
                bitmap = bitmap,
                contentDescription = null,
                contentScale = ContentScale.Fit,
                modifier = Modifier.fillMaxSize(),
            )
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

        val gestureModifier = if (editable) {
            Modifier
                .pointerInput(marks, frameWidthPx, frameHeightPx) {
                    detectTapGestures { offset ->
                        val nx = (offset.x / frameWidthPx).coerceIn(0f, 1f)
                        val ny = (offset.y / frameHeightPx).coerceIn(0f, 1f)
                        val hit = marks.indexOfLast { it.containsNormalized(nx, ny) }
                        onSelectMark(if (hit >= 0) hit else null)
                    }
                }
                .pointerInput(frameWidthPx, frameHeightPx, minDragPx) {
                    detectDragGestures(
                        onDragStart = { start ->
                            dragStart = start
                            dragCurrent = start
                        },
                        onDrag = { change, _ ->
                            change.consume()
                            dragCurrent = change.position
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
                            if (start != null && end != null) {
                                val dx = abs(end.x - start.x)
                                val dy = abs(end.y - start.y)
                                if (dx > minDragPx && dy > minDragPx) {
                                    onAddMark(
                                        ReportMarkUi.normalized(
                                            x1 = start.x / frameWidthPx,
                                            y1 = start.y / frameHeightPx,
                                            x2 = end.x / frameWidthPx,
                                            y2 = end.y / frameHeightPx,
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
                .then(gestureModifier),
        ) {
            val radiusPx = 6.dp.toPx()
            val strokePx = 2.dp.toPx()
            val selectedStrokePx = 3.dp.toPx()

            marks.forEachIndexed { index, mark ->
                val isSelected = index == selectedIndex
                val left = mark.left * size.width
                val top = mark.top * size.height
                val width = (mark.width * size.width).coerceAtLeast(1f)
                val height = (mark.height * size.height).coerceAtLeast(1f)
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
                val left = minOf(start.x, current.x).coerceIn(0f, size.width)
                val top = minOf(start.y, current.y).coerceIn(0f, size.height)
                val right = maxOf(start.x, current.x).coerceIn(0f, size.width)
                val bottom = maxOf(start.y, current.y).coerceIn(0f, size.height)
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
            val xPx = (mark.left * frameWidthPx).roundToInt()
            val yPx = (mark.top * frameHeightPx).roundToInt()
            Box(
                modifier = Modifier
                    .offset { IntOffset(xPx, yPx) }
                    .padding(Space.xxs)
                    .size(20.dp)
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
                        .semantics { contentDescription = rowLabel }
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
                            .thumbTarget()
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
                        modifier = Modifier
                            .thumbTarget()
                            .testTag(ReportTags.markRemove(index)),
                    ) {
                        Icon(
                            imageVector = Icons.Outlined.Close,
                            contentDescription = removeLabel,
                            tint = colors.inkMuted,
                            modifier = Modifier.size(18.dp),
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
                            enabled = state.signedIn && !isSending,
                            modifier = Modifier
                                .thumbTarget()
                                .testTag(ReportTags.RETRY),
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
                        .background(colors.ground)
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
                        modifier = Modifier.size(16.dp),
                    )
                    val hasScreenshot = state.imageBytes?.isNotEmpty() == true
                    val disclosureRes = when {
                        hasScreenshot && state.diagnosticDumpAttached -> R.string.report_contents_both
                        !hasScreenshot && state.diagnosticDumpAttached -> R.string.report_contents_dump_only
                        hasScreenshot && !state.diagnosticDumpAttached -> R.string.report_contents_screenshot_only
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
                            .thumbTarget()
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
                        enabled = state.signedIn,
                        onClick = { onAction(ReportAction.Send) },
                        modifier = Modifier.testTag(ReportTags.SEND),
                    )
                }
            }
        }
    }
}

@Composable
private fun ReportSentContent(
    referenceId: String,
    onCopyReference: () -> Unit,
    onDone: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    var copied by remember(referenceId) { mutableStateOf(false) }

    Column(
        modifier = modifier
            .padding(horizontal = Space.margin, vertical = Space.xl)
            .windowInsetsPadding(WindowInsets.navigationBars)
            .testTag(ReportTags.SENT),
        verticalArrangement = Arrangement.spacedBy(Space.l),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
            Icon(
                imageVector = Icons.Outlined.CheckCircle,
                contentDescription = null,
                tint = colors.added,
                modifier = Modifier.size(28.dp),
            )
            Text(
                text = stringResource(R.string.report_sent_title),
                style = typography.screenTitle,
                color = colors.ink,
            )
            Text(
                text = stringResource(R.string.report_sent_detail, referenceId),
                style = typography.body,
                color = colors.inkMuted,
            )
        }

        AmuxPanel(modifier = Modifier.fillMaxWidth()) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(start = Space.m, end = Space.xs, top = Space.s, bottom = Space.s),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                Column(
                    modifier = Modifier.weight(1f),
                    verticalArrangement = Arrangement.spacedBy(Space.xxs),
                ) {
                    Text(
                        text = stringResource(R.string.report_reference_label),
                        style = typography.caption,
                        color = colors.inkMuted,
                    )
                    SelectionContainer {
                        Text(
                            text = referenceId,
                            style = typography.mono,
                            color = colors.ink,
                            modifier = Modifier.testTag(ReportTags.RECEIPT_ID),
                        )
                    }
                }
                TextButton(
                    onClick = {
                        copied = true
                        onCopyReference()
                    },
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(ReportTags.COPY_RECEIPT),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.ContentCopy,
                        contentDescription = null,
                        tint = colors.ink,
                        modifier = Modifier.size(16.dp),
                    )
                    Spacer(modifier = Modifier.size(Space.xs))
                    Text(
                        text = stringResource(
                            if (copied) R.string.report_copied else R.string.report_copy_reference,
                        ),
                        style = typography.bodyEmphasis,
                        color = colors.ink,
                    )
                }
            }
        }

        AmuxActionButton(
            label = stringResource(R.string.report_done),
            kind = ActionButtonKind.Primary,
            fillWidth = true,
            onClick = onDone,
            modifier = Modifier.testTag(ReportTags.DONE),
        )
    }
}
