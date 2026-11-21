package sh.amux.features

import androidx.compose.foundation.Image
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.imeNestedScroll
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import java.io.File
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.flowOf
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.AgentKey
import sh.amux.values.AgentNameProblem
import sh.amux.values.AskCard
import sh.amux.values.AttachmentView
import sh.amux.values.BlobRef
import sh.amux.values.ChangeTotals
import sh.amux.values.ChatFrame
import sh.amux.values.CommandMatch
import sh.amux.values.Comparison
import sh.amux.values.Composer
import sh.amux.values.DeleteWarning
import sh.amux.values.FamilyHeader
import sh.amux.values.Filing
import sh.amux.values.FormField
import sh.amux.values.FormProblem
import sh.amux.values.FormValue
import sh.amux.values.GroupState
import sh.amux.values.OpenedAgent
import sh.amux.values.Overview
import sh.amux.values.QueuedRow
import sh.amux.values.Reach
import sh.amux.values.RouteKind
import sh.amux.values.SentPrompt
import sh.amux.values.SettingChange
import sh.amux.values.SettingsView
import sh.amux.values.Waiting
import sh.amux.values.WorkspaceGoes
import sh.amux.words.ChatWords
import sh.amux.words.EMPTY_GROUP_STATE
import sh.amux.words.OpenableLink

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
    /** How many of the composer's edits the model had taken when it wrote [draft]. */
    val draftEdits: Int = 0,
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
    clock: ElapsedClock = ElapsedClock.Still,
) {
    val chatIdentity: Any = state.subject.key
        ?: state.frame?.agent
        ?: state.subject.address.ifEmpty { state.subject.name }

    key(chatIdentity) {
        ChatScreenContent(
            state = state,
            rowSource = rowSource,
            callbacks = callbacks,
            clock = clock,
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
    clock: ElapsedClock,
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

    val lastKey = state.keys.lastOrNull()
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

    suspend fun pinTranscriptToBottom(lastIndex: Int) {
        if (lastIndex < 0) return
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

    // Follows the tail row's identity and its streaming revision, so
    // cap-scrolling (same key count, new tail key) and a reply growing as it
    // streams keep the newest row in view. The revision is read here, in the
    // effect, rather than in composition, so a streamed token re-pins the
    // list without recomposing the screen.
    LaunchedEffect(
        state.following,
        state.toNewest,
        lastKey,
        lastLandingId,
        totalTailAnchor,
    ) {
        if (state.following && totalTailAnchor > 0) {
            val tail = lastKey?.let(rowSource::revisions) ?: flowOf(0)
            tail.distinctUntilChanged().collect {
                val totalItems = listState.layoutInfo.totalItemsCount
                val lastIndex = maxOf(totalItems, totalTailAnchor) - 1
                pinTranscriptToBottom(lastIndex)
                initialFollowDone = true
            }
        } else {
            initialFollowDone = true
        }
    }

    // Re-pin to the newest row when following and the feed viewport or laid-out item
    // count changes (e.g., IME opens/closes, footer banner appears/disappears, or
    // LazyColumn lays out newly appended rows after the first composition pass).
    LaunchedEffect(listState, state.following) {
        if (!state.following) return@LaunchedEffect
        snapshotFlow {
            val info = listState.layoutInfo
            info.viewportEndOffset to info.totalItemsCount
        }
            .distinctUntilChanged()
            .collect { (viewportEnd, totalItems) ->
                if (
                    viewportEnd > 0 &&
                    totalItems > 0 &&
                    !listState.isScrollInProgress &&
                    !isNearTranscriptBottom(listState)
                ) {
                    pinTranscriptToBottom(totalItems - 1)
                }
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
                    clock = clock,
                    onOpenOverview = { overviewOpen = true },
                    onOpenSettings = { settingsOpen = true },
                )
            }
        }
    }

    // Modal sheets & dialogs owned by ChatScreen
    if (overviewOpen) {
        ChatOverviewModalSheet(
            clock = clock,
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
        RenameAgentDialog(
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
        SettleAgentDialog(
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
        DeleteAgentDialog(
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
                                modifier = Modifier.semantics { heading() },
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

