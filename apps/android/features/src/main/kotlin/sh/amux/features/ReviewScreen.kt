package sh.amux.features

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.scrollBy
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
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.List
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.RestartAlt
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.PointerInputChange
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalViewConfiguration
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Job
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import sh.amux.design.AmuxChrome
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxShapes
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.ThumbTarget
import sh.amux.values.ReviewAnchor
import sh.amux.values.ReviewBanner
import sh.amux.values.ReviewCommentRow
import sh.amux.values.ReviewContextAsk
import sh.amux.values.ReviewEmpty
import sh.amux.values.ReviewFileHead
import sh.amux.values.ReviewHeader
import sh.amux.values.ReviewPage
import sh.amux.values.ReviewRow
import sh.amux.values.ReviewRowOf
import sh.amux.values.ReviewView
import sh.amux.words.ChatWords
import sh.amux.words.RowState
import kotlin.math.abs
import kotlin.math.max
import kotlin.math.min

/** What the review page draws: the page as the runtime handed it last, and the person's selection on it. */
@Immutable
data class ReviewScreenState(
    val page: ReviewPage,
    /** The rows selected, first to last, by index into the page's rows. */
    val selected: IntRange? = null,
    /** Where a comment on the selection goes; null for a selection with no line or across files. */
    val anchor: ReviewAnchor? = null,
    /** Why the last act came to nothing, when the page does not say it itself. */
    val notice: String? = null,
    val closed: Boolean = false,
) {
    val canComment: Boolean
        get() = anchor != null && !closed

    fun isSelected(row: Int): Boolean = selected?.contains(row) == true
}

/** What the person does on the review page. */
sealed interface ReviewAction {
    data object Back : ReviewAction

    /** Back to the chat, whose next message sends the drafts. */
    data object Done : ReviewAction

    /** A new snapshot even with drafts waiting, which a plain open would leave on theirs. */
    data object Refresh : ReviewAction

    /** Opens the review again: tries a snapshot that failed once more. */
    data object Reopen : ReviewAction
    data object DiscardDrafts : ReviewAction
    data object StartOver : ReviewAction
    data class Show(val view: ReviewView) : ReviewAction
    data class ToggleFile(val path: String) : ReviewAction
    data class ToggleComment(val id: List<Int>) : ReviewAction
    data class MoreContext(val ask: ReviewContextAsk) : ReviewAction
    data class Select(val row: Int) : ReviewAction
    data class Extend(val row: Int) : ReviewAction
    data object ClearSelection : ReviewAction

    /** A draft on the selection; the page sends it only while [ReviewScreenState.canComment]. */
    data class Comment(val text: String) : ReviewAction
    data class Reword(val id: List<Int>, val text: String) : ReviewAction
    data class Delete(val id: List<Int>) : ReviewAction
}

/** What the person is asked to confirm before it happens. */
private enum class Asking { DiscardDrafts, StartOver }

/**
 * The agent's review, read and commented on with a thumb.
 *
 * Every row is the shared page's, drawn where the page puts it: a file's
 * head, which stays pinned while its file is read and folds it; where a hunk
 * sits in the code; its lines, with the words a line changed marked; gaps
 * that load more of the file around a change; and the person's comments under
 * the lines they are on, or under the file's head. Nothing is worked out
 * again here.
 *
 * A tap on a line selects it, a tap on another line of the same file
 * stretches the selection to it, and holding a line then dragging draws one;
 * a file's head is held for a comment on the whole file. The page's foot then
 * offers the comment, which is written in a sheet while the lines stay in
 * sight above it. Tapping a comment edits or deletes it. Drafts wait on the
 * agent's computer and show as a chip in the chat's composer, which sends
 * them with the next message; Done goes back there.
 */
@Composable
fun ReviewScreen(
    state: ReviewScreenState,
    onAction: (ReviewAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val page = state.page
    val header = page.header
    val currentState by rememberUpdatedState(state)
    val currentAction by rememberUpdatedState(onAction)

    var writing by rememberSaveable { mutableStateOf(false) }
    var editing by remember { mutableStateOf<ReviewCommentRow?>(null) }
    var asking by remember { mutableStateOf<Asking?>(null) }
    val listState = rememberLazyListState()
    val scope = rememberCoroutineScope()

    val entries = remember(page.rows) { ReviewEntries(page.rows) }
    val heads = remember(page.rows) {
        page.rows.mapNotNull { (it.of as? ReviewRowOf.File)?.value }
    }
    // The file being read is the one whose rows the list's top shows.
    val reading by remember(entries) {
        derivedStateOf { entries.fileAt(listState.firstVisibleItemIndex) }
    }

    BackHandler(enabled = state.selected != null && !writing && editing == null) {
        onAction(ReviewAction.ClearSelection)
    }

    fun goTo(key: String) {
        val index = entries.indexOf(key) ?: return
        scope.launch { listState.scrollToItem(index) }
    }

    AmuxGround(
        modifier = modifier
            .fillMaxSize()
            .testTag(ReviewTags.SCREEN),
    ) {
        Scaffold(
            containerColor = Color.Transparent,
            contentColor = colors.ink,
            contentWindowInsets = WindowInsets(0, 0, 0, 0),
            topBar = {
                val scrolled by remember { derivedStateOf { listState.canScrollBackward } }
                ReviewTopBar(
                    header = header,
                    heads = heads,
                    reading = reading,
                    scrolled = scrolled,
                    onBack = { onAction(ReviewAction.Back) },
                    onGoTo = { path -> goTo(fileKey(page.rows, path)) },
                    onRefresh = { onAction(ReviewAction.Refresh) },
                    onDiscard = { asking = Asking.DiscardDrafts },
                    onStartOver = { asking = Asking.StartOver },
                )
            },
        ) { innerPadding ->
            // The foot stands under the list rather than over it, so nothing
            // it offers sits on a line a finger means to reach.
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(innerPadding),
            ) {
                Box(modifier = Modifier.weight(1f)) {
                    if (header == null && page.banners.isEmpty()) {
                        ReviewWaiting(text = ChatWords.REVIEW_OPENING)
                    } else {
                        // Pulling the page down takes a new snapshot, as Refresh
                        // does. The indicator goes as the finger lifts rather than
                        // spinning, as nothing in the app spins: the page's own
                        // rows move when the snapshot lands.
                        PullToRefreshBox(
                            isRefreshing = false,
                            onRefresh = { onAction(ReviewAction.Refresh) },
                            modifier = Modifier.fillMaxSize(),
                        ) {
                            ReviewList(
                                state = state,
                                entries = entries,
                                listState = listState,
                                contentPadding = PaddingValues(bottom = Space.l),
                                onAction = onAction,
                                onCommentOnFile = { index ->
                                    onAction(ReviewAction.Select(index))
                                    writing = true
                                },
                                onEdit = { comment -> editing = comment },
                            )
                        }
                    }
                }
                ReviewFoot(
                    state = state,
                    selectionWords = state.selected?.let { commentWords(state, it) },
                    onAction = onAction,
                    onWrite = { writing = true },
                )
            }
        }
    }

    // The lines being written about stay in sight above the sheet.
    val writingAbout = editing?.let { "c/" + hex(it.id) }
        ?: state.selected?.let { page.rows.getOrNull(it.last)?.key }.takeIf { writing }
    LaunchedEffect(writing, editing?.id) {
        val key = writingAbout ?: return@LaunchedEffect
        val index = entries.indexOf(key) ?: return@LaunchedEffect
        listState.scrollToItem(index)
        listState.scrollBy(-listState.layoutInfo.viewportSize.height * 0.3f)
    }

    val edited = editing
    if (edited != null) {
        ReviewCommentSheet(
            title = ChatWords.reviewCommentState(edited.state),
            initial = edited.text,
            key = edited.id,
            editing = true,
            onDismiss = { editing = null },
            onAct = { text ->
                currentAction(ReviewAction.Reword(edited.id, text))
                editing = null
            },
            onDelete = {
                currentAction(ReviewAction.Delete(edited.id))
                editing = null
            },
        )
    } else if (writing) {
        ReviewCommentSheet(
            title = state.selected?.let { commentWords(state, it) }.orEmpty(),
            initial = "",
            key = state.selected,
            editing = false,
            onDismiss = { writing = false },
            onAct = { text ->
                // Written only while the selection still holds lines: the
                // page may have moved under the sheet.
                if (text.isNotBlank() && currentState.canComment) {
                    currentAction(ReviewAction.Comment(text))
                    writing = false
                }
            },
        )
    }

    when (asking) {
        Asking.DiscardDrafts -> ReviewConfirm(
            question = ChatWords.discardDrafts(header?.drafts ?: 0),
            detail = ChatWords.DISCARD_DRAFTS_DETAIL,
            confirm = stringResource(R.string.review_discard_drafts),
            confirmTag = ReviewTags.DISCARD_CONFIRM,
            onConfirm = {
                asking = null
                onAction(ReviewAction.DiscardDrafts)
            },
            onDismiss = { asking = null },
        )
        Asking.StartOver -> ReviewConfirm(
            question = ChatWords.START_OVER_QUESTION,
            detail = ChatWords.START_OVER_DETAIL,
            confirm = stringResource(R.string.review_start_over),
            confirmTag = ReviewTags.START_OVER_CONFIRM,
            onConfirm = {
                asking = null
                onAction(ReviewAction.StartOver)
            },
            onDismiss = { asking = null },
        )
        null -> Unit
    }
}

// --- The bar ---------------------------------------------------------------

/**
 * "Review" over what it is against and its lines ("Since main · +12 −4"),
 * then every file to jump to and the menu that refreshes and deletes.
 */
@Composable
private fun ReviewTopBar(
    header: ReviewHeader?,
    heads: List<ReviewFileHead>,
    reading: String?,
    scrolled: Boolean,
    onBack: () -> Unit,
    onGoTo: (String) -> Unit,
    onRefresh: () -> Unit,
    onDiscard: () -> Unit,
    onStartOver: () -> Unit,
) {
    val colors = AmuxTheme.colors
    var filesOpen by remember { mutableStateOf(false) }
    var moreOpen by remember { mutableStateOf(false) }
    AmuxTopBar(
        title = stringResource(R.string.review_title),
        scrolled = scrolled,
        onNavigate = onBack,
        navigationModifier = Modifier.testTag(ReviewTags.BACK),
        subtitleContent = header?.let {
            {
                val words = "${ChatWords.reviewAgainst(it)}, ${ChatWords.reviewCounts(it)}"
                CappedFontScale {
                    Row(
                        modifier = Modifier
                            .semantics(mergeDescendants = true) { contentDescription = words }
                            .testTag(ReviewTags.SUMMARY),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(Space.xs),
                    ) {
                        Text(
                            text = subtitle(it),
                            style = AmuxTheme.typography.caption,
                            color = colors.inkMuted,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.weight(1f, fill = false),
                        )
                        DiffDeltaCounts(
                            added = it.added,
                            removed = it.removed,
                            style = AmuxTheme.typography.caption,
                        )
                    }
                }
            }
        },
    ) {
        if (header != null) {
            Box {
                IconButton(
                    onClick = { filesOpen = true },
                    modifier = Modifier.testTag(ReviewTags.FILES),
                ) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Outlined.List,
                        contentDescription = stringResource(R.string.review_files),
                        tint = colors.ink,
                    )
                }
                DropdownMenu(
                    expanded = filesOpen,
                    onDismissRequest = { filesOpen = false },
                    containerColor = colors.raised,
                ) {
                    heads.forEach { head ->
                        DropdownMenuItem(
                            text = {
                                Column {
                                    Text(
                                        text = head.path,
                                        style = AmuxTheme.typography.body,
                                        color = colors.ink,
                                        maxLines = 1,
                                        overflow = TextOverflow.StartEllipsis,
                                    )
                                    val summary = fileSummary(head)
                                    if (summary.isNotEmpty()) {
                                        Text(
                                            text = summary,
                                            style = AmuxTheme.typography.caption,
                                            color = colors.inkMuted,
                                        )
                                    }
                                }
                            },
                            trailingIcon = if (head.path == reading) {
                                { AmuxCheckMark() }
                            } else {
                                null
                            },
                            onClick = {
                                filesOpen = false
                                onGoTo(head.path)
                            },
                            modifier = Modifier.testTag(ReviewTags.filesItem(head.path)),
                        )
                    }
                }
            }
            Box {
                IconButton(
                    onClick = { moreOpen = true },
                    modifier = Modifier.testTag(ReviewTags.MORE),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.MoreVert,
                        contentDescription = stringResource(R.string.action_more),
                        tint = colors.ink,
                    )
                }
                DropdownMenu(
                    expanded = moreOpen,
                    onDismissRequest = { moreOpen = false },
                    containerColor = colors.raised,
                ) {
                    ReviewMenuItem(
                        label = stringResource(R.string.review_refresh),
                        icon = Icons.Outlined.Refresh,
                        tag = ReviewTags.REFRESH,
                    ) {
                        moreOpen = false
                        onRefresh()
                    }
                    if (header.discardDrafts) {
                        ReviewMenuItem(
                            label = stringResource(R.string.review_discard_drafts),
                            icon = Icons.Outlined.DeleteOutline,
                            tag = ReviewTags.DISCARD,
                            destructive = true,
                        ) {
                            moreOpen = false
                            onDiscard()
                        }
                    }
                    if (header.startOver) {
                        ReviewMenuItem(
                            label = stringResource(R.string.review_start_over),
                            icon = Icons.Outlined.RestartAlt,
                            tag = ReviewTags.START_OVER,
                            destructive = true,
                        ) {
                            moreOpen = false
                            onStartOver()
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun ReviewMenuItem(
    label: String,
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    tag: String,
    destructive: Boolean = false,
    onClick: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val tint = if (destructive) colors.removed else colors.ink
    DropdownMenuItem(
        text = { Text(text = label, color = tint) },
        leadingIcon = { Icon(imageVector = icon, contentDescription = null, tint = if (destructive) tint else colors.inkMuted) },
        onClick = onClick,
        modifier = Modifier.testTag(tag),
    )
}

/**
 * What the bar says under "Review": what the change is counted against, or
 * with the two views to switch between, which the switch already names, how
 * many files the view shows.
 */
private fun subtitle(header: ReviewHeader): String =
    if (header.views.size > 1) ChatWords.files(header.files) else ChatWords.reviewAgainst(header)

/** "+3 −4 · 1 comment", leaving out what is nothing. */
internal fun fileSummary(head: ReviewFileHead): String {
    val parts = mutableListOf<String>()
    val delta = listOfNotNull(
        head.added.takeIf { it > 0 }?.let { "+$it" },
        head.removed.takeIf { it > 0 }?.let { "−$it" },
    ).joinToString(" ")
    if (delta.isNotEmpty()) parts += delta
    if (head.comments > 0) parts += ChatWords.comments(head.comments)
    return parts.joinToString(" · ")
}

private fun fileKey(rows: List<ReviewRow>, path: String): String =
    rows.firstOrNull { (it.of as? ReviewRowOf.File)?.value?.path == path }?.key ?: "f/$path"

// --- The page --------------------------------------------------------------

/**
 * The page's rows in the order the list draws them: what stands above the
 * files, then each file's head followed by its rows. Held so that a row's
 * key finds its place in the list, and a place in the list its file.
 */
internal class ReviewEntries(rows: List<ReviewRow>) {
    sealed interface Entry {
        data object Above : Entry
        data class Head(val row: Int, val path: String) : Entry
        data class Body(val row: Int) : Entry
    }

    val all: List<Entry>
    /** Each entry's list key; a key repeated on the page takes its index as well, as the list needs every key once. */
    val keys: List<String>
    private val placeOf: Map<String, Int>
    /** The file each entry is in, by list index; null above the first file. */
    private val files: List<String?>

    init {
        val entries = mutableListOf<Entry>(Entry.Above)
        val keyed = mutableListOf(TOP)
        val inFile = mutableListOf<String?>(null)
        val seen = mutableSetOf(TOP)
        var file: String? = null
        rows.forEachIndexed { index, row ->
            val of = row.of
            if (of is ReviewRowOf.File) {
                file = of.value.path
                entries += Entry.Head(index, of.value.path)
            } else {
                entries += Entry.Body(index)
            }
            keyed += if (seen.add(row.key)) row.key else "${row.key}#$index"
            inFile += file
        }
        all = entries
        keys = keyed
        files = inFile
        placeOf = keyed.withIndex().associate { (index, key) -> key to index }
    }

    fun indexOf(key: String): Int? = placeOf[key]

    fun fileAt(index: Int): String? = files.getOrNull(index)

    companion object {
        const val TOP: String = "review.top"
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ReviewList(
    state: ReviewScreenState,
    entries: ReviewEntries,
    listState: LazyListState,
    contentPadding: PaddingValues,
    onAction: (ReviewAction) -> Unit,
    onCommentOnFile: (Int) -> Unit,
    onEdit: (ReviewCommentRow) -> Unit,
) {
    val page = state.page
    val rows = page.rows
    val drawing = rememberSelectionDrawing(
        rows = rows,
        entries = entries,
        listState = listState,
        onSelect = { onAction(ReviewAction.Select(it)) },
        onExtend = { onAction(ReviewAction.Extend(it)) },
    )
    LazyColumn(
        state = listState,
        contentPadding = contentPadding,
        modifier = Modifier
            .fillMaxSize()
            .then(drawing),
    ) {
        entries.all.forEachIndexed { index, entry ->
            val key = entries.keys[index]
            when (entry) {
                ReviewEntries.Entry.Above -> item(key = key, contentType = "above") {
                    ReviewAbove(page = page, onAction = onAction)
                }
                is ReviewEntries.Entry.Head -> stickyHeader(key = key, contentType = "file") {
                    val head = (rows[entry.row].of as ReviewRowOf.File).value
                    val selected = state.isSelected(entry.row)
                    ReviewFileHeadRow(
                        head = head,
                        selected = selected,
                        marksSince = page.header?.view == ReviewView.All,
                        onToggle = { onAction(ReviewAction.ToggleFile(head.path)) },
                        onCommentOnFile = { onCommentOnFile(entry.row) },
                        modifier = Modifier.testTag(ReviewTags.FILE),
                    )
                }
                is ReviewEntries.Entry.Body -> {
                    val row = rows[entry.row]
                    item(key = key, contentType = row.of::class) {
                        ReviewBodyRow(
                            row = row,
                            index = entry.row,
                            state = state,
                            onAction = onAction,
                            onEdit = onEdit,
                        )
                    }
                }
            }
        }
    }
}

/**
 * What stands above the files: the switch between the views, what the page
 * says about itself, and why there is nothing to read.
 */
@Composable
private fun ReviewAbove(page: ReviewPage, onAction: (ReviewAction) -> Unit) {
    val header = page.header
    val showsViews = header != null && header.views.size > 1
    if (!showsViews && page.banners.isEmpty() && page.empty == null) return
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = AmuxTheme.metrics.gutter, vertical = Space.s),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        if (header != null && showsViews) {
            AmuxSegmentedChoice(
                options = header.views.map { it.view },
                selected = header.view,
                label = { ChatWords.reviewView(it) },
                onSelect = { onAction(ReviewAction.Show(it)) },
                tag = { "${ReviewTags.VIEWS}.${it.name}" },
                modifier = Modifier.testTag(ReviewTags.VIEWS),
            )
        }
        page.banners.forEach { banner ->
            val act: (() -> Unit)? = when (banner) {
                ReviewBanner.ChangedSinceYouStarted, ReviewBanner.ChangedSince -> {
                    { onAction(ReviewAction.Refresh) }
                }
                is ReviewBanner.SnapshotFailed -> {
                    { onAction(ReviewAction.Reopen) }
                }
                else -> null
            }
            ReviewBannerCard(banner = banner, onAct = act)
        }
        page.empty?.let { ReviewEmptyNote(it) }
    }
}

/** Nothing to review at all, or nothing since the last round, with all changes on the switch above. */
@Composable
private fun ReviewEmptyNote(empty: ReviewEmpty) {
    Text(
        text = ChatWords.reviewEmpty(empty),
        style = AmuxTheme.typography.body,
        color = AmuxTheme.colors.inkMuted,
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = Space.m)
            .semantics { stateDescription = empty.name }
            .testTag(ReviewTags.EMPTY),
    )
}

@Composable
private fun ReviewBodyRow(
    row: ReviewRow,
    index: Int,
    state: ReviewScreenState,
    onAction: (ReviewAction) -> Unit,
    onEdit: (ReviewCommentRow) -> Unit,
) {
    when (val of = row.of) {
        is ReviewRowOf.File -> Unit
        is ReviewRowOf.Comment -> {
            val comment = of.value
            val folded = stringResource(R.string.review_folded)
            val hint = stringResource(if (comment.folded) R.string.review_open_comment else R.string.review_edit_comment)
            ReviewCommentCard(
                comment = comment,
                modifier = Modifier
                    .clickable(role = Role.Button, onClickLabel = hint) {
                        if (comment.folded) onAction(ReviewAction.ToggleComment(comment.id)) else onEdit(comment)
                    }
                    .semantics(mergeDescendants = true) { stateDescription = commentValue(comment, folded) }
                    .testTag(ReviewTags.COMMENT),
            )
        }
        is ReviewRowOf.Hunk -> ReviewHunkBand(hunk = of.value)
        is ReviewRowOf.Line -> {
            val selected = state.isSelected(index)
            val select = stringResource(R.string.review_select_line)
            ReviewCodeLineRow(
                line = of.value,
                selected = selected,
                modifier = Modifier
                    .clickable(
                        interactionSource = null,
                        indication = null,
                        onClickLabel = select,
                    ) { tap(state, index, onAction) }
                    .semantics {
                        customActions = listOf(
                            CustomAccessibilityAction(select) {
                                tap(state, index, onAction)
                                true
                            },
                        )
                        stateDescription = listOfNotNull(
                            "commented".takeIf { of.value.commented },
                            "selected".takeIf { selected },
                        ).joinToString(" ")
                    }
                    .testTag(ReviewTags.line(row.key)),
            )
        }
        is ReviewRowOf.Gap -> ReviewGapRow(gap = of.value, onLoad = { onAction(ReviewAction.MoreContext(it)) })
        is ReviewRowOf.Loading -> ReviewNote(
            text = stringResource(R.string.review_loading),
            loading = true,
            modifier = Modifier.testTag(ReviewTags.LOADING),
        )
        is ReviewRowOf.NoLines -> ReviewNote(
            text = ChatWords.reviewNoLines(of.why),
            modifier = Modifier.testTag(ReviewTags.NO_LINES),
        )
    }
}

/** A tap on a line: it stretches a selection begun in its file to it, and otherwise selects it alone. */
private fun tap(state: ReviewScreenState, index: Int, onAction: (ReviewAction) -> Unit) {
    val selected = state.selected
    if (selected != null && state.anchor != null && sameFile(state.page.rows, selected.first, index)) {
        if (selected == index..index) {
            onAction(ReviewAction.ClearSelection)
        } else {
            onAction(ReviewAction.Extend(index))
        }
    } else {
        onAction(ReviewAction.Select(index))
    }
}

/** The two rows are under one file's head. */
private fun sameFile(rows: List<ReviewRow>, one: Int, other: Int): Boolean {
    fun head(index: Int): Int = (min(index, rows.lastIndex) downTo 0).firstOrNull { rows[it].of is ReviewRowOf.File } ?: -1
    return head(one) == head(other)
}

/** "Comment on 3 lines", "Comment on the File", or why nothing can be written on what is selected. */
@Composable
private fun commentWords(state: ReviewScreenState, selected: IntRange): String {
    val anchor = state.anchor ?: return ChatWords.REVIEW_ONE_FILE
    if (anchor.start == 0) return stringResource(R.string.review_comment_on_file)
    val lines = selected.count { state.page.rows.getOrNull(it)?.of is ReviewRowOf.Line }
    return ChatWords.commentOn(max(lines, 1))
}

/** A comment's id as the page's row key spells it. */
internal fun hex(bytes: List<Int>): String = bytes.joinToString("") { "%02x".format(it and 0xFF) }

// --- Drawing a selection ---------------------------------------------------

/** How deep the band at each edge is where the list runs under a held finger. */
private val EdgeBand = 56.dp

/** How fast the list runs with the finger at or past the edge, in dp a second: a screenful in about half a second. */
private const val FASTEST = 1400f

/** How fast it runs as the finger enters the band. */
private const val SLOWEST = 120f

/**
 * A hold on the review's lines that then follows the finger, the way text is
 * selected: the list scrolls under a finger that moves first, and once the
 * hold is up the finger draws instead. The selection follows the finger
 * through the lines of the file it began in, and the list runs under a finger
 * held at its top or bottom edge.
 *
 * The hold is watched before the list sees the touch, so that once it is up
 * the movement is taken from the list rather than scrolling it; until then
 * nothing is taken, and a swipe that moves first scrolls. A hold that finds
 * no line under it takes nothing either, so a file head's own hold still
 * opens its menu.
 */
@Composable
private fun rememberSelectionDrawing(
    rows: List<ReviewRow>,
    entries: ReviewEntries,
    listState: LazyListState,
    onSelect: (Int) -> Unit,
    onExtend: (Int) -> Unit,
): Modifier {
    val haptics = LocalHapticFeedback.current
    val density = LocalDensity.current
    val holdMillis = LocalViewConfiguration.current.longPressTimeoutMillis
    val slop = LocalViewConfiguration.current.touchSlop
    val scope = rememberCoroutineScope()
    val currentRows by rememberUpdatedState(rows)
    val currentEntries by rememberUpdatedState(entries)
    val select by rememberUpdatedState(onSelect)
    val extend by rememberUpdatedState(onExtend)

    return Modifier.pointerInput(listState) {
        val edge = with(density) { EdgeBand.toPx() }
        awaitEachGesture {
            val down = awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
            // Held still for the hold's length, or this is a swipe or a tap.
            val held = withTimeoutOrNull(holdMillis) {
                while (true) {
                    val event = awaitPointerEvent(PointerEventPass.Initial)
                    val change = event.changes.firstOrNull { it.id == down.id } ?: return@withTimeoutOrNull false
                    if (!change.pressed) return@withTimeoutOrNull false
                    if (abs(change.position.y - down.position.y) > slop || abs(change.position.x - down.position.x) > slop) {
                        return@withTimeoutOrNull false
                    }
                }
                @Suppress("UNREACHABLE_CODE")
                false
            } == null
            if (!held) return@awaitEachGesture

            val start = lineAt(down.position.y, listState, currentEntries, currentRows, within = null)
                ?: return@awaitEachGesture
            val file = fileOf(start, currentRows)
            var reached = start
            select(start)
            haptics.performHapticFeedback(HapticFeedbackType.LongPress)

            var finger = down.position.y
            var running: Job? = null
            fun reach(y: Float) {
                val line = lineAt(y, listState, currentEntries, currentRows, within = file) ?: return
                if (line != reached) {
                    reached = line
                    extend(line)
                }
            }
            fun speed(): Float {
                val bottom = listState.layoutInfo.viewportSize.height.toFloat()
                return edgeSpeed(finger, top = 0f, bottom = bottom, edge = edge, density = density.density)
            }
            fun run() {
                if (running?.isActive == true) return
                running = scope.launch {
                    var last = withFrameNanosCompat()
                    while (isActive) {
                        val now = withFrameNanosCompat()
                        val seconds = (now - last) / 1_000_000_000f
                        last = now
                        val pace = speed()
                        if (pace == 0f) break
                        val moved = listState.scrollBy(pace * seconds)
                        if (moved == 0f) break
                        reach(finger)
                    }
                }
            }
            while (true) {
                val event = awaitPointerEvent(PointerEventPass.Initial)
                val change: PointerInputChange = event.changes.firstOrNull { it.id == down.id } ?: break
                change.consume()
                if (!change.pressed) break
                finger = change.position.y
                reach(finger)
                if (speed() != 0f) run()
            }
            running?.cancel()
        }
    }
}

private suspend fun withFrameNanosCompat(): Long = androidx.compose.runtime.withFrameNanos { it }

/**
 * Pixels a second the list runs with the finger at [y] between a visible
 * [top] and [bottom]: up as a negative, nothing away from the edges, and
 * faster the deeper into an edge's band.
 */
internal fun edgeSpeed(y: Float, top: Float, bottom: Float, edge: Float, density: Float): Float {
    if (bottom - top <= edge * 2) return 0f
    fun pace(depth: Float): Float {
        val reach = min(1f, depth / edge)
        return (SLOWEST + (FASTEST - SLOWEST) * reach * reach) * density
    }
    return when {
        y < top + edge -> -pace(top + edge - y)
        y > bottom - edge -> pace(y - (bottom - edge))
        else -> 0f
    }
}

/** The rows from a row's file head to the next head. */
internal fun fileOf(row: Int, rows: List<ReviewRow>): IntRange {
    val start = (row downTo 0).firstOrNull { rows[it].of is ReviewRowOf.File } ?: 0
    val end = ((row + 1)..rows.lastIndex).firstOrNull { rows[it].of is ReviewRowOf.File } ?: rows.size
    return start until end
}

/**
 * The line under a height in the list. Within a file, a finger past the
 * file's first or last drawn line holds at it, and one over a comment or a
 * hunk's band holds where it was.
 */
private fun lineAt(
    y: Float,
    listState: LazyListState,
    entries: ReviewEntries,
    rows: List<ReviewRow>,
    within: IntRange?,
): Int? {
    val lines = listState.layoutInfo.visibleItemsInfo.mapNotNull { item ->
        val entry = entries.all.getOrNull(item.index) as? ReviewEntries.Entry.Body ?: return@mapNotNull null
        if (rows.getOrNull(entry.row)?.of !is ReviewRowOf.Line) return@mapNotNull null
        Triple(entry.row, item.offset.toFloat(), (item.offset + item.size).toFloat())
    }
    val under = lines.firstOrNull { (_, top, bottom) -> y >= top && y < bottom }?.first
    if (under != null && (within == null || under in within)) return under
    if (within == null) return null
    val inFile = lines.filter { it.first in within }
    val first = inFile.minByOrNull { it.first }
    if (first != null && y < first.second) return first.first
    val last = inFile.maxByOrNull { it.first }
    if (last != null && y >= last.third) return last.first
    return null
}

// --- The foot --------------------------------------------------------------

@Composable
private fun ReviewFoot(
    state: ReviewScreenState,
    selectionWords: String?,
    onAction: (ReviewAction) -> Unit,
    onWrite: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val drafts = state.page.header?.drafts ?: 0
    Column(
        modifier = modifier
            .fillMaxWidth()
            .windowInsetsPadding(WindowInsets.navigationBars)
            .padding(bottom = Space.s),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        state.notice?.let { notice ->
            Text(
                text = notice,
                style = AmuxTheme.typography.detail,
                color = colors.removed,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = AmuxTheme.metrics.gutter + 6.dp)
                    .testTag(ReviewTags.NOTICE),
            )
        }
        if (state.selected != null && selectionWords != null) {
            SelectionBar(
                words = selectionWords,
                canComment = state.canComment,
                onCancel = { onAction(ReviewAction.ClearSelection) },
                onComment = onWrite,
            )
        } else if (drafts > 0) {
            val done = ChatWords.reviewDone(drafts)
            Box(
                modifier = Modifier
                    .padding(horizontal = AmuxTheme.metrics.gutter)
                    .fillMaxWidth()
                    .heightIn(min = ThumbTarget.MinSize)
                    .clip(AmuxShapes.Pill)
                    .clickable(role = Role.Button) { onAction(ReviewAction.Done) }
                    .testTag(ReviewTags.DONE),
                contentAlignment = Alignment.Center,
            ) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(44.dp)
                        .background(colors.ink, AmuxShapes.Pill),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        text = done,
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.ground,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

/** Cancel, and the comment the selection can take, on a floating bar over the lines. */
@Composable
private fun SelectionBar(
    words: String,
    canComment: Boolean,
    onCancel: () -> Unit,
    onComment: () -> Unit,
) {
    val colors = AmuxTheme.colors
    AmuxChrome(
        shape = AmuxShapes.Pill,
        modifier = Modifier
            .padding(horizontal = AmuxTheme.metrics.gutter)
            .fillMaxWidth(),
    ) {
        Row(
            modifier = Modifier.padding(horizontal = Space.xs, vertical = Space.xs),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TextButton(
                onClick = onCancel,
                modifier = Modifier.testTag(ReviewTags.DESELECT),
            ) {
                Text(
                    text = stringResource(R.string.action_cancel),
                    style = AmuxTheme.typography.body,
                    color = colors.inkMuted,
                )
            }
            Spacer(modifier = Modifier.weight(1f))
            Box(
                modifier = Modifier
                    .heightIn(min = ThumbTarget.MinSize)
                    .clip(AmuxShapes.Pill)
                    .clickable(enabled = canComment, role = Role.Button, onClick = onComment)
                    .testTag(ReviewTags.COMMENT_ON),
                contentAlignment = Alignment.Center,
            ) {
                Box(
                    modifier = Modifier
                        .height(44.dp)
                        .background(if (canComment) colors.ink else colors.sunken, AmuxShapes.Pill)
                        .padding(horizontal = 18.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        text = words,
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = if (canComment) colors.ground else colors.inkFaint,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

// --- Writing a comment -----------------------------------------------------

/**
 * A comment being written or edited, in a sheet under the lines it is about.
 * Editing adds the way to delete it. The sheet keeps its own words, so typing
 * redraws the sheet and not the page under it.
 */
@Composable
private fun ReviewCommentSheet(
    title: String,
    initial: String,
    key: Any?,
    editing: Boolean,
    onDismiss: () -> Unit,
    onAct: (String) -> Unit,
    onDelete: () -> Unit = {},
) {
    AmuxSheet(onDismissRequest = onDismiss) {
        ReviewCommentSheetContent(
            title = title,
            initial = initial,
            key = key,
            editing = editing,
            onDismiss = onDismiss,
            onAct = onAct,
            onDelete = onDelete,
        )
    }
}

/**
 * What the comment sheet holds: what the comment is on, its words, and Add
 * Comment, or for one being edited Save and Delete. Drawn on its own where
 * the sheet's window cannot be, as a story pictures it.
 */
@Composable
fun ReviewCommentSheetContent(
    title: String,
    initial: String,
    key: Any?,
    editing: Boolean,
    onDismiss: () -> Unit,
    onAct: (String) -> Unit,
    onDelete: () -> Unit = {},
    focusOnOpen: Boolean = true,
) {
    val colors = AmuxTheme.colors
    // Held with its caret, which starts after the words being edited.
    var note by rememberSaveable(key, stateSaver = TextFieldValue.Saver) {
        mutableStateOf(TextFieldValue(initial, selection = TextRange(initial.length)))
    }
    val focus = remember { FocusRequester() }
    val blank = note.text.isBlank()
    val label = stringResource(R.string.review_comment_field)
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(bottom = Space.l)
            .testTag(ReviewTags.SHEET),
    ) {
        AmuxSheetHeader(
            title = title,
            onClose = onDismiss,
            closeLabel = stringResource(R.string.action_cancel),
            closeModifier = Modifier.testTag(ReviewTags.CANCEL),
        )
        AmuxField(
            value = note,
            onValueChange = { note = it },
            placeholder = label,
            singleLine = false,
            minLines = 2,
            maxLines = 6,
            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
            modifier = Modifier
                .padding(horizontal = Space.margin)
                .fillMaxWidth()
                .focusRequester(focus)
                .semantics { contentDescription = label }
                .testTag(ReviewTags.NOTE),
        )
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = Space.margin - Space.s, end = Space.margin, top = Space.m),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (editing) {
                TextButton(
                    onClick = onDelete,
                    modifier = Modifier.testTag(ReviewTags.DELETE),
                ) {
                    Text(
                        text = stringResource(R.string.review_delete),
                        style = AmuxTheme.typography.body,
                        color = colors.removed,
                    )
                }
            }
            Spacer(modifier = Modifier.weight(1f))
            AmuxActionButton(
                label = stringResource(if (editing) R.string.review_save else R.string.review_add_comment),
                kind = ActionButtonKind.Primary,
                enabled = !blank,
                onClick = { if (!blank) onAct(note.text.trim()) },
                modifier = Modifier.testTag(if (editing) ReviewTags.SAVE else ReviewTags.ADD),
            )
        }
    }
    if (focusOnOpen) LaunchedEffect(Unit) { runCatching { focus.requestFocus() } }
}

@Composable
private fun ReviewConfirm(
    question: String,
    detail: String,
    confirm: String,
    confirmTag: String,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    val colors = AmuxTheme.colors
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = colors.raised,
        titleContentColor = colors.ink,
        textContentColor = colors.inkMuted,
        title = { Text(text = question, style = AmuxTheme.typography.bodyEmphasis) },
        text = { Text(text = detail, style = AmuxTheme.typography.detail, color = colors.inkMuted) },
        confirmButton = {
            TextButton(onClick = onConfirm, modifier = Modifier.testTag(confirmTag)) {
                Text(text = confirm, style = AmuxTheme.typography.bodyEmphasis, color = colors.removed)
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(
                    text = stringResource(R.string.action_cancel),
                    style = AmuxTheme.typography.body,
                    color = colors.inkMuted,
                )
            }
        },
    )
}

/** Something on its way, with nothing yet to draw: the review opening, or its chat. */
@Composable
internal fun ReviewWaiting(text: String, modifier: Modifier = Modifier) {
    Row(
        modifier = modifier
            .padding(start = Space.margin, end = Space.margin, top = Space.l)
            .semantics(mergeDescendants = true) {}
            .testTag(ReviewTags.SCREEN + ".waiting"),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
    ) {
        AmuxStateMark(state = RowState.Working)
        Text(text = text, style = AmuxTheme.typography.bodyEmphasis, color = AmuxTheme.colors.ink)
    }
}

/**
 * The review page before its chat is open: "Review" over the chat being
 * read, or why it could not be.
 */
@Composable
fun ReviewPlaceholderScreen(
    failure: String?,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    AmuxGround(
        modifier = modifier
            .fillMaxSize()
            .testTag(ReviewTags.SCREEN),
    ) {
        Scaffold(
            containerColor = Color.Transparent,
            contentColor = colors.ink,
            topBar = {
                AmuxTopBar(
                    title = stringResource(R.string.review_title),
                    onNavigate = onBack,
                    navigationModifier = Modifier.testTag(ReviewTags.BACK),
                )
            },
        ) { innerPadding ->
            Box(modifier = Modifier.padding(innerPadding)) {
                if (failure == null) {
                    ReviewWaiting(text = stringResource(R.string.review_reading_changes))
                } else {
                    Text(
                        text = failure,
                        style = AmuxTheme.typography.detail,
                        color = colors.removed,
                        modifier = Modifier.padding(start = Space.margin, end = Space.margin, top = Space.l),
                    )
                }
            }
        }
    }
}
