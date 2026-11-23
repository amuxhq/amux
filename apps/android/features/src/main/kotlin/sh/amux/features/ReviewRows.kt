package sh.amux.features

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.ModeComment
import androidx.compose.material.icons.outlined.MoveDown
import androidx.compose.material.icons.outlined.Sync
import androidx.compose.material.icons.outlined.VerticalAlignBottom
import androidx.compose.material.icons.outlined.VerticalAlignTop
import androidx.compose.material.icons.outlined.WarningAmber
import androidx.compose.material.icons.outlined.WifiOff
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxColors
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxShapes
import sh.amux.design.AmuxTheme
import sh.amux.design.IconSize
import sh.amux.design.Space
import sh.amux.design.ThumbTarget
import sh.amux.design.thumbTarget
import sh.amux.values.LineKind
import sh.amux.values.ReviewBanner
import sh.amux.values.ReviewCodeLine
import sh.amux.values.ReviewCommentAt
import sh.amux.values.ReviewCommentRow
import sh.amux.values.ReviewCommentState
import sh.amux.values.ReviewContextAsk
import sh.amux.values.ReviewFileHead
import sh.amux.values.ReviewGap
import sh.amux.values.ReviewHunkHead
import sh.amux.words.ChatWords
import sh.amux.words.RowState

// The review page's rows, each drawn from one of the shared page's rows as
// handed: the page and a sent review's round draw the same ones.

/** Test tags of the review page and its round. */
object ReviewTags {
    const val SCREEN: String = "review"
    const val BACK: String = "review.back"
    const val SUMMARY: String = "review.summary"
    const val FILES: String = "review.files"
    const val MORE: String = "review.more"
    const val REFRESH: String = "review.refresh"
    const val DISCARD: String = "review.discard"
    const val DISCARD_CONFIRM: String = "review.discard.confirm"
    const val START_OVER: String = "review.startOver"
    const val START_OVER_CONFIRM: String = "review.startOver.confirm"
    const val VIEWS: String = "review.views"
    const val BANNER: String = "review.banner"
    const val BANNER_ACTION: String = "review.banner.action"
    const val EMPTY: String = "review.empty"
    const val FILE: String = "review.file"
    const val FILE_COMMENT: String = "review.file.comment"
    const val FILE_FOLD: String = "review.file.fold"
    const val COMMENT: String = "review.comment"
    const val HUNK: String = "review.hunk"
    const val GAP: String = "review.gap"
    const val GAP_UP: String = "review.gap.up"
    const val GAP_DOWN: String = "review.gap.down"
    const val LOADING: String = "review.loading"
    const val NO_LINES: String = "review.noLines"
    const val NOTICE: String = "review.notice"
    const val DONE: String = "review.done"
    const val DESELECT: String = "review.deselect"
    const val COMMENT_ON: String = "review.commentOn"
    const val SHEET: String = "review.sheet"
    const val NOTE: String = "review.note"
    const val ADD: String = "review.add"
    const val SAVE: String = "review.save"
    const val DELETE: String = "review.delete"
    const val CANCEL: String = "review.cancel"
    const val ROUND: String = "review.round"
    const val ROUND_DONE: String = "review.round.done"
    const val ROUND_FILE: String = "review.round.file"
    const val ROUND_COMMENT: String = "review.round.comment"
    const val ROUND_GONE: String = "review.round.gone"
    const val ROUND_TEXT: String = "review.round.text"
    const val ROUND_OPEN: String = "review.round.open"

    /** One tag per line, its row's key, so each line can be reached on its own, as TalkBack reaches it. */
    fun line(key: String): String = "review.line.$key"

    fun filesItem(path: String): String = "review.files.$path"
}

/** A width given in dp at the default text size that grows with the text, as the code it lines up with does. */
@Composable
private fun scaled(width: Dp): Dp = width * LocalDensity.current.fontScale

/** The number column fits four digits at the size the code is drawn. */
private val NumberColumn = 32.dp

/** The mark column fits one sign. */
private val MarkColumn = 12.dp

/**
 * A file's head: the fold, its path, what it changed and how many comments
 * it has, and under the path what a reader may want to know before opening
 * it. Pinned at the top while its file is read; a press folds or opens it,
 * and holding it offers a comment on the whole file.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun ReviewFileHeadRow(
    head: ReviewFileHead,
    modifier: Modifier = Modifier,
    selected: Boolean = false,
    /** Its counts are drawn: a round lists files only for their comments. */
    tally: Boolean = true,
    /** Whether it changed since the last round is said: on all changes, not on the view that lists only those. */
    marksSince: Boolean = true,
    onToggle: (() -> Unit)? = null,
    onCommentOnFile: (() -> Unit)? = null,
) {
    val colors = AmuxTheme.colors
    val binary = stringResource(R.string.review_binary_file)
    val about = remember(head, tally, marksSince, binary) { fileAbout(head, tally, marksSince, binary) }
    var menuOpen by remember { mutableStateOf(false) }
    val commentLabel = stringResource(R.string.review_comment_on_file)
    val foldLabel = stringResource(if (head.folded) R.string.review_open_file else R.string.review_fold_file)
    val state = stringResource(if (head.folded) R.string.fold_state_folded else R.string.fold_state_open)
    val spoken = listOfNotNull(head.path, about).joinToString(", ")
    val selectedBar = colors.accent
    Box(modifier = modifier) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .background(if (selected) colors.accent.copy(alpha = 0.18f) else colors.ground)
                .drawBehind {
                    if (selected) drawRect(selectedBar, size = Size(3.dp.toPx(), size.height))
                }
                .then(
                    if (onToggle != null) {
                        Modifier.combinedClickable(
                            role = Role.Button,
                            onClick = onToggle,
                            onLongClick = if (onCommentOnFile != null) ({ menuOpen = true }) else null,
                            onLongClickLabel = commentLabel.takeIf { onCommentOnFile != null },
                        )
                    } else {
                        Modifier
                    },
                )
                .semantics(mergeDescendants = true) {
                    contentDescription = spoken
                    if (onToggle != null) stateDescription = state
                    if (onToggle != null) this.selected = selected
                    if (onCommentOnFile != null) {
                        customActions = listOf(
                            CustomAccessibilityAction(commentLabel) {
                                onCommentOnFile()
                                true
                            },
                        )
                    }
                }
                .heightIn(min = ThumbTarget.MinSize)
                .padding(horizontal = AmuxTheme.metrics.gutter, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            if (onToggle != null) {
                Icon(
                    imageVector = if (head.folded) {
                        Icons.AutoMirrored.Outlined.KeyboardArrowRight
                    } else {
                        Icons.Outlined.KeyboardArrowDown
                    },
                    contentDescription = null,
                    tint = colors.inkFaint,
                    modifier = Modifier.size(IconSize.Inline),
                )
            }
            Column(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(Space.xxs),
            ) {
                Text(
                    text = head.path,
                    style = AmuxTheme.typography.identifier,
                    color = if (head.inChange || !tally) colors.ink else colors.inkMuted,
                    maxLines = if (LocalDensity.current.fontScale >= LARGE_TEXT_SCALE) 4 else 2,
                    overflow = TextOverflow.Ellipsis,
                )
                if (about != null) {
                    Text(
                        text = about,
                        style = AmuxTheme.typography.caption,
                        color = colors.inkFaint,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            if (head.comments > 0) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.xxs),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.ModeComment,
                        contentDescription = null,
                        tint = colors.accent,
                        modifier = Modifier.size(IconSize.Small),
                    )
                    Text(
                        text = head.comments.toString(),
                        style = AmuxTheme.typography.caption,
                        color = colors.accent,
                    )
                }
            }
            if (tally) {
                DiffDeltaCounts(added = head.added, removed = head.removed)
            }
        }
        AmuxHairline(modifier = Modifier.align(Alignment.BottomStart))
        if (onCommentOnFile != null && onToggle != null) {
            DropdownMenu(
                expanded = menuOpen,
                onDismissRequest = { menuOpen = false },
                containerColor = colors.raised,
            ) {
                DropdownMenuItem(
                    text = { Text(text = commentLabel, color = colors.ink) },
                    leadingIcon = { Icon(Icons.Outlined.ModeComment, contentDescription = null, tint = colors.inkMuted) },
                    onClick = {
                        menuOpen = false
                        onCommentOnFile()
                    },
                    modifier = Modifier.testTag(ReviewTags.FILE_COMMENT),
                )
                DropdownMenuItem(
                    text = { Text(text = foldLabel, color = colors.ink) },
                    leadingIcon = {
                        Icon(
                            imageVector = if (head.folded) {
                                Icons.Outlined.KeyboardArrowDown
                            } else {
                                Icons.AutoMirrored.Outlined.KeyboardArrowRight
                            },
                            contentDescription = null,
                            tint = colors.inkMuted,
                        )
                    },
                    onClick = {
                        menuOpen = false
                        onToggle()
                    },
                    modifier = Modifier.testTag(ReviewTags.FILE_FOLD),
                )
            }
        }
    }
}

/**
 * "Renamed from old.rs · Generated · Changed since your last review", or
 * that the file is listed only for its comments.
 */
private fun fileAbout(head: ReviewFileHead, tally: Boolean, marksSince: Boolean, binary: String): String? {
    val parts = mutableListOf<String>()
    head.oldPath?.let { parts += ChatWords.reviewRenamedFrom(it) }
    parts += head.flags.map(ChatWords::reviewFlag)
    if (head.binary) parts += binary
    if (!head.inChange && tally) parts += ChatWords.REVIEW_OUTSIDE_CHANGE
    if (head.changedSinceLast && marksSince) parts += ChatWords.REVIEW_CHANGED_SINCE_LAST
    return parts.takeIf { it.isNotEmpty() }?.joinToString(" · ")
}

/**
 * Where a hunk sits in the code: a sunken band with "⋯" in the number column
 * and the definitions its changes are in, outermost first.
 */
@Composable
internal fun ReviewHunkBand(hunk: ReviewHunkHead, modifier: Modifier = Modifier) {
    val colors = AmuxTheme.colors
    val scope = if (hunk.scope.isEmpty()) {
        ChatWords.reviewLines(hunk.newStart, hunk.newStart)
    } else {
        hunk.scope.joinToString(" › ")
    }
    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(colors.sunken)
            .padding(start = Space.xs, end = Space.m, top = 5.dp, bottom = 5.dp)
            .clearAndSetSemantics { contentDescription = scope }
            .testTag(ReviewTags.HUNK),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = stringResource(R.string.review_elided),
            style = AmuxTheme.typography.monoSmall,
            color = colors.inkFaint,
            textAlign = TextAlign.End,
            modifier = Modifier
                .width(scaled(NumberColumn))
                .padding(end = Space.s + scaled(MarkColumn)),
        )
        Text(
            text = scope,
            style = AmuxTheme.typography.monoSmall,
            color = colors.inkFaint,
            maxLines = 1,
            overflow = TextOverflow.StartEllipsis,
            modifier = Modifier.weight(1f),
        )
    }
}

/**
 * One line of code: its number, its mark, and its text in the colours of its
 * tokens on the change's green or red, with the words an edited line changed
 * marked more strongly. A removed line is numbered as a line of the commit
 * compared against, every other as the snapshot's. A long line wraps under
 * its own text, not under the mark.
 *
 * A diff reads as a listing, one line under the next, so each line is drawn
 * its text's height rather than grown to a 48 dp target: it is as wide as
 * the screen, and a hunk grown line by line would spread over pages.
 */
@Composable
internal fun ReviewCodeLineRow(
    line: ReviewCodeLine,
    modifier: Modifier = Modifier,
    selected: Boolean = false,
) {
    val colors = AmuxTheme.colors
    val words = remember(line, colors) { codeWords(line, colors) }
    val added = stringResource(R.string.review_line_added)
    val removed = stringResource(R.string.review_line_removed)
    val spoken = remember(line, added, removed) { spokenLine(line, added, removed) }
    val tint = when {
        selected -> colors.accent.copy(alpha = 0.18f)
        line.kind == LineKind.Added -> colors.addedTint
        line.kind == LineKind.Removed -> colors.removedTint
        line.extra -> colors.sunken.copy(alpha = 0.5f)
        else -> Color.Transparent
    }
    val bar: Pair<Color, Dp>? = when {
        selected -> colors.accent to 3.dp
        line.commented -> colors.accent.copy(alpha = 0.6f) to 2.dp
        else -> null
    }
    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(tint)
            .drawBehind {
                if (bar != null) drawRect(bar.first, topLeft = Offset.Zero, size = Size(bar.second.toPx(), size.height))
            }
            // Read whole, a line at a time; whether it is selected is said
            // where a line can be selected, which a sent round's cannot.
            .semantics { contentDescription = spoken }
            .padding(start = Space.xs, end = Space.m, top = 1.dp, bottom = 1.dp),
    ) {
        Text(
            text = (line.newLine ?: line.oldLine)?.toString().orEmpty(),
            style = AmuxTheme.typography.monoSmall,
            color = if (line.commented) colors.accent else colors.inkFaint,
            maxLines = 1,
            textAlign = TextAlign.End,
            modifier = Modifier
                .width(scaled(NumberColumn))
                .padding(end = Space.s),
        )
        Text(
            text = when (line.kind) {
                LineKind.Added -> "+"
                LineKind.Removed -> "−"
                LineKind.Context -> ""
            },
            style = AmuxTheme.typography.monoSmall,
            color = when (line.kind) {
                LineKind.Added -> colors.added
                LineKind.Removed -> colors.removed
                LineKind.Context -> Color.Transparent
            },
            modifier = Modifier.width(scaled(MarkColumn)),
        )
        Text(
            text = words,
            style = AmuxTheme.typography.monoSmall,
            color = colors.ink,
            modifier = Modifier.weight(1f),
        )
    }
}

/** The runs in their token colours; a changed run sits on a stronger tint of its side's colour. */
private fun codeWords(line: ReviewCodeLine, colors: AmuxColors): AnnotatedString {
    if (line.runs.isEmpty()) return AnnotatedString(line.text.ifEmpty { " " })
    val side = if (line.kind == LineKind.Removed) colors.removed else colors.added
    return buildAnnotatedString {
        for (run in line.runs) {
            val style = SpanStyle(
                color = colors.ink(run.kind),
                background = if (run.changed) side.copy(alpha = 0.30f) else Color.Unspecified,
            )
            withStyle(style) { append(run.text) }
        }
    }
}

/** "Line 41, added: let a = 2;" */
private fun spokenLine(line: ReviewCodeLine, added: String, removed: String): String {
    val number = (line.newLine ?: line.oldLine)?.let { ChatWords.reviewLines(it, it) }
    val kind = when (line.kind) {
        LineKind.Added -> added
        LineKind.Removed -> removed
        LineKind.Context -> null
    }
    val head = listOfNotNull(number, kind).joinToString(", ")
    return if (head.isEmpty()) line.text else "$head: ${line.text}"
}

/**
 * Lines of the file not shown, and the asks that load more of them: up to
 * the definition the code below starts in, or a step, and down likewise.
 */
@Composable
internal fun ReviewGapRow(
    gap: ReviewGap,
    onLoad: (ReviewContextAsk) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val words = ChatWords.reviewGap(gap)
    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(colors.sunken)
            .heightIn(min = ThumbTarget.MinSize)
            .padding(start = Space.xs, end = Space.s)
            .semantics(mergeDescendants = false) {
                if (gap.loading) stateDescription = "loading"
            }
            .testTag(ReviewTags.GAP),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Text(
            text = stringResource(R.string.review_elided),
            style = AmuxTheme.typography.monoSmall,
            color = colors.inkMuted,
            textAlign = TextAlign.End,
            modifier = Modifier
                .width(scaled(NumberColumn))
                .padding(end = Space.xs),
        )
        if (gap.loading) {
            AmuxStateMark(state = RowState.Working)
            Text(
                text = stringResource(R.string.review_loading),
                style = AmuxTheme.typography.caption,
                color = colors.inkMuted,
            )
        } else {
            Text(
                text = words,
                style = AmuxTheme.typography.caption,
                color = colors.inkMuted,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Spacer(modifier = Modifier.weight(1f))
        if (!gap.loading) {
            gap.up?.let { GapAsk(ask = it, up = true, onLoad = onLoad) }
            gap.down?.let { GapAsk(ask = it, up = false, onLoad = onLoad) }
        }
    }
}

@Composable
private fun GapAsk(ask: ReviewContextAsk, up: Boolean, onLoad: (ReviewContextAsk) -> Unit) {
    val colors = AmuxTheme.colors
    val words = ChatWords.reviewMore(ask, up)
    Box(
        modifier = Modifier
            .thumbTarget()
            .clip(AmuxShapes.Pill)
            .clickable(role = Role.Button) { onLoad(ask) }
            .semantics { contentDescription = words }
            .testTag(if (up) ReviewTags.GAP_UP else ReviewTags.GAP_DOWN),
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier = Modifier
                .size(width = 36.dp, height = 30.dp)
                .background(colors.raised, AmuxShapes.Pill),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                imageVector = if (up) Icons.Outlined.VerticalAlignTop else Icons.Outlined.VerticalAlignBottom,
                contentDescription = null,
                tint = colors.ink,
                modifier = Modifier.size(IconSize.Inline),
            )
        }
    }
}

/**
 * A comment where the page placed it: under its lines, under its file's
 * head, or folded when outdated. A draft's bar is the accent, a sent one's
 * muted. One not drawn under its lines shows the code it was written on, as
 * stored with it.
 */
@Composable
internal fun ReviewCommentCard(
    comment: ReviewCommentRow,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val folded = stringResource(R.string.review_folded)
    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(colors.raised)
            .height(IntrinsicSize.Min)
            .padding(start = AmuxTheme.metrics.gutter, end = 14.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Box(
            modifier = Modifier
                .width(3.dp)
                .fillMaxHeight()
                .background(if (comment.state == ReviewCommentState.Draft) colors.accent else colors.inkMuted),
        )
        Column(
            modifier = Modifier
                .weight(1f)
                .padding(vertical = Space.s),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(
                text = commentHeading(comment),
                style = AmuxTheme.typography.caption,
                color = colors.inkMuted,
                maxLines = if (comment.folded) 1 else 2,
                overflow = TextOverflow.Ellipsis,
            )
            if (comment.folded) {
                Text(
                    text = comment.text,
                    style = AmuxTheme.typography.body,
                    color = colors.inkMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            } else {
                if (comment.snippet.isNotEmpty()) {
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(6.dp))
                            .background(colors.ground),
                    ) {
                        if (comment.scope.isNotEmpty()) {
                            Text(
                                text = comment.scope.joinToString(" › "),
                                style = AmuxTheme.typography.monoSmall,
                                color = colors.inkFaint,
                                maxLines = 1,
                                overflow = TextOverflow.StartEllipsis,
                                modifier = Modifier.padding(horizontal = 6.dp, vertical = 3.dp),
                            )
                        }
                        comment.snippet.forEach { line -> ReviewCodeLineRow(line = line) }
                    }
                }
                Text(
                    text = comment.text,
                    style = AmuxTheme.typography.body,
                    color = colors.ink,
                    modifier = Modifier.fillMaxWidth(),
                )
                if (comment.changedNear) {
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(Space.xs),
                    ) {
                        Icon(
                            imageVector = Icons.Outlined.Info,
                            contentDescription = null,
                            tint = colors.accent,
                            modifier = Modifier.size(IconSize.Small),
                        )
                        Text(
                            text = ChatWords.REVIEW_CHANGED_NEAR,
                            style = AmuxTheme.typography.caption,
                            color = colors.accent,
                        )
                    }
                }
            }
        }
        if (comment.folded) {
            Icon(
                imageVector = Icons.Outlined.KeyboardArrowDown,
                contentDescription = folded,
                tint = colors.inkFaint,
                modifier = Modifier
                    .padding(top = 10.dp)
                    .size(IconSize.Small),
            )
        }
    }
}

/**
 * "Draft", "Sent in round 2 · Outdated", "Draft · On the file · Lines
 * 41–42": its state, and where it sits when not under its lines.
 */
internal fun commentHeading(comment: ReviewCommentRow): String {
    val parts = mutableListOf(ChatWords.reviewCommentState(comment.state))
    ChatWords.reviewCommentAt(comment.at)?.let { parts += it }
    val at = comment.at
    if (at is ReviewCommentAt.Lines && comment.snippet.isNotEmpty()) {
        parts += ChatWords.reviewLines(at.start, at.end)
    }
    return parts.joinToString(" · ")
}

/** What a test or TalkBack reads of a comment: its state, where it sits, and whether it is folded. */
internal fun commentValue(comment: ReviewCommentRow, folded: String): String {
    val parts = mutableListOf(ChatWords.reviewCommentState(comment.state))
    ChatWords.reviewCommentAt(comment.at)?.let { parts += it }
    if (comment.folded) parts += folded
    return parts.joinToString(" · ")
}

/**
 * What the page says about itself above the files, with what can be done
 * about it when anything: a refresh, or trying a snapshot again.
 */
@Composable
internal fun ReviewBannerCard(
    banner: ReviewBanner,
    modifier: Modifier = Modifier,
    onAct: (() -> Unit)? = null,
) {
    val colors = AmuxTheme.colors
    val words = ChatWords.reviewBanner(banner)
    // The banners that say the page is behind the code catch the eye.
    val warns = when (banner) {
        ReviewBanner.ChangedSinceYouStarted, ReviewBanner.ChangedSince,
        ReviewBanner.MergeUnderway, is ReviewBanner.SnapshotFailed,
        -> true
        else -> false
    }
    Column(
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
            .background(colors.raised)
            .padding(14.dp)
            .testTag(ReviewTags.BANNER),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Icon(
                imageVector = bannerGlyph(banner),
                contentDescription = null,
                tint = if (warns) colors.accent else colors.inkMuted,
                modifier = Modifier
                    .padding(top = Space.xxs)
                    .size(IconSize.Inline),
            )
            Text(
                text = words,
                style = AmuxTheme.typography.detail,
                color = colors.ink,
                modifier = Modifier.weight(1f),
            )
        }
        val action = ChatWords.reviewBannerAction(banner)
        if (onAct != null && action != null) {
            AmuxActionButton(
                label = action,
                kind = ActionButtonKind.Outline,
                onClick = onAct,
                modifier = Modifier.testTag(ReviewTags.BANNER_ACTION),
            )
        }
    }
}

private fun bannerGlyph(banner: ReviewBanner): ImageVector = when (banner) {
    ReviewBanner.ChangedSinceYouStarted, ReviewBanner.ChangedSince -> Icons.Outlined.Sync
    is ReviewBanner.MovedDrafts -> Icons.Outlined.MoveDown
    ReviewBanner.MergeUnderway -> Icons.Outlined.WarningAmber
    is ReviewBanner.SnapshotFailed -> Icons.Outlined.ErrorOutline
    ReviewBanner.Unreachable -> Icons.Outlined.WifiOff
    ReviewBanner.NotOffered, ReviewBanner.NotARepository, ReviewBanner.RoundGone -> Icons.Outlined.Info
}

/** A line saying the file's body is on its way, or why it has no lines. */
@Composable
internal fun ReviewNote(
    text: String,
    modifier: Modifier = Modifier,
    loading: Boolean = false,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = AmuxTheme.metrics.gutter, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        if (loading) AmuxStateMark(state = RowState.Working)
        Text(
            text = text,
            style = AmuxTheme.typography.caption,
            color = colors.inkFaint,
        )
    }
}
