package sh.amux.features

import sh.amux.design.IconSize
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.ModeComment
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.key
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxChrome
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxScrim
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.DiffLine
import sh.amux.values.LineKind
import sh.amux.values.ReviewComment
import sh.amux.values.ReviewDoc
import sh.amux.values.ReviewFile
import sh.amux.words.ChatWords

/** Pure UI state rendered by [ReviewPage]. */
@Immutable
data class ReviewPageState(
    val doc: ReviewDoc? = null,
    val comments: List<ReviewComment> = emptyList(),
    val selectedFileIndex: Int = 0,
    val loading: Boolean = false,
    val error: String? = null,
)

/** Actions emitted by [ReviewPage]. */
sealed interface ReviewPageAction {
    data object Back : ReviewPageAction
    data object Retry : ReviewPageAction
    data class SelectFile(val fileIndex: Int) : ReviewPageAction
    data class AddComment(
        val path: String,
        val line: Int,
        val oldLine: Int,
        val text: String,
    ) : ReviewPageAction

    data class EditComment(
        val index: Int,
        val text: String,
    ) : ReviewPageAction

    data class DeleteComment(val index: Int) : ReviewPageAction
    data object SendReview : ReviewPageAction
}

private sealed interface CommentDraftTarget {
    data class New(
        val path: String,
        val line: Int,
        val oldLine: Int,
    ) : CommentDraftTarget

    data class Editing(
        val index: Int,
        val text: String,
    ) : CommentDraftTarget
}

/**
 * Full-screen diff review page.
 *
 * Displays a left-aligned [TopAppBar] with back navigation, a horizontal
 * scrollable file strip with `+N −N` counts and comment badges, `GeistMono`
 * diff hunks with old/new line numbers and subtle diff tints, and an inline
 * comment composer for adding, editing, and deleting line or file comments.
 *
 * The comparison is the one the page was opened on and stays fixed while it
 * is open, as on iPhone: comments are written against one frozen diff, and a
 * different comparison is a different review, opened from the chat.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ReviewPage(
    state: ReviewPageState,
    onAction: (ReviewPageAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val files = state.doc?.files.orEmpty()
    var activeFileIndex by remember(state.selectedFileIndex, files.size) {
        mutableIntStateOf(
            if (files.isEmpty()) 0 else state.selectedFileIndex.coerceIn(0, files.lastIndex),
        )
    }
    var commentTarget by remember { mutableStateOf<CommentDraftTarget?>(null) }

    // A comment is bound to the diff it was written on; a reloaded diff drops it.
    LaunchedEffect(state.doc?.head, state.doc?.base) {
        commentTarget = null
    }

    BackHandler(enabled = commentTarget != null) {
        commentTarget = null
    }

    val commentCount = state.comments.size
    val commentsByFile = remember(state.comments) {
        state.comments.groupingBy { it.path }.eachCount()
    }
    val fileLabels = remember(files) { shortFileLabels(files.map { it.path }) }

    Scaffold(
        modifier = modifier
            .fillMaxSize()
            .testTag(AskReviewTags.REVIEW_SCREEN),
        containerColor = colors.ground,
        contentColor = colors.ink,
        topBar = {
            TopAppBar(
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = colors.ground,
                    titleContentColor = colors.ink,
                    navigationIconContentColor = colors.ink,
                ),
                navigationIcon = {
                    val backLabel = stringResource(R.string.action_back)
                    IconButton(
                        onClick = { onAction(ReviewPageAction.Back) },
                        modifier = Modifier
                            .thumbTarget()
                            .testTag(AskReviewTags.REVIEW_BACK),
                    ) {
                        Icon(
                            imageVector = Icons.AutoMirrored.Outlined.ArrowBack,
                            contentDescription = backLabel,
                        )
                    }
                },
                title = {
                    Column {
                        Text(
                            text = stringResource(R.string.review_title),
                            style = AmuxTheme.typography.bodyEmphasis,
                            color = colors.ink,
                            maxLines = 1,
                            modifier = Modifier.semantics { heading() },
                        )
                        if (state.doc != null && files.isNotEmpty()) {
                            Row(
                                horizontalArrangement = Arrangement.spacedBy(Space.xs),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Text(
                                    text = ChatWords.files(files.size),
                                    style = AmuxTheme.typography.caption,
                                    color = colors.inkMuted,
                                )
                                DiffDeltaCounts(
                                    added = state.doc.added,
                                    removed = state.doc.removed,
                                )
                            }
                        }
                    }
                },
                actions = {
                    if (commentCount > 0) {
                        TextButton(
                            onClick = { onAction(ReviewPageAction.SendReview) },
                            modifier = Modifier
                                .thumbTarget()
                                .testTag(AskReviewTags.REVIEW_SEND),
                        ) {
                            Text(
                                text = stringResource(R.string.review_send_count, commentCount),
                                style = AmuxTheme.typography.bodyEmphasis,
                                color = colors.accent,
                            )
                        }
                    }
                },
            )
        },
    ) { innerPadding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding),
        ) {
            Column(modifier = Modifier.fillMaxSize()) {
                val activeError = state.error
                if (state.loading && state.doc == null) {
                    Box(
                        modifier = Modifier
                            .fillMaxSize()
                            .padding(Space.margin)
                            .testTag(AskReviewTags.REVIEW_LOADING),
                        contentAlignment = Alignment.TopCenter,
                    ) {
                        AmuxEmptyCard(
                            title = stringResource(R.string.review_loading_title),
                            detail = stringResource(R.string.review_loading_detail),
                        )
                    }
                } else if (activeError != null && state.doc == null) {
                    Column(
                        modifier = Modifier
                            .fillMaxSize()
                            .padding(Space.margin)
                            .testTag(AskReviewTags.REVIEW_ERROR),
                        verticalArrangement = Arrangement.spacedBy(Space.m),
                    ) {
                        AmuxEmptyCard(
                            title = stringResource(R.string.review_error_title),
                            detail = activeError,
                        )
                        AmuxActionButton(
                            label = stringResource(R.string.action_retry),
                            kind = ActionButtonKind.Primary,
                            fillWidth = true,
                            onClick = { onAction(ReviewPageAction.Retry) },
                            modifier = Modifier.testTag(AskReviewTags.REVIEW_RETRY),
                        )
                    }
                } else if (files.isEmpty()) {
                    Box(
                        modifier = Modifier
                            .fillMaxSize()
                            .padding(Space.margin)
                            .testTag(AskReviewTags.REVIEW_EMPTY),
                        contentAlignment = Alignment.TopCenter,
                    ) {
                        AmuxEmptyCard(
                            title = stringResource(R.string.review_empty_title),
                            detail = stringResource(R.string.review_empty_detail),
                        )
                    }
                } else {
                    if (activeError != null) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .background(colors.sunken)
                                .padding(horizontal = Space.margin, vertical = Space.xs)
                                .testTag(AskReviewTags.REVIEW_ERROR),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(Space.s),
                        ) {
                            Text(
                                text = activeError,
                                style = AmuxTheme.typography.detail,
                                color = colors.accent,
                                modifier = Modifier.weight(1f),
                            )
                            TextButton(
                                onClick = { onAction(ReviewPageAction.Retry) },
                                modifier = Modifier
                                    .thumbTarget()
                                    .testTag(AskReviewTags.REVIEW_RETRY),
                            ) {
                                Text(
                                    text = stringResource(R.string.action_retry),
                                    style = AmuxTheme.typography.bodyEmphasis,
                                    color = colors.ink,
                                )
                            }
                        }
                    }
                    // Horizontal scrollable file strip
                    LazyRow(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(vertical = Space.xs),
                        contentPadding = PaddingValues(horizontal = Space.margin),
                        horizontalArrangement = Arrangement.spacedBy(Space.s),
                    ) {
                        itemsIndexed(files, key = { _, file -> file.path }) { index, file ->
                            val isSelected = index == activeFileIndex
                            val fileCommentCount = commentsByFile[file.path] ?: 0
                            ReviewFileStripChip(
                                label = fileLabels[index],
                                added = file.added,
                                removed = file.removed,
                                commentCount = fileCommentCount,
                                isSelected = isSelected,
                                onClick = {
                                    activeFileIndex = index
                                    onAction(ReviewPageAction.SelectFile(index))
                                },
                                modifier = Modifier.testTag(AskReviewTags.reviewFileChip(index)),
                            )
                        }
                    }

                    AmuxHairline()

                    val currentFile = files[activeFileIndex]
                    val indexedComments = state.comments.mapIndexedNotNull { idx, comment ->
                        if (comment.path == currentFile.path) idx to comment else null
                    }
                    val fileLevelComments = indexedComments.filter { (_, c) -> c.line == 0 && c.oldLine == 0 }

                    LazyColumn(
                        modifier = Modifier
                            .fillMaxWidth()
                            .weight(1f),
                        contentPadding = PaddingValues(bottom = 96.dp),
                    ) {
                        item(key = "header:${currentFile.path}") {
                            ReviewFileHeader(
                                file = currentFile,
                                enabled = true,
                                onCommentOnFile = {
                                    commentTarget = CommentDraftTarget.New(
                                        path = currentFile.path,
                                        line = 0,
                                        oldLine = 0,
                                    )
                                },
                            )
                            AmuxHairline()
                        }

                        itemsIndexed(
                            items = fileLevelComments,
                            key = { _, (idx, _) -> "file-comment:$idx" },
                        ) { _, (commentIndex, comment) ->
                            ReviewCommentThreadRow(
                                text = comment.text,
                                onClick = {
                                    commentTarget = CommentDraftTarget.Editing(commentIndex, comment.text)
                                },
                                modifier = Modifier.testTag(AskReviewTags.reviewCommentThread(commentIndex)),
                            )
                        }

                        if (currentFile.binary) {
                            item(key = "binary:${currentFile.path}") {
                                Text(
                                    text = stringResource(R.string.review_binary_file),
                                    style = AmuxTheme.typography.detail,
                                    color = colors.inkFaint,
                                    modifier = Modifier.padding(horizontal = Space.margin, vertical = Space.m),
                                )
                            }
                        } else {
                            currentFile.hunks.forEachIndexed { hunkIndex, hunk ->
                                if (hunk.skips) {
                                    item(key = "hunk:$activeFileIndex:$hunkIndex") {
                                        ReviewHunkBreakRow(scope = hunk.scope)
                                    }
                                }
                                itemsIndexed(
                                    items = hunk.lines,
                                    key = { lineIndex, _ -> "line:$activeFileIndex:$hunkIndex:$lineIndex" },
                                ) { lineIndex, diffLine ->
                                    val lineComments = indexedComments.filter { (_, c) ->
                                        matchesDiffLine(c, diffLine)
                                    }
                                    ReviewDiffLineRow(
                                        line = diffLine,
                                        hasComments = lineComments.isNotEmpty(),
                                        onClick = {
                                            val targetLine = when (diffLine.kind) {
                                                LineKind.Removed -> 0
                                                LineKind.Added, LineKind.Context -> diffLine.newLine ?: 0
                                            }
                                            val targetOldLine = when (diffLine.kind) {
                                                LineKind.Removed -> diffLine.oldLine ?: 0
                                                LineKind.Added, LineKind.Context -> 0
                                            }
                                            commentTarget = CommentDraftTarget.New(
                                                path = currentFile.path,
                                                line = targetLine,
                                                oldLine = targetOldLine,
                                            )
                                        },
                                        modifier = Modifier.testTag(
                                            AskReviewTags.reviewLine(activeFileIndex, hunkIndex, lineIndex),
                                        ),
                                    )
                                    lineComments.forEach { (commentIndex, comment) ->
                                        ReviewCommentThreadRow(
                                            text = comment.text,
                                            onClick = {
                                                commentTarget = CommentDraftTarget.Editing(commentIndex, comment.text)
                                            },
                                            modifier = Modifier.testTag(AskReviewTags.reviewCommentThread(commentIndex)),
                                        )
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Inline / bottom comment composer sheet
            val activeTarget = commentTarget
            if (activeTarget != null) {
                Box(
                    modifier = Modifier.fillMaxSize(),
                    contentAlignment = Alignment.BottomCenter,
                ) {
                    val dismissLabel = stringResource(R.string.action_close)
                    AmuxScrim(
                        modifier = Modifier
                            .clickable(onClickLabel = dismissLabel) { commentTarget = null }
                            .semantics { contentDescription = dismissLabel },
                    )
                    // Keyed by its target, so each comment opens with its own words.
                    key(activeTarget) {
                        ReviewCommentComposer(
                            target = activeTarget,
                            onCancel = { commentTarget = null },
                            onSave = { text ->
                                when (activeTarget) {
                                    is CommentDraftTarget.New -> onAction(
                                        ReviewPageAction.AddComment(
                                            path = activeTarget.path,
                                            line = activeTarget.line,
                                            oldLine = activeTarget.oldLine,
                                            text = text,
                                        ),
                                    )
                                    is CommentDraftTarget.Editing -> onAction(
                                        ReviewPageAction.EditComment(
                                            index = activeTarget.index,
                                            text = text,
                                        ),
                                    )
                                }
                                commentTarget = null
                            },
                            onDelete = if (activeTarget is CommentDraftTarget.Editing) {
                                {
                                    onAction(ReviewPageAction.DeleteComment(activeTarget.index))
                                    commentTarget = null
                                }
                            } else {
                                null
                            },
                        )
                    }
                }
            }
        }
    }
}

private fun matchesDiffLine(comment: ReviewComment, line: DiffLine): Boolean = when (line.kind) {
    LineKind.Removed -> comment.line == 0 && comment.oldLine > 0 && comment.oldLine == line.oldLine
    LineKind.Added, LineKind.Context -> comment.line > 0 && comment.line == line.newLine
}

private fun shortFileLabels(paths: List<String>): List<String> {
    val names = paths.map { it.substringAfterLast('/') }
    val duplicates = names.groupingBy { it }.eachCount().filterValues { it > 1 }.keys
    return paths.mapIndexed { index, path ->
        val name = names[index]
        if (name in duplicates) {
            val parts = path.split('/')
            if (parts.size >= 2) "${parts[parts.lastIndex - 1]}/$name" else path
        } else {
            name
        }
    }
}

@Composable
private fun ReviewFileStripChip(
    label: String,
    added: Int,
    removed: Int,
    commentCount: Int,
    isSelected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    Surface(
        modifier = modifier
            .thumbTarget()
            .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
            .clickable(role = Role.Tab, onClick = onClick)
            .semantics { selected = isSelected },
        shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
        color = if (isSelected) colors.raised else colors.sunken,
        border = BorderStroke(
            width = if (isSelected) 1.5.dp else AmuxTheme.metrics.hairline,
            color = if (isSelected) colors.ink else colors.hairline,
        ),
    ) {
        Row(
            modifier = Modifier.padding(horizontal = Space.m, vertical = Space.s),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            Text(
                text = label,
                style = if (isSelected) AmuxTheme.typography.bodyEmphasis else AmuxTheme.typography.monoSmall,
                color = colors.ink,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            DiffDeltaCounts(added = added, removed = removed)
            if (commentCount > 0) {
                AmuxStatusChip(
                    text = commentCount.toString(),
                    accent = true,
                )
            }
        }
    }
}

@Composable
private fun ReviewFileHeader(
    file: ReviewFile,
    enabled: Boolean,
    onCommentOnFile: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val pathDisplay = if (!file.oldPath.isNullOrEmpty() && file.oldPath != file.path) {
        "${file.oldPath} \u2192 ${file.path}"
    } else {
        file.path
    }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(colors.raised)
            .padding(horizontal = Space.margin, vertical = Space.s),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(Space.xxs),
        ) {
            Text(
                text = pathDisplay,
                style = AmuxTheme.typography.mono,
                color = colors.ink,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            DiffDeltaCounts(added = file.added, removed = file.removed)
        }
        TextButton(
            enabled = enabled,
            onClick = onCommentOnFile,
            modifier = Modifier
                .thumbTarget()
                .testTag(AskReviewTags.REVIEW_FILE_COMMENT),
        ) {
            Icon(
                imageVector = Icons.Outlined.ModeComment,
                contentDescription = null,
                tint = colors.inkMuted,
                modifier = Modifier.size(IconSize.Inline),
            )
            Spacer(modifier = Modifier.width(Space.xs))
            Text(
                text = stringResource(R.string.review_file_comment_button),
                style = AmuxTheme.typography.caption,
                color = colors.ink,
            )
        }
    }
}

@Composable
private fun ReviewHunkBreakRow(scope: String) {
    val colors = AmuxTheme.colors
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(colors.sunken)
            .padding(horizontal = Space.s, vertical = Space.xs),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Text(
            text = stringResource(R.string.ask_diff_hunk_ellipsis),
            style = AmuxTheme.typography.monoSmall,
            color = colors.inkFaint,
            textAlign = TextAlign.End,
            modifier = Modifier.width(64.dp),
        )
        Text(
            text = scope,
            style = AmuxTheme.typography.monoSmall,
            color = colors.inkFaint,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
    }
}

@Composable
private fun ReviewDiffLineRow(
    line: DiffLine,
    hasComments: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val addedTint = colors.added.copy(alpha = 0.14f)
    val removedTint = colors.removed.copy(alpha = 0.14f)
    val background = when (line.kind) {
        LineKind.Added -> addedTint
        LineKind.Removed -> removedTint
        LineKind.Context -> if (hasComments) colors.sunken else Color.Transparent
    }
    val prefix = when (line.kind) {
        LineKind.Added -> "+"
        LineKind.Removed -> "\u2212"
        LineKind.Context -> " "
    }
    val prefixColor = when (line.kind) {
        LineKind.Added -> colors.added
        LineKind.Removed -> colors.removed
        LineKind.Context -> colors.inkFaint
    }
    val spoken = ChatWords.spoken(line)

    Row(
        modifier = modifier
            .fillMaxWidth()
            .thumbTarget()
            .background(background)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = Space.s, vertical = Space.xs)
            .semantics { contentDescription = spoken },
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = line.oldLine?.toString().orEmpty(),
            style = AmuxTheme.typography.monoSmall,
            color = colors.inkFaint,
            textAlign = TextAlign.End,
            modifier = Modifier
                .width(30.dp)
                .padding(end = Space.xs),
        )
        Text(
            text = line.newLine?.toString().orEmpty(),
            style = AmuxTheme.typography.monoSmall,
            color = colors.inkFaint,
            textAlign = TextAlign.End,
            modifier = Modifier
                .width(30.dp)
                .padding(end = Space.xs),
        )
        Text(
            text = prefix,
            style = AmuxTheme.typography.monoSmall,
            color = prefixColor,
            modifier = Modifier.width(14.dp),
        )
        Box(
            modifier = Modifier
                .weight(1f)
                .horizontalScroll(rememberScrollState()),
        ) {
            Text(
                text = line.text,
                style = AmuxTheme.typography.monoSmall,
                color = colors.ink,
                softWrap = false,
            )
        }
    }
}

@Composable
private fun ReviewCommentThreadRow(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(colors.raised)
            .clickable(role = Role.Button, onClick = onClick)
            .thumbTarget()
            .padding(horizontal = Space.margin, vertical = Space.s),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Box(
            modifier = Modifier
                .width(3.dp)
                .height(24.dp)
                .clip(RoundedCornerShape(2.dp))
                .background(colors.accent),
        )
        Text(
            text = text,
            style = AmuxTheme.typography.body,
            color = colors.ink,
            modifier = Modifier.weight(1f),
        )
    }
}

/**
 * The comment being written. It owns its words, so typing redraws this card
 * and not the diff under it; the page hears the words once they are saved.
 */
@Composable
private fun ReviewCommentComposer(
    target: CommentDraftTarget,
    onCancel: () -> Unit,
    onSave: (String) -> Unit,
    onDelete: (() -> Unit)?,
) {
    val colors = AmuxTheme.colors
    var text by rememberSaveable {
        mutableStateOf((target as? CommentDraftTarget.Editing)?.text.orEmpty())
    }
    val trimmed = text.trim()
    val title = when (target) {
        is CommentDraftTarget.Editing -> stringResource(R.string.review_edit_comment_title)
        is CommentDraftTarget.New -> when {
            target.line > 0 -> stringResource(R.string.review_comment_on_line, target.line)
            target.oldLine > 0 -> stringResource(R.string.review_comment_on_old_line, target.oldLine)
            else -> stringResource(R.string.review_comment_on_file)
        }
    }
    val actionLabel = stringResource(
        if (target is CommentDraftTarget.Editing) R.string.review_save_comment else R.string.review_add_to_review,
    )
    val actionTag = if (target is CommentDraftTarget.Editing) {
        AskReviewTags.REVIEW_COMMENT_SAVE
    } else {
        AskReviewTags.REVIEW_COMMENT_ADD
    }
    val placeholder = stringResource(R.string.review_comment_placeholder)

    AmuxChrome(
        modifier = Modifier
            .fillMaxWidth()
            .windowInsetsPadding(WindowInsets.navigationBars.union(WindowInsets.ime))
            .padding(Space.m)
            .testTag(AskReviewTags.REVIEW_COMMENT_SHEET),
        shape = RoundedCornerShape(AmuxTheme.metrics.cardRadius),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(Space.l),
            verticalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            Text(
                text = title,
                style = AmuxTheme.typography.bodyEmphasis,
                color = colors.ink,
            )
            OutlinedTextField(
                value = text,
                onValueChange = { text = it },
                placeholder = {
                    Text(
                        text = placeholder,
                        style = AmuxTheme.typography.body,
                        color = colors.inkFaint,
                    )
                },
                textStyle = AmuxTheme.typography.body.copy(color = colors.ink),
                minLines = 2,
                maxLines = 5,
                shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                colors = amuxTextFieldColors(),
                modifier = Modifier
                    .fillMaxWidth()
                    .semantics { contentDescription = placeholder }
                    .testTag(AskReviewTags.REVIEW_COMMENT_INPUT),
            )
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                if (onDelete != null) {
                    TextButton(
                        onClick = onDelete,
                        modifier = Modifier
                            .thumbTarget()
                            .testTag(AskReviewTags.REVIEW_COMMENT_DELETE),
                    ) {
                        Text(
                            text = stringResource(R.string.review_delete_comment),
                            style = AmuxTheme.typography.bodyEmphasis,
                            color = colors.removed,
                        )
                    }
                }
                Spacer(modifier = Modifier.weight(1f))
                TextButton(
                    onClick = onCancel,
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(AskReviewTags.REVIEW_COMMENT_CANCEL),
                ) {
                    Text(
                        text = stringResource(R.string.action_cancel),
                        style = AmuxTheme.typography.body,
                        color = colors.inkMuted,
                    )
                }
                AmuxActionButton(
                    label = actionLabel,
                    kind = ActionButtonKind.Primary,
                    enabled = trimmed.isNotEmpty(),
                    onClick = { if (trimmed.isNotEmpty()) onSave(trimmed) },
                    modifier = Modifier.testTag(actionTag),
                )
            }
        }
    }
}
