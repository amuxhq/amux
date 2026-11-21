package sh.amux.features

import sh.amux.design.IconSize
import sh.amux.design.AmuxShapes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ArrowDownward
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flowOf
import sh.amux.design.AmuxChrome
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.BlobRef
import sh.amux.values.GroupState
import sh.amux.values.Offline
import sh.amux.values.OpenedAgent
import sh.amux.values.Reach
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Run
import sh.amux.values.RunCounts
import sh.amux.values.TurnPart
import sh.amux.words.ChatWords
import sh.amux.words.RailJoin
import sh.amux.words.RunPlacement

/** Per-key cell snapshot observed by a single item in the chat transcript. */
@Immutable
data class ChatRowCellUi(
    val id: String,
    val row: Row? = null,
    val revision: Int = 0,
    val attachmentRevision: Int = 0,
    val openedAgent: OpenedAgent? = null,
    val groupState: GroupState? = null,
)

/**
 * Per-row data source read lazily by [ChatScreen]'s transcript `LazyColumn`.
 *
 * Implementations backed by a live `ChatModel` observe only that row's cell
 * state inside [observeCell] so updating one row recomposes only that cell.
 */
@Stable
interface ChatRowSource {
    @Composable
    fun observeCell(id: String): ChatRowCellUi

    fun shows(row: Row): Boolean
    fun rowBelow(id: String): Row?
    fun countsOf(run: Run): RunCounts?
    fun imageBytes(blob: BlobRef): ByteArray?

    /**
     * The revision of row [id] as it changes, read outside composition by
     * whatever follows it (the transcript keeping its newest row in view
     * while a reply streams in) so a token does not recompose the screen.
     */
    fun revisions(id: String): Flow<Int>
}

/**
 * In-memory [ChatRowSource] over a static row list, convenient for Robolectric
 * UI tests and headless storybook fixtures.
 */
class StaticChatRowSource(
    rows: List<Row>,
    private val expanded: Set<String> = emptySet(),
    private val openedAgents: Map<List<Int>, OpenedAgent> = emptyMap(),
    private val groupStates: Map<String, GroupState> = emptyMap(),
    private val images: Map<List<Int>, ByteArray> = emptyMap(),
) : ChatRowSource {
    private val ordered: List<Row> = rows
    private val byId: Map<String, Row> = rows.associateBy { it.id }
    private val indexById: Map<String, Int> = rows.mapIndexed { index, row -> row.id to index }.toMap()

    @Composable
    override fun observeCell(id: String): ChatRowCellUi {
        val row = byId[id]
        return ChatRowCellUi(
            id = id,
            row = row,
            openedAgent = row?.opens?.let { openedAgents[it] },
            groupState = row?.group?.let { groupStates[it.first] },
        )
    }

    override fun shows(row: Row): Boolean {
        if (row.run != null && isPassThroughRunKind(row.kind)) return false
        val turn = row.turn
        if (turn != null && !turn.open && turn.part == TurnPart.Work) return false
        return !row.collapsed || (row.parent != null && row.parent in expanded)
    }

    override fun rowBelow(id: String): Row? {
        val start = (indexById[id] ?: return null) + 1
        for (i in start until ordered.size) {
            val candidate = ordered[i]
            if (candidate.kind is RowKind.Hidden) continue
            if (shows(candidate)) return candidate
        }
        return null
    }

    override fun countsOf(run: Run): RunCounts? = byId[run.last]?.run?.counts

    override fun imageBytes(blob: BlobRef): ByteArray? = images[blob.hash]

    override fun revisions(id: String): Flow<Int> = flowOf(0)

    private fun isPassThroughRunKind(kind: RowKind): Boolean = when (kind) {
        is RowKind.Thinking,
        is RowKind.Error,
        RowKind.Hidden,
        -> true
        else -> false
    }
}

@Composable
internal fun ChatTranscriptList(
    state: ChatScreenState,
    rowSource: ChatRowSource,
    listState: LazyListState,
    callbacks: ChatRowCallbacks,
    onRetryPaging: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val pagingMessage: String? = when (val paging = state.paging) {
        ChatPagingUi.Idle -> null
        ChatPagingUi.Fetching -> stringResource(R.string.chat_paging_loading)
        ChatPagingUi.Unreachable -> stringResource(R.string.chat_paging_unreachable, state.subject.host)
        is ChatPagingUi.Failed -> paging.reason
    }
    val canRetryPaging = state.paging is ChatPagingUi.Failed || state.paging is ChatPagingUi.Unreachable

    val emptyNotice: Pair<String, String>? = if (state.empty && state.frame?.caughtUp != true) {
        if (!state.subject.reachable && !state.subject.connecting) {
            val awayText = when (val reach = state.subject.reach) {
                is Reach.Offline -> when (reach.value) {
                    Offline.Revoked -> stringResource(R.string.chat_away_revoked, state.subject.host)
                    Offline.SignedOut -> stringResource(R.string.chat_away_signed_out, state.subject.host)
                    Offline.Plain -> stringResource(R.string.chat_away_plain, state.subject.host)
                }
                Reach.Away -> stringResource(R.string.chat_away_free, state.subject.host)
                is Reach.Online -> stringResource(R.string.chat_away_plain, state.subject.host)
            }
            ChatScreenTags.AWAY to awayText
        } else if (state.loadingHint) {
            ChatScreenTags.LOADING to stringResource(R.string.chat_loading_from_host, state.subject.host)
        } else {
            null
        }
    } else {
        null
    }

    LazyColumn(
        state = listState,
        modifier = Modifier
            .fillMaxSize()
            .testTag(ChatScreenTags.FEED),
        contentPadding = PaddingValues(
            horizontal = Space.margin,
            vertical = Space.m,
        ),
    ) {
        if (pagingMessage != null) {
            item(key = "chat-paging-notice") {
                TranscriptNoticeRow(
                    text = pagingMessage,
                    tag = ChatScreenTags.PAGING,
                    onRetry = if (canRetryPaging) onRetryPaging else null,
                )
            }
        }

        if (emptyNotice != null) {
            item(key = "chat-empty-notice") {
                TranscriptNoticeRow(
                    text = emptyNotice.second,
                    tag = emptyNotice.first,
                )
            }
        }

        itemsIndexed(
            items = state.keys,
            key = { _, id -> id },
        ) { index, rowId ->
            ChatTranscriptRowItem(
                rowId = rowId,
                nextKey = state.keys.getOrNull(index + 1),
                expanded = rowId in state.expanded,
                openedAgents = state.openedAgents,
                groupStates = state.groupStates,
                rowSource = rowSource,
                callbacks = callbacks,
            )
        }

        itemsIndexed(
            items = state.landingInFeed,
            key = { index, prompt -> "landing:$index:${prompt.inputId}" },
        ) { index, prompt ->
            val stateText = remember(prompt.underway, state.subject.host) {
                ChatWords.underway(prompt.underway, host = state.subject.host)
            }
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(ChatScreenTags.landing(index)),
                horizontalAlignment = Alignment.End,
            ) {
                PromptBubbleRow(
                    segments = prompt.text,
                    steered = false,
                    // A prompt still landing has no row to say its image's
                    // bytes arrived, so each new chat state lets the image
                    // look again; one already decoded comes from the cache.
                    imageBytes = remember(rowSource, state) { { blob: BlobRef -> rowSource.imageBytes(blob) } },
                    onOpenImage = callbacks.onOpenImage,
                    onOpenAttachment = callbacks.onOpenAttachment,
                    onOpenLink = callbacks.onOpenLink,
                    onCopyCode = callbacks.onCopyCode,
                )
                Text(
                    text = stateText,
                    style = AmuxTheme.typography.caption,
                    color = colors.inkFaint,
                    modifier = Modifier.padding(bottom = Space.s),
                )
            }
        }
    }
}

@Composable
private fun ChatTranscriptRowItem(
    rowId: String,
    nextKey: String?,
    expanded: Boolean,
    openedAgents: Map<List<Int>, OpenedAgent>,
    groupStates: Map<String, GroupState>,
    rowSource: ChatRowSource,
    callbacks: ChatRowCallbacks,
) {
    val cell = rowSource.observeCell(rowId)
    val row = cell.row ?: return
    if (!rowSource.shows(row)) return

    val rowBelow = rowSource.rowBelow(row.id)
    val rail = remember(row, cell.revision, nextKey, rowBelow?.id, rowBelow?.kind, rowBelow?.run) {
        RailJoin.of(row, next = rowBelow)
    }
    val placement = remember(row, cell.revision) {
        RunPlacement.of(
            row = row,
            open = row.run?.open == true,
            counts = { row.run?.let(rowSource::countsOf) },
        )
    }
    val opened = cell.openedAgent ?: row.opens?.let { openedAgents[it] }
    val groupState = cell.groupState ?: row.group?.let { groupStates[it.first] }

    ChatRowView(
        row = row,
        expanded = expanded,
        shown = true,
        rail = rail,
        placement = placement,
        openedAgents = openedAgents,
        opened = opened,
        groupState = groupState,
        // A new reader each time the cell's bytes land, so an image waiting
        // on them looks again; the bytes themselves are read outside
        // composition, by the image that draws them.
        imageBytes = remember(rowSource, cell.attachmentRevision) {
            { blob: BlobRef -> rowSource.imageBytes(blob) }
        },
        callbacks = callbacks,
    )
}

@Composable
private fun TranscriptNoticeRow(
    text: String,
    tag: String,
    onRetry: (() -> Unit)? = null,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = Space.l)
            .testTag(tag),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        Text(
            text = text,
            style = AmuxTheme.typography.detail,
            color = AmuxTheme.colors.inkMuted,
            textAlign = TextAlign.Center,
            modifier = Modifier.fillMaxWidth(),
        )
        if (onRetry != null) {
            TextButton(
                onClick = onRetry,
                modifier = Modifier.testTag(ChatScreenTags.PAGING_RETRY),
            ) {
                Text(
                    text = stringResource(R.string.action_retry),
                    style = AmuxTheme.typography.bodyEmphasis,
                    color = AmuxTheme.colors.ink,
                )
            }
        }
    }
}

@Composable
internal fun NewActivityPill(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val label = stringResource(R.string.chat_new_activity)
    AmuxChrome(
        modifier = modifier
            .thumbTarget()
            .clip(AmuxShapes.Pill)
            .clickable(role = Role.Button, onClick = onClick)
            .testTag(ChatScreenTags.NEW_ACTIVITY),
        shape = AmuxShapes.Pill,
    ) {
        Row(
            modifier = Modifier.padding(horizontal = Space.l, vertical = Space.s),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.xs + Space.xxs),
        ) {
            Icon(
                imageVector = Icons.Outlined.ArrowDownward,
                contentDescription = null,
                tint = colors.accent,
                modifier = Modifier.size(IconSize.Inline),
            )
            Text(
                text = label,
                style = AmuxTheme.typography.bodyEmphasis,
                color = colors.ink,
            )
        }
    }
}

@Composable
internal fun JumpToNewestButton(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val label = stringResource(R.string.chat_go_to_newest)
    AmuxChrome(
        modifier = modifier
            .size(48.dp)
            .clip(CircleShape)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics { contentDescription = label }
            .testTag(ChatScreenTags.TO_NEWEST),
        shape = CircleShape,
    ) {
        Box(contentAlignment = Alignment.Center) {
            Icon(
                imageVector = Icons.Outlined.ArrowDownward,
                contentDescription = null,
                tint = colors.ink,
                modifier = Modifier.size(IconSize.Row),
            )
        }
    }
}
