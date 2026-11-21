package sh.amux.features

import android.graphics.BitmapFactory
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.imeNestedScroll
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.ListAlt
import androidx.compose.material.icons.automirrored.outlined.Undo
import androidx.compose.material.icons.outlined.AccountTree
import androidx.compose.material.icons.outlined.ArrowDownward
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.NorthWest
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material.icons.outlined.RateReview
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.outlined.Tune
import androidx.compose.material.icons.outlined.Unarchive
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import java.io.File
import java.time.Instant
import kotlinx.coroutines.flow.distinctUntilChanged
import sh.amux.design.AmuxChrome
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.LocalAmuxPhotographed
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.AgentKey
import sh.amux.values.AgentNameProblem
import sh.amux.values.AskCard
import sh.amux.values.AttachmentView
import sh.amux.values.Attention
import sh.amux.values.BlobRef
import sh.amux.values.CardState
import sh.amux.values.ChangeTotals
import sh.amux.values.ChatFrame
import sh.amux.values.CommandMatch
import sh.amux.values.Comparison
import sh.amux.values.Composer
import sh.amux.values.ComposerView
import sh.amux.values.DeleteWarning
import sh.amux.values.FamilyHeader
import sh.amux.values.Filing
import sh.amux.values.FleetCard
import sh.amux.values.FormField
import sh.amux.values.FormProblem
import sh.amux.values.FormValue
import sh.amux.values.GroupState
import sh.amux.values.Offline
import sh.amux.values.OpenedAgent
import sh.amux.values.Overview
import sh.amux.values.PhaseView
import sh.amux.values.QueuedRow
import sh.amux.values.Reach
import sh.amux.values.RouteKind
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Run
import sh.amux.values.RunCounts
import sh.amux.values.SentPrompt
import sh.amux.values.SettingChange
import sh.amux.values.SettingsView
import sh.amux.values.TurnPart
import sh.amux.values.Waiting
import sh.amux.values.WorkspaceGoes
import sh.amux.words.ChatWords
import sh.amux.words.DeleteWords
import sh.amux.words.EMPTY_GROUP_STATE
import sh.amux.words.OpenableLink
import sh.amux.words.PlaceNames
import sh.amux.words.RailJoin
import sh.amux.words.RowState
import sh.amux.words.RunPlacement

/** Semantic test tags for [ChatScreen] and its headers, banners, feed, and dialogs. */
object ChatScreenTags {
    const val TOP_BAR: String = "chat.topBar"
    const val TITLE: String = "chat.title"
    const val HEADER: String = "chat.header"
    const val CHANGES: String = "chat.changes"
    const val OVERVIEW_ACTION: String = "chat.overview.action"
    const val MORE: String = "chat.more"
    const val MENU_OVERVIEW: String = "chat.menu.overview"
    const val MENU_SETTINGS: String = "chat.menu.settings"
    const val MENU_REVIEW: String = "chat.menu.review"
    const val MENU_PIN: String = "chat.menu.pin"
    const val MENU_UNPIN: String = "chat.menu.unpin"
    const val MENU_RENAME: String = "chat.menu.rename"
    const val MENU_COPY_ADDRESS: String = "chat.menu.copyAddress"
    const val MENU_SETTLE: String = "chat.menu.settle"
    const val MENU_UNSETTLE: String = "chat.menu.unsettle"
    const val MENU_CANCEL_STOP: String = "chat.menu.cancelStop"
    const val MENU_DELETE: String = "chat.menu.delete"
    const val FAMILY_PARENT: String = "chat.family.parent"
    const val FAMILY_CHILDREN: String = "chat.family.children"
    const val HELD_STOP: String = "chat.settleHeld"
    const val HELD_STOP_CANCEL: String = "chat.settleHeld.cancel"
    const val ENDED_BANNER: String = "chat.ended"
    const val FEED: String = "chat.feed"
    const val PAGING: String = "chat.paging"
    const val PAGING_RETRY: String = "chat.paging.retry"
    const val LOADING: String = "chat.loading"
    const val AWAY: String = "chat.away"
    const val NEW_ACTIVITY: String = "chat.newActivity"
    const val TO_NEWEST: String = "chat.toNewest"
    const val PREVIEW_DIALOG: String = "chat.preview"
    const val PREVIEW_CLOSE: String = "chat.preview.close"
    const val PREVIEW_OPEN: String = "chat.preview.open"
    const val PREVIEW_SHARE: String = "chat.preview.share"
    const val COMMAND_REPLACE_DIALOG: String = "chat.command.replace.dialog"
    const val COMMAND_REPLACE_CONFIRM: String = "chat.command.replace.confirm"
    const val COMMAND_REPLACE_KEEP: String = "chat.command.replace.keep"
    const val RENAME_DIALOG: String = "chat.rename.dialog"
    const val RENAME_INPUT: String = "chat.rename.field"
    const val RENAME_PROBLEM: String = "chat.rename.problem"
    const val RENAME_CONFIRM: String = "chat.rename.confirm"
    const val SETTLE_DIALOG: String = "chat.settle.dialog"
    const val SETTLE_CONFIRM: String = "chat.settle.confirm"
    const val DELETE_DIALOG: String = "chat.delete.dialog"
    const val DELETE_CONFIRM: String = "chat.delete.confirm"

    fun familyChild(name: String): String = "chat.family.child.$name"
    fun landing(index: Int): String = "chat.landing.$index"
}

/** Who a chat is with and where it runs, painted from the fleet before rows arrive. */
@Immutable
data class ChatSubject(
    val name: String,
    val host: String = "",
    val directory: String = "",
    val branch: String? = null,
    val reach: Reach = Reach.Online(RouteKind.Direct),
    val connecting: Boolean = false,
    val descendants: Int = 0,
    val backgroundJobs: Int = 0,
    val closeHeld: Boolean = false,
    val filing: Filing = Filing.Open,
    val topLevel: Boolean = true,
    val workspaceGoes: WorkspaceGoes? = null,
    val native: Boolean = false,
    val key: AgentKey? = null,
) {
    val reachable: Boolean
        get() = reach is Reach.Online

    val address: String
        get() = listOfNotNull(
            name.takeIf { it.isNotEmpty() },
            host.takeIf { it.isNotEmpty() }?.lowercase(),
        ).joinToString("/")

    fun toOverviewUi(): ChatSubjectUi = ChatSubjectUi(
        name = name,
        host = host,
        directory = directory,
        branch = branch,
        address = if (native) "" else address,
    )
}

/** Top-edge history paging status rendered above the oldest transcript row. */
sealed interface ChatPagingUi {
    data object Idle : ChatPagingUi
    data object Fetching : ChatPagingUi
    data object Unreachable : ChatPagingUi
    data class Failed(val reason: String) : ChatPagingUi
}

/** One-shot overlay requested by a slash command (such as `/model` or `/rename`). */
enum class ChatRequestedOverlay {
    Settings,
    Rename,
}

/** Attachment preview currently opened in a full-screen dialog. */
@Immutable
data class ChatOpenedAttachmentUi(
    val name: String,
    val bytes: ByteArray? = null,
    val isImage: Boolean = true,
    val file: File? = null,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ChatOpenedAttachmentUi) return false
        return name == other.name &&
            bytes.contentEquals(other.bytes) &&
            isImage == other.isImage &&
            file == other.file
    }

    override fun hashCode(): Int {
        var result = name.hashCode()
        result = 31 * result + (bytes?.contentHashCode() ?: 0)
        result = 31 * result + isImage.hashCode()
        result = 31 * result + (file?.hashCode() ?: 0)
        return result
    }
}

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

    private fun isPassThroughRunKind(kind: RowKind): Boolean = when (kind) {
        is RowKind.Thinking,
        is RowKind.Error,
        RowKind.Hidden,
        -> true
        else -> false
    }
}

/** Pure immutable UI state rendered by [ChatScreen]. */
@Immutable
data class ChatScreenState(
    val subject: ChatSubject,
    val family: FamilyHeader? = null,
    val keys: List<String> = emptyList(),
    val empty: Boolean = keys.isEmpty(),
    val frame: ChatFrame? = null,
    val ask: AskCard? = null,
    val overview: Overview? = null,
    val overviewChangesComparison: Comparison? = null,
    val overviewChangesLoading: Boolean = false,
    val overviewChangesError: String? = null,
    val settings: SettingsView? = null,
    val openedAgents: Map<List<Int>, OpenedAgent> = emptyMap(),
    val liveSubagents: GroupState = EMPTY_GROUP_STATE,
    val groupStates: Map<String, GroupState> = emptyMap(),
    val expanded: Set<String> = emptySet(),
    val paging: ChatPagingUi = ChatPagingUi.Idle,
    val loadingHint: Boolean = false,
    val following: Boolean = true,
    val toNewest: Int = 0,
    val draft: String = "",
    val attachments: List<ComposerAttachmentUi> = emptyList(),
    val uploadingCount: Int = 0,
    val sending: Boolean = false,
    val canSend: Boolean = false,
    val canResume: Boolean = false,
    val notice: String? = null,
    val comparison: Comparison = Comparison.Uncommitted,
    val comparisons: List<Comparison> = listOf(Comparison.Uncommitted),
    val changes: ChangeTotals? = null,
    val slashMatches: List<CommandMatch> = emptyList(),
    val activitySubject: String? = null,
    val landingInFeed: List<SentPrompt> = emptyList(),
    val landingInQueue: List<SentPrompt> = emptyList(),
    val keptQuestionDraft: QuestionDraft? = null,
    val keptFormValues: List<FormValue>? = null,
    val requestedOverlay: ChatRequestedOverlay? = null,
    val openedAttachment: ChatOpenedAttachmentUi? = null,
    val dictationEnabled: Boolean = false,
    val dictationActive: Boolean = false,
    val dictationSentence: String? = null,
    val nowMs: Long = 0L,
) {
    val arrivalsHeld: Boolean
        get() = frame?.arrivalsHeld == true

    val newActivity: Boolean
        get() = !following && arrivalsHeld

    val hasOverviewHighlights: Boolean
        get() {
            val ov = overview ?: return false
            val hasTasks = ov.tasks?.let { it.total > 0 || it.entries.isNotEmpty() } == true
            return hasTasks ||
                ov.jobs.isNotEmpty() ||
                ov.failedServers.isNotEmpty() ||
                ov.usageNearLimit != null ||
                (ov.changes?.totals?.files ?: 0) > 0
        }
}

/** Pure Kotlin agent-name validation matching the host rules for offline tests/previews. */
fun defaultValidateAgentName(name: String): AgentNameProblem? {
    val trimmed = name.trim()
    if (trimmed.isEmpty()) return AgentNameProblem.Empty
    if (trimmed.length > 64) return AgentNameProblem.TooLong
    val first = trimmed.first()
    if (!(first in 'a'..'z' || first in '0'..'9')) return AgentNameProblem.Characters
    if (trimmed.any { ch -> !(ch in 'a'..'z' || ch in '0'..'9' || ch == '-') }) {
        return AgentNameProblem.Characters
    }
    return null
}

/** Callbacks emitted by [ChatScreen] without coupling to runtime, platform pickers, or navigation. */
@Immutable
data class ChatScreenCallbacks(
    val onBack: () -> Unit = {},
    val onReadingChange: (atNewest: Boolean) -> Unit = {},
    val onReachedTop: (firstVisibleIndex: Int) -> Unit = {},
    val onJumpToNewest: () -> Unit = {},
    val onToggleExpand: (rowId: String) -> Unit = {},
    val onToggleRun: (rowId: String) -> Unit = {},
    val onToggleGroup: (rowId: String) -> Unit = {},
    val onOpenAgent: (AgentKey) -> Unit = {},
    val onOpenReview: (filePath: String?) -> Unit = {},
    val onOpenAttachment: (BlobRef) -> Unit = {},
    val onOpenAttachmentExternally: (File) -> Unit = {},
    val onShareAttachment: (File) -> Unit = {},
    val onClearOpenedAttachment: () -> Unit = {},
    val onOpenLink: (OpenableLink) -> Unit = {},
    val onCopyCode: (String) -> Unit = {},
    val onCopyAddress: (String) -> Unit = {},
    val onDraftChange: (String) -> Unit = {},
    val onSend: () -> Unit = {},
    val onResume: () -> Unit = {},
    val onInterrupt: () -> Unit = {},
    val onAttach: (AttachChoice) -> Unit = {},
    val onRemoveAttachment: (Int) -> Unit = {},
    val onPickCommand: (CommandMatch) -> Unit = {},
    val onSendNow: (QueuedRow) -> Unit = {},
    val onWithdraw: (QueuedRow) -> Unit = {},
    val onResend: (List<Int>) -> Unit = {},
    val onDiscard: (List<Int>) -> Unit = {},
    val onDismissRefused: (List<Int>) -> Unit = {},
    val onSwitchBack: () -> Unit = {},
    val onStopJob: (String) -> Unit = {},
    val onToggleDictation: () -> Unit = {},
    val onAskAction: (AskAction) -> Unit = {},
    val onQuestionDraftChange: (askKey: String, QuestionDraft) -> Unit = { _, _ -> },
    val onFormValuesChange: (askKey: String, List<FormValue>) -> Unit = { _, _ -> },
    val validateForm: (List<FormField>, List<FormValue>) -> List<FormProblem> = ::validateFormFields,
    val onSelectComparison: (Comparison) -> Unit = {},
    val onRetryChangedFiles: () -> Unit = {},
    val onChangeSetting: (SettingChange) -> Unit = {},
    val onOverviewOpenChange: (Boolean) -> Unit = {},
    val onClearRequestedOverlay: () -> Unit = {},
    val onPin: () -> Unit = {},
    val onUnpin: () -> Unit = {},
    val onRename: (String) -> Unit = {},
    val onSettle: () -> Unit = {},
    val onUnsettle: () -> Unit = {},
    val onCancelSettle: () -> Unit = {},
    val onDelete: (discardingChanges: Boolean) -> Unit = {},
    val onPreviewDelete: suspend () -> Result<DeleteWarning?> = { Result.success(null) },
    val validateAgentName: (String) -> AgentNameProblem? = ::defaultValidateAgentName,
)

private const val TOP_PAGING_THRESHOLD: Int = 40

/**
 * Full-screen agent conversation view.
 *
 * Composes the top app bar, optional family header, held-stop / ended banners,
 * keyed per-row transcript `LazyColumn`, floating New activity pill, and the
 * edge-to-edge IME-aware bottom dock (switching cleanly between [AskCardView]
 * and [ChatOverviewStrip] + [ChatComposer]).
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun ChatScreen(
    state: ChatScreenState,
    rowSource: ChatRowSource,
    callbacks: ChatScreenCallbacks = ChatScreenCallbacks(),
    modifier: Modifier = Modifier,
) {
    val chatIdentity: Any = state.subject.key
        ?: state.frame?.agent
        ?: state.subject.address.ifEmpty { state.subject.name }

    key(chatIdentity) {
        ChatScreenContent(
            state = state,
            rowSource = rowSource,
            callbacks = callbacks,
            modifier = modifier,
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
private fun ChatScreenContent(
    state: ChatScreenState,
    rowSource: ChatRowSource,
    callbacks: ChatScreenCallbacks,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val listState = rememberLazyListState()
    val currentCallbacks by rememberUpdatedState(callbacks)
    val currentFollowing by rememberUpdatedState(state.following)

    var overviewOpen by remember { mutableStateOf(false) }
    var settingsOpen by remember { mutableStateOf(false) }
    var renameOpen by remember { mutableStateOf(false) }
    var settleConfirmCuts by remember { mutableStateOf<String?>(null) }
    var deleteOpen by remember { mutableStateOf(false) }
    var pendingCommandPrefix by remember { mutableStateOf<String?>(null) }
    var localPreview by remember { mutableStateOf<ChatOpenedAttachmentUi?>(null) }

    LaunchedEffect(overviewOpen) {
        currentCallbacks.onOverviewOpenChange(overviewOpen)
    }

    LaunchedEffect(state.requestedOverlay) {
        when (state.requestedOverlay) {
            ChatRequestedOverlay.Settings -> {
                settingsOpen = true
                currentCallbacks.onClearRequestedOverlay()
            }
            ChatRequestedOverlay.Rename -> {
                renameOpen = true
                currentCallbacks.onClearRequestedOverlay()
            }
            null -> Unit
        }
    }

    val descendants = remember(state.subject.descendants, state.family) {
        val fromFamily = state.family?.children?.sumOf { it.members } ?: 0
        maxOf(state.subject.descendants, fromFamily)
    }

    val requestSettle: () -> Unit = {
        val cuts = ChatWords.closeCuts(
            name = state.subject.name,
            phase = state.frame?.phase,
            jobs = maxOf(state.subject.backgroundJobs, state.overview?.jobs?.size ?: 0),
            subAgents = descendants,
        )
        if (cuts == null) {
            callbacks.onSettle()
        } else {
            settleConfirmCuts = cuts
        }
    }

    // Observe tail row identity + streaming revision so cap-scrolling (same key count,
    // new tail key) and growing streamed replies keep the viewport pinned to newest.
    val lastKey = state.keys.lastOrNull()
    val tailCell = if (lastKey != null) rowSource.observeCell(lastKey) else null
    val lastLandingId = state.landingInFeed.lastOrNull()?.inputId
    val totalTailAnchor = state.keys.size + state.landingInFeed.size
    var initialFollowDone by remember { mutableStateOf(!state.following || totalTailAnchor == 0) }
    var programmaticScrollCount by remember(listState) { mutableIntStateOf(0) }
    var programmaticScrollEpoch by remember(listState) { mutableIntStateOf(0) }
    var userScrolledSinceSettle by remember { mutableStateOf(false) }
    var userHasInteracted by remember { mutableStateOf(false) }
    val currentKeysAnchor by rememberUpdatedState(
        Triple(state.keys.firstOrNull(), state.keys.lastOrNull(), totalTailAnchor),
    )

    LaunchedEffect(
        state.following,
        state.toNewest,
        lastKey,
        tailCell?.revision,
        lastLandingId,
        totalTailAnchor,
    ) {
        if (state.following && totalTailAnchor > 0) {
            val totalItems = listState.layoutInfo.totalItemsCount
            val lastIndex = if (totalItems > 0) totalItems - 1 else totalTailAnchor - 1
            if (lastIndex >= 0) {
                programmaticScrollCount += 1
                try {
                    listState.scrollToItem(lastIndex)
                    val info = listState.layoutInfo
                    val lastVisible = info.visibleItemsInfo.lastOrNull()
                    if (lastVisible != null && lastVisible.index == info.totalItemsCount - 1) {
                        val overflow = (lastVisible.offset + lastVisible.size) - info.viewportEndOffset
                        if (overflow > 0) {
                            listState.scrollBy(overflow.toFloat())
                        }
                    }
                } finally {
                    programmaticScrollEpoch += 1
                    programmaticScrollCount -= 1
                }
            }
            initialFollowDone = true
        } else if (!state.following || totalTailAnchor == 0) {
            initialFollowDone = true
        }
    }

    // Any scroll outside our own auto-follow calls (touch drag, wheel, keyboard,
    // accessibility actions) updates reading/following state, while frame-0 layout,
    // programmatic scrollToItem, cap-scrolling, and IME viewport shrink do not.
    LaunchedEffect(listState) {
        var settledPosition: Pair<Int, Int>? = null
        var settledAnchor: Triple<String?, String?, Int>? = null
        var settledEpoch = programmaticScrollEpoch
        snapshotFlow {
            TranscriptScrollObservation(
                scrolling = listState.isScrollInProgress,
                programmatic = programmaticScrollCount,
                epoch = programmaticScrollEpoch,
                atBottom = isNearTranscriptBottom(listState),
                firstIndex = listState.firstVisibleItemIndex,
                firstOffset = listState.firstVisibleItemScrollOffset,
                followReady = initialFollowDone,
                anchor = currentKeysAnchor,
            )
        }
            .distinctUntilChanged()
            .collect { obs ->
                val pos = obs.firstIndex to obs.firstOffset
                if (!obs.followReady || obs.programmatic > 0) {
                    settledPosition = pos
                    settledAnchor = obs.anchor
                    settledEpoch = obs.epoch
                    return@collect
                }
                if (obs.scrolling) {
                    settledEpoch = obs.epoch
                    settledAnchor = obs.anchor
                    userScrolledSinceSettle = true
                    userHasInteracted = true
                } else if (userScrolledSinceSettle) {
                    userScrolledSinceSettle = false
                    settledPosition = pos
                    settledAnchor = obs.anchor
                    settledEpoch = obs.epoch
                    currentCallbacks.onReadingChange(obs.atBottom)
                } else if (obs.epoch != settledEpoch || obs.anchor != settledAnchor) {
                    settledPosition = pos
                    settledAnchor = obs.anchor
                    settledEpoch = obs.epoch
                } else if (settledPosition != null && pos != settledPosition) {
                    settledPosition = pos
                    userHasInteracted = true
                    currentCallbacks.onReadingChange(obs.atBottom)
                } else {
                    settledPosition = pos
                }
            }
    }

    LaunchedEffect(
        listState,
        state.keys.size,
        state.frame?.hasOlder,
        state.paging,
        initialFollowDone,
        userHasInteracted,
    ) {
        if (!initialFollowDone) return@LaunchedEffect
        snapshotFlow { listState.firstVisibleItemIndex }
            .distinctUntilChanged()
            .collect { firstVisible ->
                if ((!currentFollowing || userHasInteracted) &&
                    state.keys.isNotEmpty() &&
                    firstVisible < TOP_PAGING_THRESHOLD &&
                    state.frame?.hasOlder == true &&
                    state.paging is ChatPagingUi.Idle
                ) {
                    currentCallbacks.onReachedTop(firstVisible)
                }
            }
    }

    val rowCallbacks = remember(callbacks, rowSource) {
        ChatRowCallbacks(
            onToggleExpand = callbacks.onToggleExpand,
            onToggleRun = callbacks.onToggleRun,
            onToggleGroup = callbacks.onToggleGroup,
            onOpenAgent = callbacks.onOpenAgent,
            onOpenFileChange = { fileRow -> callbacks.onOpenReview(fileRow.path) },
            onOpenImage = { hash, label ->
                val blob = BlobRef(hash = hash, name = label, mime = "image/png", size = 0L)
                val bytes = if (hash.isNotEmpty()) rowSource.imageBytes(blob) else null
                if (bytes != null) {
                    localPreview = ChatOpenedAttachmentUi(name = label, bytes = bytes, isImage = true)
                } else if (hash.isNotEmpty()) {
                    callbacks.onOpenAttachment(blob)
                } else {
                    localPreview = ChatOpenedAttachmentUi(name = label, bytes = null, isImage = true)
                }
            },
            onOpenAttachment = { view ->
                when (view) {
                    is AttachmentView.Image -> {
                        val bytes = rowSource.imageBytes(view.value)
                        if (bytes != null) {
                            localPreview = ChatOpenedAttachmentUi(
                                name = view.value.name,
                                bytes = bytes,
                                isImage = true,
                            )
                        } else {
                            callbacks.onOpenAttachment(view.value)
                        }
                    }
                    is AttachmentView.File -> callbacks.onOpenAttachment(view.value)
                    else -> Unit
                }
            },
            onOpenLink = callbacks.onOpenLink,
            onCopyCode = callbacks.onCopyCode,
        )
    }

    val detached = state.frame?.composer?.mode == Composer.Waiting(Waiting.Detached)
    val placeLine = chatPlaceLine(state.subject, state.frame, detached)

    AmuxGround(
        modifier = modifier
            .fillMaxSize()
            .testTag(TestTags.SCREEN_CONVERSATION),
    ) {
        Scaffold(
            containerColor = Color.Transparent,
            contentColor = colors.ink,
            contentWindowInsets = WindowInsets(0, 0, 0, 0),
            topBar = {
                ChatTopBar(
                    subject = state.subject,
                    placeLine = placeLine,
                    unconfirmed = detached || !state.subject.reachable,
                    changes = state.changes,
                    hasOverviewHighlights = state.hasOverviewHighlights,
                    settingsAvailable = state.settings != null,
                    onBack = callbacks.onBack,
                    onOpenOverview = { overviewOpen = true },
                    onOpenSettings = { settingsOpen = true },
                    onOpenReview = { callbacks.onOpenReview(null) },
                    onPin = callbacks.onPin,
                    onUnpin = callbacks.onUnpin,
                    onOpenRename = { renameOpen = true },
                    onCopyAddress = { callbacks.onCopyAddress(state.subject.address) },
                    onSettle = requestSettle,
                    onUnsettle = callbacks.onUnsettle,
                    onCancelSettle = callbacks.onCancelSettle,
                    onOpenDelete = { deleteOpen = true },
                )
            },
        ) { innerPadding ->
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(innerPadding)
                    .windowInsetsPadding(WindowInsets.navigationBars.union(WindowInsets.ime)),
            ) {
                // Family header (parent back-chip for subagents; expandable children summary for head)
                val family = state.family
                if (family != null) {
                    ChatFamilyBar(
                        family = family,
                        onOpenAgent = callbacks.onOpenAgent,
                    )
                }

                // Held stop banner under header
                if (state.subject.closeHeld) {
                    HeldStopBanner(
                        host = state.subject.host,
                        onCancelStop = callbacks.onCancelSettle,
                    )
                }

                // Deleted / ended chat notice
                val endedMessage = state.frame?.ended
                if (!endedMessage.isNullOrBlank()) {
                    EndedChatBanner(message = endedMessage)
                }

                // Transcript feed + floating New activity pill
                Box(
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxWidth()
                        .imeNestedScroll(),
                ) {
                    ChatTranscriptList(
                        state = state,
                        rowSource = rowSource,
                        listState = listState,
                        callbacks = rowCallbacks,
                        onRetryPaging = { callbacks.onReachedTop(0) },
                    )

                    if (state.newActivity) {
                        NewActivityPill(
                            onClick = callbacks.onJumpToNewest,
                            modifier = Modifier
                                .align(Alignment.BottomCenter)
                                .padding(bottom = Space.s),
                        )
                    } else if (!state.following && state.keys.isNotEmpty()) {
                        JumpToNewestButton(
                            onClick = callbacks.onJumpToNewest,
                            modifier = Modifier
                                .align(Alignment.BottomCenter)
                                .padding(bottom = Space.s),
                        )
                    }
                }

                // Bottom dock: Ask card takes over composer; otherwise overview strip + composer
                ChatBottomDock(
                    state = state,
                    callbacks = callbacks,
                    onOpenOverview = { overviewOpen = true },
                    onOpenSettings = { settingsOpen = true },
                )
            }
        }
    }

    // Modal sheets & dialogs owned by ChatScreen
    if (overviewOpen) {
        ChatOverviewModalSheet(
            state = ChatOverviewState(
                subject = state.subject.toOverviewUi(),
                overview = state.overview,
                changesComparison = state.overviewChangesComparison,
                changesLoading = state.overviewChangesLoading,
                changesError = state.overviewChangesError,
                context = state.frame?.context,
                comparisons = state.comparisons,
                comparison = state.comparison,
                baseBranch = state.frame?.git?.baseBranch,
                changeTotals = state.changes,
                nowMs = state.nowMs,
            ),
            onAction = { action ->
                when (action) {
                    is ChatOverviewAction.StopJob -> callbacks.onStopJob(action.key)
                    is ChatOverviewAction.SelectComparison -> callbacks.onSelectComparison(action.comparison)
                    ChatOverviewAction.RetryChanges -> callbacks.onRetryChangedFiles()
                    is ChatOverviewAction.OpenReview -> {
                        overviewOpen = false
                        callbacks.onOpenReview(action.filePath)
                    }
                    ChatOverviewAction.Dismiss -> overviewOpen = false
                }
            },
        )
    }

    val currentSettings = state.settings
    if (settingsOpen && currentSettings != null) {
        ChatSettingsModalSheet(
            state = ChatSettingsState(
                settings = currentSettings,
                kind = state.frame?.kind,
                context = state.frame?.context,
            ),
            onChange = callbacks.onChangeSetting,
            onDismiss = { settingsOpen = false },
            onSelectCommand = { command ->
                settingsOpen = false
                val token = command.sigil.ifEmpty { "/" } + command.name
                val prefix = "$token "
                val trimmedStart = state.draft.trimStart()
                when {
                    state.draft.isBlank() -> callbacks.onDraftChange(prefix)
                    trimmedStart == token || trimmedStart.startsWith(prefix) -> Unit
                    else -> pendingCommandPrefix = prefix
                }
            },
        )
    }

    val commandToReplace = pendingCommandPrefix
    if (commandToReplace != null) {
        AlertDialog(
            onDismissRequest = { pendingCommandPrefix = null },
            containerColor = colors.raised,
            titleContentColor = colors.ink,
            textContentColor = colors.inkMuted,
            modifier = Modifier.testTag(ChatScreenTags.COMMAND_REPLACE_DIALOG),
            title = {
                Text(
                    text = stringResource(R.string.chat_command_replace_title),
                    style = AmuxTheme.typography.bodyEmphasis,
                )
            },
            text = {
                Text(
                    text = stringResource(R.string.chat_command_replace_detail, commandToReplace.trim()),
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        pendingCommandPrefix = null
                        callbacks.onDraftChange(commandToReplace)
                    },
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(ChatScreenTags.COMMAND_REPLACE_CONFIRM),
                ) {
                    Text(
                        text = stringResource(R.string.chat_command_replace_confirm),
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.ink,
                    )
                }
            },
            dismissButton = {
                TextButton(
                    onClick = { pendingCommandPrefix = null },
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(ChatScreenTags.COMMAND_REPLACE_KEEP),
                ) {
                    Text(
                        text = stringResource(R.string.chat_command_replace_keep),
                        style = AmuxTheme.typography.body,
                        color = colors.inkMuted,
                    )
                }
            },
        )
    }

    if (renameOpen) {
        ChatRenameDialog(
            currentName = state.subject.name,
            validateName = callbacks.validateAgentName,
            onDismiss = { renameOpen = false },
            onConfirm = { newName ->
                renameOpen = false
                callbacks.onRename(newName)
            },
        )
    }

    val cutsToConfirm = settleConfirmCuts
    if (cutsToConfirm != null) {
        ChatSettleDialog(
            agentName = state.subject.name,
            cuts = cutsToConfirm,
            onDismiss = { settleConfirmCuts = null },
            onConfirm = {
                settleConfirmCuts = null
                callbacks.onSettle()
            },
        )
    }

    if (deleteOpen) {
        ChatDeleteDialog(
            agentName = state.subject.name,
            descendants = descendants,
            workspaceGoes = state.subject.workspaceGoes,
            onPreviewDelete = callbacks.onPreviewDelete,
            onDismiss = { deleteOpen = false },
            onConfirm = { discardingChanges ->
                deleteOpen = false
                callbacks.onDelete(discardingChanges)
            },
        )
    }

    val previewTarget = state.openedAttachment ?: localPreview
    if (previewTarget != null) {
        AttachmentPreviewDialog(
            attachment = previewTarget,
            onOpenExternally = callbacks.onOpenAttachmentExternally,
            onShare = callbacks.onShareAttachment,
            onDismiss = {
                localPreview = null
                if (state.openedAttachment != null) {
                    callbacks.onClearOpenedAttachment()
                }
            },
        )
    }
}

private data class TranscriptScrollObservation(
    val scrolling: Boolean,
    val programmatic: Int,
    val epoch: Int,
    val atBottom: Boolean,
    val firstIndex: Int,
    val firstOffset: Int,
    val followReady: Boolean,
    val anchor: Triple<String?, String?, Int>,
)

/** Fallback screen shown when a chat cannot be opened (e.g. deleted or detached profile). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChatUnavailableScreen(
    title: String,
    reason: String,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val backLabel = stringResource(R.string.action_back)
    AmuxGround(
        modifier = modifier
            .fillMaxSize()
            .testTag(TestTags.SCREEN_CONVERSATION),
    ) {
        Scaffold(
            containerColor = Color.Transparent,
            contentColor = colors.ink,
            topBar = {
                Column {
                    TopAppBar(
                        colors = TopAppBarDefaults.topAppBarColors(
                            containerColor = colors.ground,
                            titleContentColor = colors.ink,
                            navigationIconContentColor = colors.ink,
                        ),
                        navigationIcon = {
                            IconButton(
                                onClick = onBack,
                                modifier = Modifier
                                    .thumbTarget()
                                    .testTag(TestTags.NAV_BACK),
                            ) {
                                Icon(
                                    imageVector = Icons.AutoMirrored.Outlined.ArrowBack,
                                    contentDescription = backLabel,
                                )
                            }
                        },
                        title = {
                            Text(
                                text = title,
                                style = AmuxTheme.typography.identifier,
                                color = colors.ink,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        },
                    )
                    AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))
                }
            },
        ) { innerPadding ->
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(innerPadding)
                    .padding(Space.margin),
            ) {
                AmuxEmptyCard(
                    title = stringResource(R.string.chat_not_opened_title),
                    detail = reason,
                )
            }
        }
    }
}

private fun isNearTranscriptBottom(listState: LazyListState): Boolean {
    val info = listState.layoutInfo
    val total = info.totalItemsCount
    if (total == 0) return true
    val lastVisible = info.visibleItemsInfo.lastOrNull() ?: return true
    if (lastVisible.index < total - 1) return false
    val overflow = (lastVisible.offset + lastVisible.size) - info.viewportEndOffset
    return overflow <= 24
}

@Composable
private fun chatPlaceLine(
    subject: ChatSubject,
    frame: ChatFrame?,
    detached: Boolean,
): String {
    if (detached && subject.reachable) {
        return stringResource(R.string.chat_place_out_of_reach, subject.host)
    }
    val exited = frame?.phase as? PhaseView.Exited
    if (exited != null) {
        return ChatWords.exited(exited.cause, settled = subject.filing == Filing.Closed)
    }
    if (subject.connecting) {
        return PlaceNames.place(host = subject.host.takeIf { it.isNotEmpty() }, directory = subject.directory)
    }
    return when (val reach = subject.reach) {
        is Reach.Online -> PlaceNames.place(
            host = subject.host.takeIf { it.isNotEmpty() },
            directory = subject.directory,
        )
        Reach.Away -> stringResource(R.string.chat_place_away_free, subject.host)
        is Reach.Offline -> when (reach.value) {
            Offline.Revoked -> stringResource(R.string.chat_place_unpaired, subject.host)
            Offline.SignedOut -> stringResource(R.string.chat_place_signed_out, subject.host)
            Offline.Plain -> stringResource(R.string.chat_place_away, subject.host)
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ChatTopBar(
    subject: ChatSubject,
    placeLine: String,
    unconfirmed: Boolean,
    changes: ChangeTotals?,
    hasOverviewHighlights: Boolean,
    settingsAvailable: Boolean,
    onBack: () -> Unit,
    onOpenOverview: () -> Unit,
    onOpenSettings: () -> Unit,
    onOpenReview: () -> Unit,
    onPin: () -> Unit,
    onUnpin: () -> Unit,
    onOpenRename: () -> Unit,
    onCopyAddress: () -> Unit,
    onSettle: () -> Unit,
    onUnsettle: () -> Unit,
    onCancelSettle: () -> Unit,
    onOpenDelete: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val backLabel = stringResource(R.string.action_back)
    val moreLabel = stringResource(R.string.action_more_options)
    val openOverviewDesc = stringResource(R.string.chat_header_open_overview)
    val titleSpoken = stringResource(R.string.chat_title_spoken, subject.name, placeLine, openOverviewDesc)
    var menuExpanded by remember { mutableStateOf(false) }

    Column(modifier = Modifier.testTag(ChatScreenTags.TOP_BAR)) {
        TopAppBar(
            colors = TopAppBarDefaults.topAppBarColors(
                containerColor = colors.ground,
                titleContentColor = colors.ink,
                navigationIconContentColor = colors.ink,
                actionIconContentColor = colors.ink,
            ),
            navigationIcon = {
                IconButton(
                    onClick = onBack,
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(TestTags.NAV_BACK),
                ) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Outlined.ArrowBack,
                        contentDescription = backLabel,
                    )
                }
            },
            title = {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(Space.s))
                        .clickable(role = Role.Button, onClick = onOpenOverview)
                        .semantics { contentDescription = titleSpoken }
                        .padding(vertical = Space.xs)
                        .testTag(ChatScreenTags.TITLE),
                    verticalArrangement = Arrangement.spacedBy(1.dp),
                ) {
                    Text(
                        text = subject.name,
                        style = AmuxTheme.typography.identifier,
                        color = colors.ink,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (placeLine.isNotEmpty()) {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(Space.xs),
                            modifier = Modifier.testTag(ChatScreenTags.HEADER),
                        ) {
                            if (unconfirmed) {
                                AmuxStateMark(
                                    state = RowState.HostAway(subject.host),
                                    size = 8.dp,
                                )
                            }
                            Text(
                                text = placeLine,
                                style = AmuxTheme.typography.caption,
                                color = colors.inkMuted,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                }
            },
            actions = {
                if (changes != null) {
                    val spokenChanges = ChatWords.changes(
                        files = changes.files,
                        added = changes.added,
                        removed = changes.removed,
                    )
                    Box(
                        modifier = Modifier
                            .thumbTarget()
                            .clip(RoundedCornerShape(50))
                            .clickable(role = Role.Button, onClick = onOpenReview)
                            .semantics { contentDescription = spokenChanges }
                            .testTag(ChatScreenTags.CHANGES),
                        contentAlignment = Alignment.Center,
                    ) {
                        Surface(
                            shape = RoundedCornerShape(50),
                            color = colors.sunken,
                            border = BorderStroke(AmuxTheme.metrics.hairline, colors.hairline),
                        ) {
                            Box(
                                modifier = Modifier.padding(horizontal = Space.m, vertical = Space.xs + Space.xxs),
                                contentAlignment = Alignment.Center,
                            ) {
                                DiffDeltaCounts(added = changes.added, removed = changes.removed)
                            }
                        }
                    }
                } else if (hasOverviewHighlights) {
                    IconButton(
                        onClick = onOpenOverview,
                        modifier = Modifier
                            .thumbTarget()
                            .testTag(ChatScreenTags.OVERVIEW_ACTION),
                    ) {
                        Icon(
                            imageVector = Icons.AutoMirrored.Outlined.ListAlt,
                            contentDescription = openOverviewDesc,
                            tint = colors.inkMuted,
                            modifier = Modifier.size(20.dp),
                        )
                    }
                }

                Box {
                    IconButton(
                        onClick = { menuExpanded = true },
                        modifier = Modifier
                            .thumbTarget()
                            .testTag(ChatScreenTags.MORE),
                    ) {
                        Icon(
                            imageVector = Icons.Outlined.MoreVert,
                            contentDescription = moreLabel,
                        )
                    }

                    val closed = subject.filing == Filing.Closed
                    DropdownMenu(
                        expanded = menuExpanded,
                        onDismissRequest = { menuExpanded = false },
                        containerColor = colors.raised,
                    ) {
                        DropdownMenuItem(
                            text = { Text(text = stringResource(R.string.chat_menu_overview), color = colors.ink) },
                            leadingIcon = {
                                Icon(
                                    imageVector = Icons.AutoMirrored.Outlined.ListAlt,
                                    contentDescription = null,
                                    tint = colors.inkMuted,
                                )
                            },
                            onClick = {
                                menuExpanded = false
                                onOpenOverview()
                            },
                            modifier = Modifier.testTag(ChatScreenTags.MENU_OVERVIEW),
                        )
                        if (settingsAvailable) {
                            DropdownMenuItem(
                                text = { Text(text = stringResource(R.string.chat_menu_settings), color = colors.ink) },
                                leadingIcon = {
                                    Icon(
                                        imageVector = Icons.Outlined.Tune,
                                        contentDescription = null,
                                        tint = colors.inkMuted,
                                    )
                                },
                                onClick = {
                                    menuExpanded = false
                                    onOpenSettings()
                                },
                                modifier = Modifier.testTag(ChatScreenTags.MENU_SETTINGS),
                            )
                        }
                        if (changes != null) {
                            DropdownMenuItem(
                                text = { Text(text = stringResource(R.string.chat_menu_review), color = colors.ink) },
                                leadingIcon = {
                                    Icon(
                                        imageVector = Icons.Outlined.RateReview,
                                        contentDescription = null,
                                        tint = colors.inkMuted,
                                    )
                                },
                                onClick = {
                                    menuExpanded = false
                                    onOpenReview()
                                },
                                modifier = Modifier.testTag(ChatScreenTags.MENU_REVIEW),
                            )
                        }
                        if (subject.topLevel && !closed) {
                            if (subject.filing == Filing.Pinned) {
                                DropdownMenuItem(
                                    text = { Text(text = stringResource(R.string.chat_menu_unpin), color = colors.ink) },
                                    leadingIcon = {
                                        Icon(
                                            imageVector = Icons.Outlined.PushPin,
                                            contentDescription = null,
                                            tint = colors.inkMuted,
                                        )
                                    },
                                    onClick = {
                                        menuExpanded = false
                                        onUnpin()
                                    },
                                    modifier = Modifier.testTag(ChatScreenTags.MENU_UNPIN),
                                )
                            } else {
                                DropdownMenuItem(
                                    text = { Text(text = stringResource(R.string.chat_menu_pin), color = colors.ink) },
                                    leadingIcon = {
                                        Icon(
                                            imageVector = Icons.Outlined.PushPin,
                                            contentDescription = null,
                                            tint = colors.inkMuted,
                                        )
                                    },
                                    onClick = {
                                        menuExpanded = false
                                        onPin()
                                    },
                                    modifier = Modifier.testTag(ChatScreenTags.MENU_PIN),
                                )
                            }
                        }
                        DropdownMenuItem(
                            text = { Text(text = stringResource(R.string.chat_menu_rename), color = colors.ink) },
                            leadingIcon = {
                                Icon(
                                    imageVector = Icons.Outlined.Edit,
                                    contentDescription = null,
                                    tint = colors.inkMuted,
                                )
                            },
                            onClick = {
                                menuExpanded = false
                                onOpenRename()
                            },
                            modifier = Modifier.testTag(ChatScreenTags.MENU_RENAME),
                        )
                        if (!subject.native) {
                            DropdownMenuItem(
                                text = { Text(text = stringResource(R.string.chat_menu_copy_address), color = colors.ink) },
                                leadingIcon = {
                                    Icon(
                                        imageVector = Icons.Outlined.ContentCopy,
                                        contentDescription = null,
                                        tint = colors.inkMuted,
                                    )
                                },
                                onClick = {
                                    menuExpanded = false
                                    onCopyAddress()
                                },
                                modifier = Modifier.testTag(ChatScreenTags.MENU_COPY_ADDRESS),
                            )
                        }
                        when {
                            subject.closeHeld -> {
                                DropdownMenuItem(
                                    text = { Text(text = stringResource(R.string.chat_menu_cancel_stop), color = colors.ink) },
                                    leadingIcon = {
                                        Icon(
                                            imageVector = Icons.AutoMirrored.Outlined.Undo,
                                            contentDescription = null,
                                            tint = colors.inkMuted,
                                        )
                                    },
                                    onClick = {
                                        menuExpanded = false
                                        onCancelSettle()
                                    },
                                    modifier = Modifier.testTag(ChatScreenTags.MENU_CANCEL_STOP),
                                )
                            }
                            closed -> {
                                DropdownMenuItem(
                                    text = { Text(text = stringResource(R.string.chat_menu_unsettle), color = colors.ink) },
                                    leadingIcon = {
                                        Icon(
                                            imageVector = Icons.Outlined.Unarchive,
                                            contentDescription = null,
                                            tint = colors.inkMuted,
                                        )
                                    },
                                    onClick = {
                                        menuExpanded = false
                                        onUnsettle()
                                    },
                                    modifier = Modifier.testTag(ChatScreenTags.MENU_UNSETTLE),
                                )
                                if (!subject.native) {
                                    DropdownMenuItem(
                                        text = { Text(text = stringResource(R.string.chat_menu_delete), color = colors.removed) },
                                        leadingIcon = {
                                            Icon(
                                                imageVector = Icons.Outlined.DeleteOutline,
                                                contentDescription = null,
                                                tint = colors.removed,
                                            )
                                        },
                                        onClick = {
                                            menuExpanded = false
                                            onOpenDelete()
                                        },
                                        modifier = Modifier.testTag(ChatScreenTags.MENU_DELETE),
                                    )
                                }
                            }
                            else -> {
                                DropdownMenuItem(
                                    text = { Text(text = stringResource(R.string.chat_menu_settle), color = colors.ink) },
                                    leadingIcon = {
                                        Icon(
                                            imageVector = Icons.Outlined.CheckCircle,
                                            contentDescription = null,
                                            tint = colors.inkMuted,
                                        )
                                    },
                                    onClick = {
                                        menuExpanded = false
                                        onSettle()
                                    },
                                    modifier = Modifier.testTag(ChatScreenTags.MENU_SETTLE),
                                )
                            }
                        }
                    }
                }
            },
        )
        AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))
    }
}

@Composable
private fun ChatFamilyBar(
    family: FamilyHeader,
    onOpenAgent: (AgentKey) -> Unit,
) {
    val parent = family.parent
    val children = family.children
    if (parent == null && children.isEmpty()) return

    val colors = AmuxTheme.colors
    var childrenExpanded by remember { mutableStateOf(false) }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(colors.ground)
            .padding(horizontal = Space.margin, vertical = Space.xs),
        verticalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        if (parent != null) {
            val parentNeedsYou = parent.familyAttention == Attention.NeedsYou ||
                parent.attention == Attention.NeedsYou
            Row(modifier = Modifier.fillMaxWidth()) {
                Surface(
                    modifier = Modifier
                        .thumbTarget()
                        .clip(RoundedCornerShape(50))
                        .clickable(role = Role.Button) { onOpenAgent(parent.agent) }
                        .testTag(ChatScreenTags.FAMILY_PARENT),
                    shape = RoundedCornerShape(50),
                    color = colors.raised,
                    border = BorderStroke(AmuxTheme.metrics.hairline, colors.hairline),
                ) {
                    Row(
                        modifier = Modifier.padding(horizontal = Space.m, vertical = Space.xs + Space.xxs),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(Space.xs + Space.xxs),
                    ) {
                        if (parentNeedsYou) {
                            AmuxAttentionDot(size = 7.dp)
                        } else {
                            Icon(
                                imageVector = Icons.Outlined.NorthWest,
                                contentDescription = null,
                                tint = colors.inkFaint,
                                modifier = Modifier.size(13.dp),
                            )
                        }
                        Text(
                            text = stringResource(R.string.chat_family_from, parent.name),
                            style = AmuxTheme.typography.caption,
                            color = colors.ink,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
            }
        }

        if (children.isNotEmpty()) {
            val needYouCount = maxOf(
                family.needYou,
                children.count {
                    it.attention == Attention.NeedsYou || it.familyAttention == Attention.NeedsYou
                },
            )
            val countText = if (children.size == 1) {
                stringResource(R.string.chat_family_subagent_one)
            } else {
                stringResource(R.string.chat_family_subagent_many, children.size)
            }
            val attentionText = when {
                needYouCount == 1 -> stringResource(R.string.chat_family_needs_you_one)
                needYouCount > 1 -> stringResource(R.string.chat_family_needs_you, needYouCount)
                else -> null
            }

            AmuxPanel(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.fillMaxWidth()) {
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .thumbTarget()
                            .clickable(role = Role.Button) { childrenExpanded = !childrenExpanded }
                            .padding(horizontal = Space.m, vertical = Space.s)
                            .testTag(ChatScreenTags.FAMILY_CHILDREN),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(Space.s),
                    ) {
                        if (needYouCount > 0) {
                            AmuxAttentionDot(size = 7.dp)
                        } else {
                            Icon(
                                imageVector = Icons.Outlined.AccountTree,
                                contentDescription = null,
                                tint = colors.inkMuted,
                                modifier = Modifier.size(15.dp),
                            )
                        }
                        Text(
                            text = countText,
                            style = AmuxTheme.typography.caption,
                            color = colors.ink,
                        )
                        if (attentionText != null) {
                            Text(
                                text = attentionText,
                                style = AmuxTheme.typography.caption,
                                color = colors.accent,
                            )
                        }
                        Spacer(modifier = Modifier.weight(1f))
                        Icon(
                            imageVector = if (childrenExpanded) Icons.Outlined.ExpandLess else Icons.Outlined.ExpandMore,
                            contentDescription = null,
                            tint = colors.inkFaint,
                            modifier = Modifier.size(16.dp),
                        )
                    }
                    if (childrenExpanded) {
                        children.forEach { child ->
                            AmuxHairline()
                            FamilyChildRow(
                                child = child,
                                onOpen = { onOpenAgent(child.agent) },
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun FamilyChildRow(
    child: FleetCard,
    onOpen: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val needsYou = child.attention == Attention.NeedsYou || child.familyAttention == Attention.NeedsYou
    val stateText = remember(child) { ChatWords.childState(child) }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .thumbTarget()
            .clickable(role = Role.Button, onClick = onOpen)
            .padding(horizontal = Space.m, vertical = Space.s)
            .testTag(ChatScreenTags.familyChild(child.name)),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        if (needsYou) {
            AmuxAttentionDot(size = 7.dp)
        } else {
            Spacer(modifier = Modifier.width(7.dp))
        }
        Text(
            text = child.name,
            style = AmuxTheme.typography.detail,
            color = colors.ink,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        Text(
            text = stateText,
            style = AmuxTheme.typography.caption,
            color = if (needsYou) colors.accent else colors.inkMuted,
            maxLines = 1,
        )
    }
}

@Composable
private fun HeldStopBanner(
    host: String,
    onCancelStop: () -> Unit,
) {
    val colors = AmuxTheme.colors
    AmuxPanel(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Space.margin, vertical = Space.xs)
            .testTag(ChatScreenTags.HELD_STOP),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.m, vertical = Space.s),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            Icon(
                imageVector = Icons.Outlined.Schedule,
                contentDescription = null,
                tint = colors.inkMuted,
                modifier = Modifier.size(16.dp),
            )
            Text(
                text = stringResource(R.string.chat_settle_held, host),
                style = AmuxTheme.typography.detail,
                color = colors.ink,
                modifier = Modifier.weight(1f),
            )
            AmuxActionButton(
                label = stringResource(R.string.chat_settle_held_cancel),
                kind = ActionButtonKind.Outline,
                onClick = onCancelStop,
                modifier = Modifier.testTag(ChatScreenTags.HELD_STOP_CANCEL),
            )
        }
    }
}

@Composable
private fun EndedChatBanner(message: String) {
    val colors = AmuxTheme.colors
    AmuxPanel(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Space.margin, vertical = Space.xs)
            .testTag(ChatScreenTags.ENDED_BANNER),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.m, vertical = Space.s),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            Icon(
                imageVector = Icons.Outlined.Info,
                contentDescription = null,
                tint = colors.inkMuted,
                modifier = Modifier.size(16.dp),
            )
            Text(
                text = message,
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
                modifier = Modifier.weight(1f),
            )
        }
    }
}

@Composable
private fun ChatTranscriptList(
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
                    imageBytes = rowSource::imageBytes,
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
        imageBytes = { blob ->
            // Read attachmentRevision so cell recomposes when blob bytes land.
            @Suppress("UNUSED_VARIABLE")
            val rev = cell.attachmentRevision
            rowSource.imageBytes(blob)
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
                modifier = Modifier
                    .thumbTarget()
                    .testTag(ChatScreenTags.PAGING_RETRY),
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
private fun NewActivityPill(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val label = stringResource(R.string.chat_new_activity)
    AmuxChrome(
        modifier = modifier
            .thumbTarget()
            .clip(RoundedCornerShape(50))
            .clickable(role = Role.Button, onClick = onClick)
            .testTag(ChatScreenTags.NEW_ACTIVITY),
        shape = RoundedCornerShape(50),
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
                modifier = Modifier.size(16.dp),
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
private fun JumpToNewestButton(
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
                modifier = Modifier.size(20.dp),
            )
        }
    }
}

@Composable
private fun ChatBottomDock(
    state: ChatScreenState,
    callbacks: ChatScreenCallbacks,
    onOpenOverview: () -> Unit,
    onOpenSettings: () -> Unit,
) {
    val activeAsk = state.ask
    if (activeAsk != null && activeAsk.state !is CardState.Dismissed) {
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.m, vertical = Space.s)
                .heightIn(max = 520.dp),
        ) {
            AskCardView(
                card = activeAsk,
                onAction = callbacks.onAskAction,
                keptQuestionDraft = state.keptQuestionDraft,
                onQuestionDraftChange = { draft -> callbacks.onQuestionDraftChange(activeAsk.key, draft) },
                keptFormValues = state.keptFormValues,
                onFormValuesChange = { values -> callbacks.onFormValuesChange(activeAsk.key, values) },
                validateForm = callbacks.validateForm,
                onInterrupt = callbacks.onInterrupt,
                onCopyCode = callbacks.onCopyCode,
            )
        }
        return
    }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        if (activeAsk != null) {
            Box(modifier = Modifier.padding(horizontal = Space.m)) {
                AskCardView(
                    card = activeAsk,
                    onAction = callbacks.onAskAction,
                    keptQuestionDraft = state.keptQuestionDraft,
                    onQuestionDraftChange = { draft -> callbacks.onQuestionDraftChange(activeAsk.key, draft) },
                    keptFormValues = state.keptFormValues,
                    onFormValuesChange = { values -> callbacks.onFormValuesChange(activeAsk.key, values) },
                    validateForm = callbacks.validateForm,
                    onInterrupt = callbacks.onInterrupt,
                    onCopyCode = callbacks.onCopyCode,
                )
            }
        }

        val nativeMode = state.frame?.composer?.mode is Composer.Native
        if (!nativeMode) {
            ChatOverviewStrip(
                overview = state.overview,
                context = state.frame?.context,
                changeTotals = state.changes,
                nowMs = state.nowMs,
                onOpenOverview = onOpenOverview,
                onOpenReview = if (state.changes != null) {
                    { callbacks.onOpenReview(null) }
                } else {
                    null
                },
                modifier = Modifier.padding(horizontal = Space.m),
            )
        }

        val usageLimitMessage = remember(state.overview?.usageNearLimit, state.nowMs) {
            val usage = state.overview?.usageNearLimit
            if (usage != null && usage.blocked) {
                ChatWords.limitReached(usage, now = Instant.ofEpochMilli(state.nowMs))
            } else {
                null
            }
        }

        val composerView = state.frame?.composer ?: ComposerView(
            mode = Composer.Send,
            switchedAway = false,
            activity = null,
        )

        ChatComposer(
            state = ChatComposerState(
                composer = composerView,
                draft = state.draft,
                host = state.subject.host,
                reach = state.subject.reach,
                connecting = state.subject.connecting,
                phase = state.frame?.phase ?: PhaseView.Idle,
                kind = state.frame?.kind,
                settled = state.subject.filing == Filing.Closed,
                canSend = state.canSend,
                canResume = state.canResume,
                activitySubject = state.activitySubject,
                liveSubagents = state.liveSubagents,
                signIn = state.frame?.signIn,
                queued = state.frame?.queue.orEmpty(),
                underway = state.landingInQueue,
                refused = state.frame?.refused.orEmpty(),
                slashMatches = state.slashMatches,
                attachments = state.attachments,
                uploadingCount = state.uploadingCount,
                notice = state.notice,
                controls = state.frame?.controls,
                context = state.frame?.context,
                settingsAvailable = state.settings != null,
                showOverviewChip = state.hasOverviewHighlights,
                usageLimitMessage = usageLimitMessage,
                dictationEnabled = state.dictationEnabled,
                dictationActive = state.dictationActive,
                dictationSentence = state.dictationSentence,
                nowMs = state.nowMs,
            ),
            callbacks = ChatComposerCallbacks(
                onDraftChange = callbacks.onDraftChange,
                onSend = callbacks.onSend,
                onResume = callbacks.onResume,
                onInterrupt = callbacks.onInterrupt,
                onAttach = callbacks.onAttach,
                onRemoveAttachment = callbacks.onRemoveAttachment,
                onPickCommand = callbacks.onPickCommand,
                onSendNow = callbacks.onSendNow,
                onWithdraw = callbacks.onWithdraw,
                onResend = callbacks.onResend,
                onDiscard = callbacks.onDiscard,
                onDismissRefused = callbacks.onDismissRefused,
                onSwitchBack = callbacks.onSwitchBack,
                onStopNative = callbacks.onStopJob,
                onOpenSettings = onOpenSettings,
                onOpenOverview = onOpenOverview,
                onToggleDictation = callbacks.onToggleDictation,
            ),
        )
    }
}

@Composable
private fun ChatRenameDialog(
    currentName: String,
    validateName: (String) -> AgentNameProblem?,
    onDismiss: () -> Unit,
    onConfirm: (String) -> Unit,
) {
    val colors = AmuxTheme.colors
    var draft by remember(currentName) { mutableStateOf(currentName) }
    val trimmed = draft.trim()
    val problem = remember(trimmed) { validateName(trimmed) }

    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = colors.ground,
        titleContentColor = colors.ink,
        textContentColor = colors.inkMuted,
        modifier = Modifier.testTag(ChatScreenTags.RENAME_DIALOG),
        title = {
            Text(
                text = stringResource(R.string.chat_rename_title, currentName),
                style = AmuxTheme.typography.identifier,
            )
        },
        text = {
            val inputShape = RoundedCornerShape(AmuxTheme.metrics.controlRadius)
            val photographed = LocalAmuxPhotographed.current
            Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                Box(
                    modifier = Modifier
                        .width(260.dp)
                        .clip(inputShape)
                        .background(colors.raised)
                        .border(AmuxTheme.metrics.hairline, colors.hairline, inputShape)
                        .padding(horizontal = Space.m, vertical = Space.m),
                ) {
                    if (draft.isEmpty()) {
                        Text(
                            text = stringResource(R.string.chat_rename_placeholder),
                            style = AmuxTheme.typography.mono,
                            color = colors.inkFaint,
                        )
                    }
                    BasicTextField(
                        value = draft,
                        onValueChange = { draft = it },
                        singleLine = true,
                        textStyle = AmuxTheme.typography.mono.copy(color = colors.ink),
                        cursorBrush = SolidColor(if (photographed) Color.Unspecified else colors.ink),
                        modifier = Modifier
                            .width(236.dp)
                            .testTag(ChatScreenTags.RENAME_INPUT),
                    )
                }
                if (problem != null && trimmed.isNotEmpty()) {
                    Text(
                        text = ChatWords.nameProblem(problem),
                        style = AmuxTheme.typography.detail,
                        color = colors.accent,
                        modifier = Modifier.testTag(ChatScreenTags.RENAME_PROBLEM),
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = { if (problem == null) onConfirm(trimmed) },
                enabled = problem == null,
                modifier = Modifier
                    .thumbTarget()
                    .testTag(ChatScreenTags.RENAME_CONFIRM),
            ) {
                Text(
                    text = stringResource(R.string.chat_menu_rename),
                    style = AmuxTheme.typography.bodyEmphasis,
                    color = if (problem == null) colors.ink else colors.inkFaint,
                )
            }
        },
        dismissButton = {
            TextButton(
                onClick = onDismiss,
                modifier = Modifier.thumbTarget(),
            ) {
                Text(
                    text = stringResource(R.string.action_cancel),
                    style = AmuxTheme.typography.body,
                    color = colors.inkMuted,
                )
            }
        },
    )
}

@Composable
private fun ChatSettleDialog(
    agentName: String,
    cuts: String,
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
) {
    val colors = AmuxTheme.colors
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = colors.ground,
        titleContentColor = colors.ink,
        textContentColor = colors.inkMuted,
        modifier = Modifier.testTag(ChatScreenTags.SETTLE_DIALOG),
        title = {
            Text(
                text = stringResource(R.string.chat_settle_title, agentName),
                style = AmuxTheme.typography.identifier,
            )
        },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                Text(
                    text = cuts,
                    style = AmuxTheme.typography.detail,
                    color = colors.ink,
                )
                Text(
                    text = stringResource(R.string.chat_settle_stops_now),
                    style = AmuxTheme.typography.detail,
                    color = colors.removed,
                )
                Text(
                    text = stringResource(R.string.chat_settle_keeps_chat),
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = onConfirm,
                modifier = Modifier
                    .thumbTarget()
                    .testTag(ChatScreenTags.SETTLE_CONFIRM),
            ) {
                Text(
                    text = stringResource(R.string.chat_menu_settle),
                    style = AmuxTheme.typography.bodyEmphasis,
                    color = colors.removed,
                )
            }
        },
        dismissButton = {
            TextButton(
                onClick = onDismiss,
                modifier = Modifier.thumbTarget(),
            ) {
                Text(
                    text = stringResource(R.string.action_cancel),
                    style = AmuxTheme.typography.body,
                    color = colors.inkMuted,
                )
            }
        },
    )
}

@Composable
private fun ChatDeleteDialog(
    agentName: String,
    descendants: Int,
    workspaceGoes: WorkspaceGoes?,
    onPreviewDelete: suspend () -> Result<DeleteWarning?>,
    onDismiss: () -> Unit,
    onConfirm: (discardingChanges: Boolean) -> Unit,
) {
    val colors = AmuxTheme.colors
    var warning by remember(workspaceGoes) { mutableStateOf(DeleteWords.asking(workspaceGoes)) }

    LaunchedEffect(workspaceGoes) {
        if (DeleteWords.awaitsAnswer(warning)) {
            val answer = onPreviewDelete()
            warning = DeleteWords.answered(warning, answer)
        }
    }

    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = colors.ground,
        titleContentColor = colors.ink,
        textContentColor = colors.inkMuted,
        modifier = Modifier.testTag(ChatScreenTags.DELETE_DIALOG),
        title = {
            Text(
                text = stringResource(R.string.chat_delete_title, agentName),
                style = AmuxTheme.typography.identifier,
            )
        },
        text = {
            Text(
                text = DeleteWords.message(descendants = descendants, warning = warning),
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
            )
        },
        confirmButton = {
            TextButton(
                onClick = { onConfirm(DeleteWords.discards(warning)) },
                modifier = Modifier
                    .thumbTarget()
                    .testTag(ChatScreenTags.DELETE_CONFIRM),
            ) {
                Text(
                    text = stringResource(R.string.home_action_delete),
                    style = AmuxTheme.typography.bodyEmphasis,
                    color = colors.removed,
                )
            }
        },
        dismissButton = {
            TextButton(
                onClick = onDismiss,
                modifier = Modifier.thumbTarget(),
            ) {
                Text(
                    text = stringResource(R.string.action_cancel),
                    style = AmuxTheme.typography.body,
                    color = colors.inkMuted,
                )
            }
        },
    )
}

/**
 * Pure Compose dialog panel for renaming an agent from its chat. Exposed for
 * headless storybook rendering over a conversation frame.
 */
@Composable
fun ChatRenameDialogContent(
    currentName: String,
    initialDraft: String = currentName,
    validateName: (String) -> AgentNameProblem? = { null },
    onDismiss: () -> Unit,
    onConfirm: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    var draft by remember(currentName, initialDraft) { mutableStateOf(initialDraft) }
    val trimmed = draft.trim()
    val problem = remember(trimmed) { validateName(trimmed) }
    val inputShape = RoundedCornerShape(AmuxTheme.metrics.controlRadius)
    val photographed = LocalAmuxPhotographed.current

    AmuxPanel(
        shape = RoundedCornerShape(20.dp),
        color = colors.ground,
        modifier = modifier
            .fillMaxWidth()
            .testTag(ChatScreenTags.RENAME_DIALOG),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(Space.l),
            verticalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            Text(
                text = stringResource(R.string.chat_rename_title, currentName),
                style = AmuxTheme.typography.identifier,
                color = colors.ink,
            )
            Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(inputShape)
                        .background(colors.raised)
                        .border(AmuxTheme.metrics.hairline, colors.hairline, inputShape)
                        .padding(horizontal = Space.m, vertical = Space.m),
                ) {
                    if (draft.isEmpty()) {
                        Text(
                            text = stringResource(R.string.chat_rename_placeholder),
                            style = AmuxTheme.typography.mono,
                            color = colors.inkFaint,
                        )
                    }
                    BasicTextField(
                        value = draft,
                        onValueChange = { draft = it },
                        singleLine = true,
                        textStyle = AmuxTheme.typography.mono.copy(color = colors.ink),
                        cursorBrush = SolidColor(if (photographed) Color.Unspecified else colors.ink),
                        modifier = Modifier
                            .fillMaxWidth()
                            .testTag(ChatScreenTags.RENAME_INPUT),
                    )
                }
                if (problem != null && trimmed.isNotEmpty()) {
                    Text(
                        text = ChatWords.nameProblem(problem),
                        style = AmuxTheme.typography.detail,
                        color = colors.accent,
                        modifier = Modifier.testTag(ChatScreenTags.RENAME_PROBLEM),
                    )
                }
            }
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(Space.s, Alignment.End),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TextButton(
                    onClick = onDismiss,
                    modifier = Modifier.thumbTarget(),
                ) {
                    Text(
                        text = stringResource(R.string.action_cancel),
                        style = AmuxTheme.typography.body,
                        color = colors.inkMuted,
                    )
                }
                TextButton(
                    onClick = { if (problem == null) onConfirm(trimmed) },
                    enabled = problem == null,
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(ChatScreenTags.RENAME_CONFIRM),
                ) {
                    Text(
                        text = stringResource(R.string.chat_menu_rename),
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = if (problem == null) colors.ink else colors.inkFaint,
                    )
                }
            }
        }
    }
}

/**
 * Pure Compose dialog panel for confirming agent deletion from its chat.
 * Exposed for headless storybook rendering over a conversation frame.
 */
@Composable
fun ChatDeleteDialogContent(
    agentName: String,
    descendants: Int = 0,
    warning: DeleteWarning? = null,
    onDismiss: () -> Unit,
    onConfirm: (discardingChanges: Boolean) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    AmuxPanel(
        shape = RoundedCornerShape(20.dp),
        color = colors.ground,
        modifier = modifier
            .fillMaxWidth()
            .testTag(ChatScreenTags.DELETE_DIALOG),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(Space.l),
            verticalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            Text(
                text = stringResource(R.string.chat_delete_title, agentName),
                style = AmuxTheme.typography.identifier,
                color = colors.ink,
            )
            Text(
                text = DeleteWords.message(descendants = descendants, warning = warning),
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
            )
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(Space.s, Alignment.End),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TextButton(
                    onClick = onDismiss,
                    modifier = Modifier.thumbTarget(),
                ) {
                    Text(
                        text = stringResource(R.string.action_cancel),
                        style = AmuxTheme.typography.body,
                        color = colors.inkMuted,
                    )
                }
                TextButton(
                    onClick = { onConfirm(DeleteWords.discards(warning)) },
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(ChatScreenTags.DELETE_CONFIRM),
                ) {
                    Text(
                        text = stringResource(R.string.home_action_delete),
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.removed,
                    )
                }
            }
        }
    }
}

@Composable
private fun AttachmentPreviewDialog(
    attachment: ChatOpenedAttachmentUi,
    onOpenExternally: (File) -> Unit,
    onShare: (File) -> Unit,
    onDismiss: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val closeLabel = stringResource(R.string.action_close)
    val bitmap = remember(attachment.bytes) {
        attachment.bytes?.let { data ->
            runCatching { BitmapFactory.decodeByteArray(data, 0, data.size)?.asImageBitmap() }.getOrNull()
        }
    }
    val localFile = attachment.file

    Dialog(
        onDismissRequest = onDismiss,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .background(colors.ground)
                .navigationBarsPadding()
                .testTag(ChatScreenTags.PREVIEW_DIALOG),
        ) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.m, vertical = Space.s),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                Text(
                    text = attachment.name,
                    style = AmuxTheme.typography.bodyEmphasis,
                    color = colors.ink,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                IconButton(
                    onClick = onDismiss,
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(ChatScreenTags.PREVIEW_CLOSE),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.Close,
                        contentDescription = closeLabel,
                        tint = colors.ink,
                    )
                }
            }
            AmuxHairline()
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .padding(Space.l),
                contentAlignment = Alignment.Center,
            ) {
                if (bitmap != null) {
                    Image(
                        bitmap = bitmap,
                        contentDescription = attachment.name,
                        contentScale = ContentScale.Fit,
                        modifier = Modifier.fillMaxSize(),
                    )
                } else {
                    Column(
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(Space.m),
                    ) {
                        Icon(
                            imageVector = Icons.Outlined.Description,
                            contentDescription = null,
                            tint = colors.inkMuted,
                            modifier = Modifier.size(40.dp),
                        )
                        Text(
                            text = attachment.name,
                            style = AmuxTheme.typography.mono,
                            color = colors.ink,
                        )
                        Text(
                            text = stringResource(R.string.chat_preview_unavailable),
                            style = AmuxTheme.typography.detail,
                            color = colors.inkMuted,
                            textAlign = TextAlign.Center,
                        )
                        if (localFile != null) {
                            Row(
                                horizontalArrangement = Arrangement.spacedBy(Space.s),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                AmuxActionButton(
                                    label = stringResource(R.string.chat_preview_open),
                                    kind = ActionButtonKind.Primary,
                                    onClick = { onOpenExternally(localFile) },
                                    modifier = Modifier.testTag(ChatScreenTags.PREVIEW_OPEN),
                                )
                                AmuxActionButton(
                                    label = stringResource(R.string.chat_preview_share),
                                    kind = ActionButtonKind.Outline,
                                    onClick = { onShare(localFile) },
                                    modifier = Modifier.testTag(ChatScreenTags.PREVIEW_SHARE),
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}
