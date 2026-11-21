package sh.amux.core.runtime

import java.io.File
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import sh.amux.values.ActOutcome
import sh.amux.values.AskCard
import sh.amux.values.BlobRef
import sh.amux.values.CardState
import sh.amux.values.ChangeTotals
import sh.amux.values.ChatAgents
import sh.amux.values.ChatDelta
import sh.amux.values.ChatFrame
import sh.amux.values.ClientCommand
import sh.amux.values.CommandMatch
import sh.amux.values.CommandOutcome
import sh.amux.values.CommandView
import sh.amux.values.Comparison
import sh.amux.values.Composer
import sh.amux.values.Draft
import sh.amux.values.DraftAttachment
import sh.amux.values.FormField
import sh.amux.values.FormProblem
import sh.amux.values.FormValue
import sh.amux.values.FrozenReview
import sh.amux.values.GroupState
import sh.amux.values.KeysMoved
import sh.amux.values.Kind
import sh.amux.values.Lands
import sh.amux.values.OpenedAgent
import sh.amux.values.Overview
import sh.amux.values.PageOutcome
import sh.amux.values.QuestionResponse
import sh.amux.values.QueuedRow
import sh.amux.values.Refusal
import sh.amux.values.RefusalReason
import sh.amux.values.RefusedPrompt
import sh.amux.values.ReviewComment
import sh.amux.values.ReviewDoc
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Run
import sh.amux.values.RunCounts
import sh.amux.values.SentPrompt
import sh.amux.values.SettingChange
import sh.amux.values.SettingsView
import sh.amux.values.SubagentGroup
import sh.amux.values.TurnPart
import sh.amux.values.TurnPlace
import sh.amux.words.EMPTY_GROUP_STATE

/**
 * Stateless view-helper bridge used by [ChatModel] and [ReviewModel].
 *
 * Defaults to [NativeChatBridge] (the real Rust library via JNI); tests or
 * host previews without JNI loaded may pass a lightweight stand-in.
 */
interface ChatBridge {
    val attachmentLimit: Long
    fun commandMatches(commands: List<CommandView>, draft: String): List<CommandMatch>
    fun pastesAsAttachment(text: String): Boolean
    fun formProblems(fields: List<FormField>, values: List<FormValue>): List<FormProblem>?
    fun reviewDoc(review: FrozenReview, comments: List<ReviewComment>): ReviewDoc?
}

/** Production [ChatBridge] backed by [Bridge] / Rust JNI. */
object NativeChatBridge : ChatBridge {
    override val attachmentLimit: Long
        get() = Bridge.attachmentLimit

    override fun commandMatches(commands: List<CommandView>, draft: String): List<CommandMatch> =
        Bridge.commandMatches(commands, draft)

    override fun pastesAsAttachment(text: String): Boolean = Bridge.pastesAsAttachment(text)

    override fun formProblems(fields: List<FormField>, values: List<FormValue>): List<FormProblem>? =
        Bridge.formProblems(fields, values)

    override fun reviewDoc(review: FrozenReview, comments: List<ReviewComment>): ReviewDoc? =
        Bridge.reviewDoc(review, comments)
}

/** What a client command asked the chat screen to open. */
sealed interface ChatAsk {
    data class Settings(val command: ClientCommand? = null) : ChatAsk
    data object Rename : ChatAsk
}

/** Status of requesting older rows at the top of the transcript. */
sealed interface ChatPaging {
    data object Idle : ChatPaging
    data object Fetching : ChatPaging

    /** Older history is held only by the agent's host, which cannot be reached. */
    data object Unreachable : ChatPaging

    data class Failed(val reason: String) : ChatPaging

    val message: String?
        get() = when (this) {
            Idle, Fetching -> null
            Unreachable -> "Older history is on its host, which is away."
            is Failed -> reason
        }
}

/** Observable state of one row cell, keyed by row id. */
data class RowCellState(
    val id: String,
    val row: Row? = null,
    val revision: Int = 0,
    val attachmentRevision: Int = 0,
    val openedAgent: OpenedAgent? = null,
    val groupState: GroupState? = null,
)

/**
 * One row of the transcript, held by its key.
 *
 * Collecting [state] (or [rowFlow]) observes only this row and any fleet
 * subagent/group standing stamped on it, so updating one row recomposes only
 * that cell.
 */
class RowCell internal constructor(
    val id: String,
    initialRow: Row?,
    initialRevision: Int,
    initialOpenedAgent: OpenedAgent? = null,
    initialGroupState: GroupState? = null,
) {
    private val _row = MutableStateFlow(initialRow)
    val rowFlow: StateFlow<Row?> = _row.asStateFlow()

    private val _state = MutableStateFlow(
        RowCellState(
            id = id,
            row = initialRow,
            revision = initialRevision,
            attachmentRevision = 0,
            openedAgent = initialOpenedAgent,
            groupState = initialGroupState,
        ),
    )
    val state: StateFlow<RowCellState> = _state.asStateFlow()

    val row: Row? get() = _row.value
    val revision: Int get() = _state.value.revision
    val attachmentRevision: Int get() = _state.value.attachmentRevision
    val openedAgent: OpenedAgent? get() = _state.value.openedAgent
    val groupState: GroupState? get() = _state.value.groupState

    internal fun updateRow(
        row: Row,
        revision: Int,
        openedAgent: OpenedAgent?,
        groupState: GroupState?,
    ) {
        _row.value = row
        _state.value = _state.value.copy(
            row = row,
            revision = revision,
            openedAgent = openedAgent,
            groupState = groupState,
        )
    }

    internal fun bumpRevision(revision: Int) {
        _state.value = _state.value.copy(revision = revision)
    }

    internal fun bumpAttachmentRevision() {
        _state.value = _state.value.copy(attachmentRevision = _state.value.attachmentRevision + 1)
    }

    internal fun updateStanding(
        openedAgent: OpenedAgent?,
        groupState: GroupState?,
        revision: Int,
    ) {
        _state.value = _state.value.copy(
            openedAgent = openedAgent,
            groupState = groupState,
            revision = revision,
        )
    }
}

/** Observable state of an open chat screen. */
data class ChatUiState(
    val keys: List<String> = emptyList(),
    val sequence: Int = 0,
    val revision: Int = 0,
    val empty: Boolean = true,
    val heldCount: Int = 0,
    val frame: ChatFrame? = null,
    val ask: AskCard? = null,
    val overview: Overview? = null,
    val settings: SettingsView? = null,
    val openedAgents: Map<List<Int>, OpenedAgent> = emptyMap(),
    val liveSubagents: GroupState = EMPTY_GROUP_STATE,
    val groupStates: Map<String, GroupState> = emptyMap(),
    val expanded: Set<String> = emptySet(),
    val paging: ChatPaging = ChatPaging.Idle,
    val loadingHint: Boolean = false,
    val following: Boolean = true,
    val toNewest: Int = 0,
    val draft: String = "",
    val attachments: List<DraftAttachment> = emptyList(),
    val uploading: Int = 0,
    val sending: Boolean = false,
    val notice: String? = null,
    val comparison: Comparison = Comparison.Uncommitted,
    val overviewChangesComparison: Comparison? = null,
    val overviewChangesLoading: Boolean = false,
    val overviewChangesError: String? = null,
    val asks: ChatAsk? = null,
    val openedAttachment: File? = null,
    val dictation: DictationState = DictationState(),
    val slashMatches: List<CommandMatch> = emptyList(),
) {
    val arrivalsHeld: Boolean
        get() = frame?.arrivalsHeld == true

    val newActivity: Boolean
        get() = !following && arrivalsHeld

    val hasDraft: Boolean
        get() = draft.isNotBlank() || attachments.isNotEmpty()

    val canSend: Boolean
        get() = frame != null &&
            !sending &&
            uploading == 0 &&
            hasDraft &&
            frame.composer.mode == Composer.Send &&
            frame.caughtUp

    val canResume: Boolean
        get() = frame != null &&
            !sending &&
            uploading == 0 &&
            frame.composer.mode == Composer.Resume

    val landingInFeed: List<SentPrompt>
        get() = frame?.underway.orEmpty().filter { it.lands == Lands.Feed }

    val landingInQueue: List<SentPrompt>
        get() = frame?.underway.orEmpty().filter { it.lands == Lands.Queue }

    val base: String?
        get() = frame?.git?.baseBranch

    val comparisons: List<Comparison>
        get() = if (base == null) {
            listOf(Comparison.Uncommitted)
        } else {
            listOf(Comparison.Uncommitted, Comparison.OnBranch)
        }

    val changes: ChangeTotals?
        get() {
            val totals = when (comparison) {
                Comparison.Uncommitted -> frame?.git?.uncommitted
                Comparison.OnBranch -> frame?.git?.onBranch
            }
            return totals?.takeIf { it.files > 0 || it.added + it.removed > 0 }
        }
}

/**
 * One open chat as the Android client holds it.
 *
 * Holds the session's windowed row keys oldest-first, updating only at its two
 * edges until a reload, and keeps one keyed [RowCell] per drawn row so a change
 * to one item recomposes only that cell. Manages following/history, top-edge
 * paging, daemon-backed draft persistence, attachment validation, paste-as-
 * attachment, slash-command autocomplete and client commands, ask unsent state,
 * overview changed-file listings, and in-progress diff reviews.
 */
class ChatModel(
    val source: ChatSource,
    private val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default),
    private val bridge: ChatBridge = NativeChatBridge,
    private val attachmentDir: File = File(System.getProperty("java.io.tmpdir", "/tmp"), "amux-attachments"),
    loadingHintAfterMs: Long = LOADING_HINT_DELAY_MS,
    blobCacheMaxBytes: Long = DEFAULT_BLOB_CACHE_BYTES,
) {
    private val mutableKeys = mutableListOf<String>()
    private val places = mutableMapOf<String, Int>()
    private var firstPlace: Int = 0
    private val cells = mutableMapOf<String, RowCell>()

    private var sequence: Int = 0
    private var revision: Int = 0
    private var empty: Boolean = true
    private var heldCount: Int = 0

    private var frame: ChatFrame? = null
    private var ask: AskCard? = null
    private var overview: Overview? = null
    private var overviewChangesComparison: Comparison? = null
    private var overviewChangesLoading: Boolean = false
    private var overviewChangesError: String? = null
    private var settings: SettingsView? = null
    private var openedAgents: Map<List<Int>, OpenedAgent> = emptyMap()
    private var liveSubagents: GroupState = EMPTY_GROUP_STATE
    private var groupStates: Map<String, GroupState> = emptyMap()
    private val expanded = mutableSetOf<String>()

    private var paging: ChatPaging = ChatPaging.Idle
    private var loadingHint: Boolean = false
    private var following: Boolean = true
    private var toNewest: Int = 0

    private var draftText: String = ""
    private val positionedAttachments = mutableListOf<PositionedAttachment>()
    private var uploading: Int = 0
    private var sending: Boolean = false
    private var notice: String? = null
    private var comparison: Comparison = Comparison.Uncommitted
    private var asks: ChatAsk? = null
    private var openedAttachment: File? = null
    private var openingBlob: BlobRef? = null
    private var dictation: DictationState = DictationState()
    private var slashMatches: List<CommandMatch> = emptyList()

    private var questionKept: Pair<String, QuestionDraft>? = null
    private var formKept: Pair<String, List<FormValue>>? = null

    private data class Listed(val comparison: Comparison, val totals: ChangeTotals?)
    private var overviewShown: Boolean = false
    private var listed: Listed? = null
    var listingJob: Job? = null
        private set

    var reviewing: ReviewModel? = null
        private set

    private data class PastedRun(val at: Int, val text: String)
    private var pasted: PastedRun? = null

    private var loadingHintJob: Job? = null
    private val blobCache = BlobLruCache(maxBytes = blobCacheMaxBytes)

    internal val blobCacheBytes: Long
        get() = blobCache.totalBytes

    /**
     * Called each time the draft or attachments change so drafts persist with
     * the local daemon. Cleared automatically if the agent ends/is deleted.
     */
    var keeping: ((Draft) -> Unit)? = null

    private val _state = MutableStateFlow(ChatUiState())
    val state: StateFlow<ChatUiState> = _state.asStateFlow()

    val snapshot: ChatUiState get() = _state.value
    val keys: List<String> get() = _state.value.keys
    val ids: List<String> get() = _state.value.keys
    val currentFrame: ChatFrame? get() = frame
    val currentAsk: AskCard? get() = ask
    val currentOverview: Overview? get() = overview
    val currentSettings: SettingsView? get() = settings
    val draft: String get() = draftText
    val attachments: List<DraftAttachment> get() = positionedAttachments.map { it.attachment }
    val isFollowing: Boolean get() = following
    val arrivalsHeld: Boolean get() = _state.value.arrivalsHeld
    val newActivity: Boolean get() = _state.value.newActivity
    val canSend: Boolean get() = _state.value.canSend
    val canResume: Boolean get() = _state.value.canResume
    val hasDraft: Boolean get() = _state.value.hasDraft

    init {
        val opening = source.opening()
        val openingKeys = opening.keys
        if (openingKeys is KeysMoved.Reload) {
            holdKeys(openingKeys.value)
        }
        if (opening.frame?.ended == null) {
            source.keptDraft()?.let { restoreWithoutSaving(it) }
            keeping = { draft -> source.keep(draft) }
        }
        show(opening)
        recomputeSlashMatches()
        publish()
        if (mutableKeys.isEmpty() && loadingHintAfterMs >= 0L) {
            waitForRows(loadingHintAfterMs)
        }
    }

    /** Stops draft persistence (e.g. when the agent has been deleted). */
    fun stopKeepingDraft() {
        keeping = null
    }

    /** Cancels background hint/listing timers owned by this model without cancelling [scope]. */
    fun close() {
        loadingHintJob?.cancel()
        loadingHintJob = null
        listingJob?.cancel()
        listingJob = null
    }

    // --- Reading & Deltas --------------------------------------------------

    /**
     * Applies one incremental [ChatDelta] from the runtime or a scripted source.
     */
    fun apply(delta: ChatDelta) {
        when (val moved = delta.keys) {
            is KeysMoved.Reload -> swap(moved.value)
            is KeysMoved.Edges -> moveEdges(moved.above, moved.below, moved.dropped)
            null -> Unit
        }
        fill(delta.rows)
        show(delta)
        if (delta.rows.isNotEmpty()) {
            openIfLanded()
        }
        recomputeSlashMatches()
        publish()
    }

    /**
     * Reads the current [Row] for [id] without caching a missing-row cell for
     * keys that are not yet in the held window.
     */
    fun rowOrNull(id: String): Row? {
        cells[id]?.row?.let { return it }
        if (id !in places) {
            return source.rowsFor(listOf(id)).firstOrNull()
        }
        return cell(id).row
    }

    /** The keyed [RowCell] for [id], reading its [Row] from [source] lazily on first draw. */
    fun cell(id: String): RowCell {
        cells[id]?.let { existing ->
            if (existing.row != null || id !in places) {
                return existing
            }
            val row = source.rowsFor(listOf(id)).firstOrNull()
            if (row != null) {
                existing.updateRow(
                    row = row,
                    revision = revision,
                    openedAgent = openedBy(row),
                    groupState = row.group?.let(::stateOf),
                )
            }
            return existing
        }
        val row = source.rowsFor(listOf(id)).firstOrNull()
        val cell = RowCell(
            id = id,
            initialRow = row,
            initialRevision = revision,
            initialOpenedAgent = row?.let(::openedBy),
            initialGroupState = row?.group?.let(::stateOf),
        )
        if (row != null || id in places) {
            cells[id] = cell
        }
        return cell
    }

    /** Revision at which a drawn row was last updated; 0 for rows never drawn. */
    fun revisionOf(id: String): Int = cells[id]?.revision ?: 0

    /**
     * Whether [row] draws in the transcript: collapsed rows do not unless
     * they are steps under an expanded subagent, run pass-through rows stay
     * hidden inside runs, and closed turn work stays folded.
     */
    fun shows(row: Row): Boolean {
        if (row.run != null && row.kind.passesThroughRuns) return false
        val turn = row.turn
        if (turn != null && !turn.open && turn.part == TurnPart.Work) return false
        return !row.collapsed || (row.parent != null && row.parent in expanded)
    }

    /**
     * The nearest row below [id] that draws, passing over collapsed and hidden
     * rows so the rail knows whether to continue.
     */
    fun rowBelow(id: String): Row? {
        val place = places[id] ?: return null
        val start = place - firstPlace + 1
        if (start < mutableKeys.size) {
            for (index in start until mutableKeys.size) {
                val nextRow = cell(mutableKeys[index]).row ?: continue
                if (nextRow.kind == RowKind.Hidden) continue
                if (shows(nextRow)) return nextRow
            }
        }
        return null
    }

    /** Opens or closes a row's own detail or a legacy subagent's steps. */
    fun toggle(id: String) {
        if (!expanded.add(id)) {
            expanded.remove(id)
        }
        if (cells.isNotEmpty()) {
            revision += 1
            for (cell in cells.values) {
                cell.bumpRevision(revision)
            }
        }
        publish()
    }

    fun isExpanded(id: String): Boolean = id in expanded

    fun toggleRun(run: Run) {
        source.toggleRun(run.last)
    }

    fun toggleRun(memberKey: String) {
        source.toggleRun(memberKey)
    }

    fun openedBy(row: Row): OpenedAgent? = row.opens?.let { openedAgents[it] }

    fun stateOf(group: SubagentGroup): GroupState? = groupStates[group.first]

    fun toggleGroup(group: SubagentGroup) {
        source.toggleGroup(group.first)
    }

    fun toggleGroup(memberKey: String) {
        source.toggleGroup(memberKey)
    }

    fun foldTurns(on: Boolean) {
        source.foldTurns(on)
    }

    fun toggleTurn(turn: TurnPlace) {
        source.toggleTurn(turn.end)
    }

    fun toggleTurn(endKey: String) {
        source.toggleTurn(endKey)
    }

    /** What a run's steps did, carried on its newest step's row only. */
    fun countsOf(run: Run): RunCounts? = cell(run.last).row?.run?.counts

    // --- Blobs & Attachment Preview ----------------------------------------

    /** Cached or runtime-held attachment bytes for [hash]. */
    fun blob(hash: List<Int>): ByteArray? {
        blobCache.get(hash)?.let { return it }
        val loaded = source.blob(hash) ?: return null
        blobCache.put(hash, loaded)
        return loaded
    }

    fun bytesOf(blob: BlobRef): ByteArray? = blob(blob.hash)

    fun fetchFailed(blob: BlobRef): Boolean = source.blobFailed(blob.hash)

    fun fetchBlob(hash: List<Int>) {
        source.fetchBlob(hash)
    }

    /**
     * Opens an attachment as a file in [attachmentDir] as soon as its bytes
     * are available, retrying a previously failed fetch.
     */
    fun openAttachment(blob: BlobRef) {
        if (source.blobFailed(blob.hash)) {
            source.fetchBlob(blob.hash)
        }
        openingBlob = blob
        openIfLanded()
        publish()
    }

    fun clearOpenedAttachment() {
        openedAttachment = null
        openingBlob = null
        publish()
    }

    private fun openIfLanded() {
        val target = openingBlob ?: return
        val data = blob(target.hash)
        if (data != null) {
            openingBlob = null
            openedAttachment = saveAttachmentFile(data, target)
        } else if (source.blobFailed(target.hash)) {
            openingBlob = null
        }
    }

    private fun saveAttachmentFile(data: ByteArray, blob: BlobRef): File? {
        val hex = blob.hash.joinToString(separator = "") { "%02x".format(it and 0xFF) }
        val folder = File(attachmentDir, hex)
        val lastComponent = blob.name.split('/', '\\').lastOrNull().orEmpty()
        val safeName = lastComponent.dropWhile { it == '.' }.ifEmpty { "attachment" }
        val file = File(folder, safeName)
        return try {
            folder.mkdirs()
            file.writeBytes(data)
            file
        } catch (_: Throwable) {
            null
        }
    }

    // --- Windowed Keys & Scrolling -----------------------------------------

    private fun moveEdges(above: List<String>, below: List<String>, dropped: Int) {
        var moved = dropOlder(dropped)
        if (below.isNotEmpty()) {
            mutableKeys.addAll(0, below)
            firstPlace -= below.size
            for ((offset, key) in below.withIndex()) {
                places[key] = firstPlace + offset
            }
            moved = true
        }
        if (above.isNotEmpty()) {
            val baseOffset = mutableKeys.size
            for ((offset, key) in above.withIndex()) {
                places[key] = firstPlace + baseOffset + offset
            }
            mutableKeys.addAll(above)
            loadingHint = false
            loadingHintJob?.cancel()
            loadingHintJob = null
            moved = true
        }
        if (moved) {
            val nullArrivals = (below + above).filter { key ->
                val existing = cells[key]
                existing != null && existing.row == null
            }
            if (nullArrivals.isNotEmpty()) {
                refresh(nullArrivals)
            }
            sequenceChanged()
        }
    }

    private fun dropOlder(count: Int): Boolean {
        if (count <= 0) return false
        val actual = count.coerceAtMost(mutableKeys.size)
        for (i in 0 until actual) {
            val key = mutableKeys[i]
            places.remove(key)
            cells.remove(key)
        }
        mutableKeys.subList(0, actual).clear()
        firstPlace += actual
        return true
    }

    private fun swap(newKeys: List<String>) {
        holdKeys(newKeys)
        val iterator = cells.keys.iterator()
        while (iterator.hasNext()) {
            if (iterator.next() !in places) {
                iterator.remove()
            }
        }
        refresh(cells.keys.toList())
        loadingHint = false
        loadingHintJob?.cancel()
        loadingHintJob = null
        setFollowing(true)
        toNewest += 1
    }

    private fun setFollowing(value: Boolean) {
        if (following == value) return
        following = value
        source.follow(value)
    }

    private fun holdKeys(newKeys: List<String>) {
        mutableKeys.clear()
        mutableKeys.addAll(newKeys)
        firstPlace = 0
        places.clear()
        for ((index, key) in newKeys.withIndex()) {
            places[key] = index
        }
        sequenceChanged()
    }

    private fun sequenceChanged() {
        sequence += 1
        heldCount = mutableKeys.size
        empty = mutableKeys.isEmpty()
    }

    private fun refresh(keysToRead: List<String>) {
        if (keysToRead.isEmpty()) return
        val rows = source.rowsFor(keysToRead)
        revision += 1
        for (row in rows) {
            val cell = cells[row.id] ?: continue
            cell.updateRow(
                row = row,
                revision = revision,
                openedAgent = openedBy(row),
                groupState = row.group?.let(::stateOf),
            )
        }
    }

    private fun fill(rows: List<Row>) {
        val next = revision + 1
        var moved = false
        for (row in rows) {
            if (row.id !in places) continue
            val cell = cells[row.id] ?: continue
            if (cell.row == row) {
                if (row.holdsAttachment) {
                    cell.bumpAttachmentRevision()
                }
                continue
            }
            cell.updateRow(
                row = row,
                revision = next,
                openedAgent = openedBy(row),
                groupState = row.group?.let(::stateOf),
            )
            moved = true
        }
        if (moved) {
            revision = next
        }
    }

    private fun show(delta: ChatDelta) {
        val before = frame
        delta.frame?.let { incoming ->
            val cleaned = if (incoming.refused.isNotEmpty()) {
                incoming.refused.forEach(::takeBack)
                incoming.copy(refused = emptyList())
            } else {
                incoming
            }
            if (cleaned.ended != null) {
                keeping = null
            }
            frame = cleaned
        }
        delta.ask?.let { moved ->
            ask = moved.card
            if (questionKept != null && questionKept?.first != ask?.key) {
                questionKept = null
            }
            if (formKept != null && formKept?.first != ask?.key) {
                formKept = null
            }
        }
        delta.overview?.let { incomingOverview ->
            if (incomingOverview.changes != null) {
                overview = incomingOverview
                overviewChangesComparison = comparison
                overviewChangesLoading = false
                overviewChangesError = null
            } else {
                val keepChanges = overview?.changes?.takeIf {
                    overviewChangesComparison == comparison &&
                        listed == Listed(comparison = comparison, totals = totals(comparison))
                }
                overview = if (keepChanges != null) {
                    incomingOverview.copy(changes = keepChanges)
                } else {
                    overviewChangesComparison = null
                    incomingOverview
                }
            }
        }
        delta.settings?.let { settings = it }
        delta.agents?.let { holdAgents(it) }
        if (frame?.git != before?.git) {
            listChangedFiles()
        }
    }

    private fun holdAgents(agents: ChatAgents) {
        if (agents.live != liveSubagents) {
            liveSubagents = agents.live
        }
        val nextOpened = LinkedHashMap<List<Int>, OpenedAgent>(agents.agents.size)
        for (entry in agents.agents) {
            nextOpened.putIfAbsent(entry.agent.agent, entry)
        }
        val nextGroups = LinkedHashMap<String, GroupState>(agents.groups.size)
        for (entry in agents.groups) {
            nextGroups.putIfAbsent(entry.first, entry.state)
        }
        if (nextOpened == openedAgents && nextGroups == groupStates) return
        openedAgents = nextOpened
        groupStates = nextGroups
        val next = revision + 1
        var moved = false
        for (cell in cells.values) {
            val row = cell.row ?: continue
            if (row.opens != null || row.group != null) {
                cell.updateStanding(
                    openedAgent = row.opens?.let { openedAgents[it] },
                    groupState = row.group?.let { groupStates[it.first] },
                    revision = next,
                )
                moved = true
            }
        }
        if (moved) {
            revision = next
        }
    }

    private fun waitForRows(delayMs: Long) {
        if (delayMs == 0L) {
            loadingHint = true
            publish()
            return
        }
        loadingHintJob?.cancel()
        loadingHintJob = scope.launch {
            delay(delayMs)
            if (mutableKeys.isEmpty()) {
                loadingHint = true
                publish()
            }
        }
    }

    /** Tells the model whether the reader is at the newest row or scrolled into history. */
    fun reading(atNewest: Boolean) {
        setFollowing(atNewest)
        publish()
    }

    /** Takes the reader back to the newest row and resumes following. */
    fun jumpToNewest() {
        setFollowing(true)
        toNewest += 1
        publish()
    }

    /**
     * Requests one older page when the reader reaches near the top of the held
     * transcript (fewer than [PAGE_ROWS] held rows remain above [visibleFirstIndex]).
     *
     * Returns the paging [Job] so callers or tests can await completion, or
     * `null` when no page was requested.
     */
    fun reachedTop(visibleFirstIndex: Int = 0): Job? {
        if (visibleFirstIndex >= PAGE_ROWS) return null
        if (frame?.hasOlder != true || paging == ChatPaging.Fetching) return null
        paging = ChatPaging.Fetching
        val rowsToAsk = pageSize
        publish()
        return scope.launch {
            val outcome = source.pageOlder(rowsToAsk)
            paging = when (outcome) {
                is PageOutcome.Arrived -> ChatPaging.Idle
                PageOutcome.OriginUnreachable -> ChatPaging.Unreachable
                is PageOutcome.Failed -> ChatPaging.Failed(outcome.value)
                null -> ChatPaging.Failed("the chat is closed")
            }
            publish()
        }
    }

    /**
     * A page, or at a collapsed run that continues below the window, the run's
     * own step count up to [LARGEST_PAGE].
     */
    val pageSize: Int
        get() {
            val top = mutableKeys.asSequence().mapNotNull { cell(it).row }.firstOrNull { shows(it) }
            val run = top?.run
            if (top != null && run != null && run.openBelow && top.id == run.last && !run.open) {
                return run.steps.coerceIn(PAGE_ROWS, LARGEST_PAGE)
            }
            return PAGE_ROWS
        }

    // --- Changes & Overview ------------------------------------------------

    private fun totals(forComparison: Comparison): ChangeTotals? = when (forComparison) {
        Comparison.Uncommitted -> frame?.git?.uncommitted
        Comparison.OnBranch -> frame?.git?.onBranch
    }

    fun compare(comparison: Comparison) {
        if (this.comparison == comparison) return
        this.comparison = comparison
        overviewChangesError = null
        listChangedFiles()
        publish()
    }

    /** Called when the overview sheet opens or closes. */
    fun showOverview(shown: Boolean) {
        overviewShown = shown
        if (shown) {
            listed = null
            overviewChangesError = null
            listChangedFiles()
        } else {
            listingJob?.cancel()
            listingJob = null
            if (overviewChangesLoading) {
                overviewChangesLoading = false
                publish()
            }
        }
    }

    /** Retries loading the changed-file list for the active comparison. */
    fun retryChangedFiles() {
        listed = null
        overviewChangesError = null
        listChangedFiles()
        publish()
    }

    /** Explicitly fetches the [Overview] (with changed files) for [comparison]. */
    fun openOverview(comparison: Comparison = this.comparison): Deferred<Result<Overview>> {
        listingJob?.cancel()
        if (this.comparison == comparison) {
            overviewChangesLoading = true
            overviewChangesError = null
            publish()
        }
        val deferred = scope.async {
            val result = source.openOverview(comparison)
            if (this@ChatModel.comparison == comparison) {
                overviewChangesLoading = false
                result.fold(
                    onSuccess = { fetched ->
                        overview = fetched
                        overviewChangesComparison = comparison
                        overviewChangesError = null
                    },
                    onFailure = { failure ->
                        listed = null
                        overviewChangesError = failure.failureDescription
                    },
                )
                publish()
            }
            result
        }
        listingJob = deferred
        return deferred
    }

    private fun listChangedFiles() {
        if (!overviewShown) return
        val asked = Listed(comparison = comparison, totals = totals(comparison))
        if (asked == listed) return
        listed = asked
        listingJob?.cancel()
        overviewChangesLoading = true
        overviewChangesError = null
        publish()
        listingJob = scope.launch {
            val result = source.openOverview(asked.comparison)
            if (comparison != asked.comparison) return@launch
            overviewChangesLoading = false
            result.fold(
                onSuccess = { fetched ->
                    overview = fetched
                    overviewChangesComparison = asked.comparison
                    overviewChangesError = null
                },
                onFailure = { failure ->
                    listed = null
                    overviewChangesError = failure.failureDescription
                },
            )
            publish()
        }
    }

    // --- Ask Unsent State & Answering --------------------------------------

    fun keepQuestionDraft(askKey: String, draft: QuestionDraft) {
        if (ask?.key != askKey) return
        questionKept = askKey to draft
    }

    fun questionDraft(askKey: String): QuestionDraft? =
        if (questionKept?.first == askKey) questionKept?.second else null

    fun keepFormDraft(askKey: String, values: List<FormValue>) {
        if (ask?.key != askKey) return
        formKept = askKey to values.toList()
    }

    fun formDraft(askKey: String): List<FormValue>? =
        if (formKept?.first == askKey) formKept?.second else null

    /** Live validation problems for a tool-server form via [ChatBridge.formProblems]. */
    fun formProblems(fields: List<FormField>, values: List<FormValue>): List<FormProblem> =
        bridge.formProblems(fields, values).orEmpty()

    fun answer(choice: Int, note: String? = null, expectedKey: String? = null): Job? {
        val current = ask ?: return null
        if (expectedKey != null && current.key != expectedKey) return null
        return act(body = { it.answer(current.key, choice, note) })
    }

    fun answerPermission(choice: Int, note: String? = null, expectedKey: String? = null): Job? =
        answer(choice, note, expectedKey)

    fun answerQuestions(responses: List<QuestionResponse>, expectedKey: String? = null): Job? {
        val current = ask ?: return null
        if (expectedKey != null && current.key != expectedKey) return null
        return act(body = { it.answerQuestions(current.key, responses) })
    }

    fun answerQuestion(responses: List<QuestionResponse>, expectedKey: String? = null): Job? =
        answerQuestions(responses, expectedKey)

    fun replyInstead(text: String, soFar: List<QuestionResponse>, expectedKey: String? = null): Job? {
        val current = ask ?: return null
        if (expectedKey != null && current.key != expectedKey) return null
        return act(body = { it.replyInstead(current.key, text, soFar) })
    }

    fun submitForm(values: List<FormValue>, expectedKey: String? = null): Job? {
        val current = ask ?: return null
        if (expectedKey != null && current.key != expectedKey) return null
        return act(body = { it.answerForm(current.key, values) })
    }

    fun answerForm(values: List<FormValue>, expectedKey: String? = null): Job? =
        submitForm(values, expectedKey)

    fun resendAnswer(expectedKey: String? = null): Job? {
        if (expectedKey != null && ask?.key != expectedKey) return null
        val input = frame?.askInput ?: return null
        return resend(input)
    }

    fun discardAnswer(expectedKey: String? = null) {
        if (expectedKey != null && ask?.key != expectedKey) return
        val input = frame?.askInput ?: return
        discard(input)
    }

    // --- Drafting, Slash Commands, Pastes & Attachments --------------------

    private val composed: Draft
        get() = positionedDraftOf(draftText, positionedAttachments)

    fun setDraft(text: String) {
        if (draftText == text) return
        shiftAttachmentOffsets(draftText, text)
        draftText = text
        keepDraft()
        recomputeSlashMatches()
        publish()
    }

    fun clearDraft() {
        val changed = draftText.isNotEmpty() || positionedAttachments.isNotEmpty()
        draftText = ""
        positionedAttachments.clear()
        pasted = null
        if (changed) {
            keepDraft()
        }
        recomputeSlashMatches()
        publish()
    }

    fun clearNotice() {
        if (notice == null) return
        notice = null
        publish()
    }

    fun clearAsk() {
        if (asks == null) return
        asks = null
        publish()
    }

    /** Restores a kept draft ahead of anything typed since. */
    fun reopen(kept: Draft) {
        restore(kept)
        publish()
    }

    /** Puts a picked slash command or Codex skill into the draft. */
    fun pick(match: CommandMatch) {
        setDraft(match.picked)
    }

    fun acceptCommandMatch(match: CommandMatch) = pick(match)

    private fun recomputeSlashMatches() {
        val commands = settings?.commands.orEmpty()
        slashMatches = if (commands.isEmpty() || (!draftText.contains('/') && !draftText.contains('$'))) {
            emptyList()
        } else {
            bridge.commandMatches(commands, draftText).take(SLASH_ROWS)
        }
    }

    /**
     * Handles a composer field edit, converting long pastes into inline
     * [DraftAttachment.Text] chips via [ChatBridge.pastesAsAttachment].
     */
    fun type(text: String) {
        var updated = text.replace("\r\n", "\n").replace("\r", "\n")
        val previousPaste = pasted
        if (
            previousPaste != null &&
            updated.length >= previousPaste.at + previousPaste.text.length &&
            updated.substring(previousPaste.at, previousPaste.at + previousPaste.text.length) == previousPaste.text
        ) {
            updated = updated.removeRange(previousPaste.at, previousPaste.at + previousPaste.text.length)
        } else {
            pasted = null
        }

        val old = draftText
        var head = 0
        while (head < old.length && head < updated.length && old[head] == updated[head]) {
            head += 1
        }
        var tail = 0
        while (
            tail < old.length - head &&
            tail < updated.length - head &&
            old[old.length - 1 - tail] == updated[updated.length - 1 - tail]
        ) {
            tail += 1
        }
        var inserted = updated.substring(head, updated.length - tail)
        if (!bridge.pastesAsAttachment(inserted)) {
            setDraft(updated)
            return
        }
        var before = updated.substring(0, head)
        var after = updated.substring(updated.length - tail)
        if (inserted.startsWith(' ') && before.lastOrNull()?.isWhitespace() == false) {
            inserted = inserted.substring(1)
            before += " "
        }
        if (inserted.endsWith(' ') && after.firstOrNull()?.isWhitespace() == false) {
            inserted = inserted.dropLast(1)
            after = " $after"
        }
        val nextText = before + after
        shiftAttachmentOffsets(old, nextText)
        draftText = nextText
        insertPositionedAttachment(
            attachment = DraftAttachment.Text(name = "Pasted text", text = inserted),
            offset = before.length,
        )
        pasted = PastedRun(at = before.length, text = inserted)
        keepDraft()
        recomputeSlashMatches()
        publish()
    }

    /**
     * Explicit paste handler: creates a text attachment chip when
     * [ChatBridge.pastesAsAttachment] says so, otherwise appends to the draft.
     */
    fun paste(text: String) {
        val normalized = text.replace("\r\n", "\n").replace("\r", "\n")
        if (bridge.pastesAsAttachment(normalized)) {
            positionedAttachments.add(
                PositionedAttachment(
                    attachment = DraftAttachment.Text(name = "Pasted text", text = normalized),
                    offset = null,
                ),
            )
            keepDraft()
            publish()
        } else {
            setDraft(draftText + normalized)
        }
    }

    // --- Dictation ---------------------------------------------------------

    fun prepareDictation(
        speech: DictationState.Permission,
        microphone: DictationState.Permission,
        available: Boolean,
    ) {
        dictation = dictation.prepare(speech, microphone, available)
        publish()
    }

    fun beginDictation() {
        dictation = dictation.began(draftText)
        publish()
    }

    fun heard(text: String) {
        val (nextDictation, nextDraft) = dictation.receive(text, draftText)
        dictation = nextDictation
        if (nextDraft != draftText) {
            draftText = nextDraft
            keepDraft()
            recomputeSlashMatches()
        }
        publish()
    }

    fun stopDictation() {
        dictation = dictation.stop()
        publish()
    }

    fun dictationFailed() {
        dictation = dictation.failed()
        publish()
    }

    // --- Attachment Validation & Upload ------------------------------------

    /**
     * Checks whether a file of [size] bytes may be attached before reading or
     * uploading it. Refuses oversized files with a notice naming the file and
     * the limit.
     */
    fun admits(name: String, size: Long): Boolean {
        val limit = bridge.attachmentLimit
        if (size <= limit) return true
        val megabytes = limit / (1024L * 1024L)
        notice = "$name is not attached because it is over the $megabytes MB limit for attachments."
        publish()
        return false
    }

    fun attach(attachment: PickedAttachment): Job? =
        attach(attachment.data, attachment.name, attachment.mime, attachment.image)

    fun attach(
        data: ByteArray,
        name: String,
        mime: String,
        image: Boolean,
    ): Job? = attachCheckingSize(
        name = name,
        size = data.size.toLong(),
        mime = mime,
        image = image,
        readBytes = { data },
    )

    /**
     * Validates [size] against [ChatBridge.attachmentLimit] BEFORE invoking
     * [readBytes] or uploading via [ChatSource.putBlob].
     */
    fun attachCheckingSize(
        name: String,
        size: Long,
        mime: String,
        image: Boolean,
        readBytes: suspend () -> ByteArray,
    ): Job? {
        if (!admits(name, size)) return null
        uploading += 1
        notice = null
        publish()
        return scope.launch {
            val bytes = try {
                readBytes()
            } catch (t: Throwable) {
                uploading -= 1
                notice = "$name is not attached: ${t.failureDescription}"
                publish()
                return@launch
            }
            if (!admits(name, bytes.size.toLong())) {
                uploading -= 1
                publish()
                return@launch
            }
            val stored = source.putBlob(bytes, name, mime)
            uploading -= 1
            stored.fold(
                onSuccess = { blob ->
                    blobCache.put(blob.hash, bytes)
                    positionedAttachments.add(
                        PositionedAttachment(
                            attachment = if (image) DraftAttachment.Image(blob) else DraftAttachment.File(blob),
                            offset = null,
                        ),
                    )
                    keepDraft()
                },
                onFailure = { failure ->
                    notice = "$name is not attached: ${failure.failureDescription}"
                },
            )
            publish()
        }
    }

    fun removeAttachment(index: Int) {
        if (index !in positionedAttachments.indices) return
        positionedAttachments.removeAt(index)
        keepDraft()
        publish()
    }

    // --- Review ------------------------------------------------------------

    /**
     * Opens (or reuses) the [ReviewModel] for the active [comparison].
     *
     * Returns `Result.success(null)` when there are no changes; returns
     * `Result.failure` when the host failed or the patch could not be parsed.
     */
    fun openReview(): Deferred<Result<ReviewModel?>> = scope.async { loadReview() }

    suspend fun loadReview(): Result<ReviewModel?> {
        val frozen = source.review(comparison).getOrElse { return Result.failure(it) }
        val existing = reviewing
        val existingHash = existing?.review?.diff?.patch?.hash
        val incomingHash = frozen.diff.patch?.hash
        if (existing != null && existingHash != null && existingHash == incomingHash) {
            return Result.success(existing)
        }
        val doc = ReviewModel.document(frozen, emptyList(), bridge::reviewDoc)
            ?: return Result.failure(RuntimeFailure(ReviewModel.COULD_NOT_READ_DIFF))
        if (doc.files.isEmpty()) {
            return if (frozen.diff.files.isEmpty()) {
                reviewing = null
                Result.success(null)
            } else {
                Result.failure(RuntimeFailure(ReviewModel.MISSING_LISTED_FILES))
            }
        }
        val created = ReviewModel(
            review = frozen,
            comparison = comparison,
            renderDoc = bridge::reviewDoc,
            initialDoc = doc,
        )
        reviewing = created
        return Result.success(created)
    }

    /** Attaches a written [ReviewModel] to the draft, replacing any earlier review of the same patch. */
    fun attach(review: ReviewModel) {
        val patchHash = review.review.diff.patch?.hash
        positionedAttachments.removeAll { existing ->
            existing.attachment is DraftAttachment.Review &&
                existing.attachment.diff.patch?.hash == patchHash
        }
        positionedAttachments.add(PositionedAttachment(review.attachment, offset = null))
        keepDraft()
        publish()
    }

    // --- Sending, Queueing & Acts ------------------------------------------

    /**
     * Sends the current draft. Drafts starting with `/` go through
     * [ChatSource.command] first; other drafts go straight to [ChatSource.send].
     */
    fun send(): Job? {
        if (!canSend) return null
        dictation = dictation.stop()
        if (draftText.trimStart().startsWith("/")) {
            return runCommand(composed)
        }
        return sendAsTyped()
    }

    private fun runCommand(typed: Draft): Job {
        sending = true
        notice = null
        publish()
        return scope.launch {
            val outcome = source.command(typed)
            sending = false
            when (outcome) {
                CommandOutcome.Send -> {
                    sendDraft(sent = typed, clearCurrentDraft = composed == typed).join()
                    return@launch
                }
                is CommandOutcome.Pick -> {
                    if (composed == typed) clearDraftInternal()
                    asks = ChatAsk.Settings(command = outcome.value)
                }
                CommandOutcome.AskName -> {
                    if (composed == typed) clearDraftInternal()
                    asks = ChatAsk.Rename
                }
                is CommandOutcome.Acted -> {
                    when (val acted = outcome.value) {
                        ActOutcome.Done -> if (composed == typed) clearDraftInternal()
                        is ActOutcome.Rejected -> notice = notSent(acted.value)
                        is ActOutcome.Failed -> notice = acted.value
                        ActOutcome.NotConfirmed -> notice = NOT_CONFIRMED
                    }
                }
                is CommandOutcome.Sent -> {
                    if (composed == typed) clearDraftInternal()
                    setFollowing(true)
                    toNewest += 1
                }
                is CommandOutcome.Refused -> {
                    notice = refusedCommand(
                        command = outcome.command,
                        offered = outcome.offered,
                        hint = outcome.hint,
                        kind = frame?.kind,
                    )
                }
                is CommandOutcome.Unknown -> {
                    notice = unknownCommand(
                        command = outcome.command,
                        value = outcome.value,
                        offered = outcome.offered,
                    )
                }
                null -> notice = CLOSED
            }
            publish()
        }
    }

    private fun sendAsTyped(): Job = sendDraft(sent = composed, clearCurrentDraft = true)

    private fun sendDraft(sent: Draft, clearCurrentDraft: Boolean): Job {
        if (clearCurrentDraft) {
            clearDraftInternal()
        }
        sending = true
        notice = null
        setFollowing(true)
        toNewest += 1
        publish()
        return scope.launch {
            val outcome = source.send(sent)
            sending = false
            outcome.onFailure { failure ->
                restore(sent)
                notice = failure.failureDescription
            }
            publish()
        }
    }

    /** Starts an exited agent again with the composer's draft as its first prompt. */
    fun resume(): Job? {
        if (!canResume) return null
        dictation = dictation.stop()
        val sent = composed
        sending = true
        notice = null
        publish()
        return scope.launch {
            val outcome = source.resume(sent)
            sending = false
            when (outcome) {
                ActOutcome.Done -> {
                    if (composed == sent) clearDraftInternal()
                    setFollowing(true)
                    toNewest += 1
                }
                is ActOutcome.Rejected -> notice = notSent(outcome.value)
                is ActOutcome.Failed -> notice = outcome.value
                ActOutcome.NotConfirmed -> notice = NOT_CONFIRMED
                null -> notice = CLOSED
            }
            publish()
        }
    }

    fun withdraw(queued: QueuedRow): Job? {
        val taken = source.draftOf(queued.inputId)
        return act(
            body = { it.withdraw(queued.inputId) },
            onDone = {
                if (taken != null) {
                    restore(taken)
                }
            },
        )
    }

    fun sendNow(queued: QueuedRow): Job? = act(body = { it.sendNow(queued.inputId) })

    fun resend(inputId: List<Int>): Job {
        notice = null
        publish()
        return scope.launch {
            source.resend(inputId)
        }
    }

    fun discard(inputId: List<Int>) {
        source.discard(inputId)
    }

    fun interrupt(): Job? = act(body = { it.interrupt() })

    fun stopJob(key: String): Job? = act(body = { it.stopJob(key) })

    fun switchBack(): Job? = act(body = { it.switchBack() })

    fun change(setting: SettingChange): Job? = act(body = { it.changeSetting(setting) })

    fun setSetting(setting: SettingChange): Job? = change(setting)

    private fun act(
        body: suspend (ChatSource) -> ActOutcome?,
        onDone: (() -> Unit)? = null,
    ): Job {
        notice = null
        publish()
        return scope.launch {
            when (val outcome = body(source)) {
                ActOutcome.Done -> onDone?.invoke()
                is ActOutcome.Rejected -> notice = notSent(outcome.value)
                is ActOutcome.Failed -> notice = outcome.value
                ActOutcome.NotConfirmed -> {
                    if (ask?.state != CardState.NotConfirmed) {
                        notice = NOT_CONFIRMED
                    }
                }
                null -> notice = CLOSED
            }
            publish()
        }
    }

    // --- Draft helpers -----------------------------------------------------

    private fun clearDraftInternal() {
        val changed = draftText.isNotEmpty() || positionedAttachments.isNotEmpty()
        draftText = ""
        positionedAttachments.clear()
        pasted = null
        if (changed) {
            keepDraft()
        }
        recomputeSlashMatches()
    }

    private fun keepDraft() {
        keeping?.invoke(positionedDraftOf(draftText, positionedAttachments))
    }

    private fun restore(sent: Draft) {
        restoreWithoutSaving(sent)
        keepDraft()
        recomputeSlashMatches()
    }

    private fun restoreWithoutSaving(sent: Draft) {
        val words = sent.words
        val prefixShift = when {
            draftText.isEmpty() -> 0
            words.isEmpty() -> 0
            else -> words.length + 1
        }
        if (prefixShift > 0) {
            for (i in positionedAttachments.indices) {
                val entry = positionedAttachments[i]
                val offset = entry.offset ?: continue
                positionedAttachments[i] = entry.copy(offset = offset + prefixShift)
            }
        }
        draftText = when {
            draftText.isEmpty() -> words
            words.isEmpty() -> draftText
            else -> "$words\n$draftText"
        }
        val restoredAttachments = sent.positionedAttachments()
        if (restoredAttachments.isNotEmpty()) {
            positionedAttachments.addAll(0, restoredAttachments)
        }
    }

    private fun insertPositionedAttachment(attachment: DraftAttachment, offset: Int) {
        val entry = PositionedAttachment(attachment, offset = offset)
        val insertAt = positionedAttachments.indexOfFirst { existing ->
            val existingOffset = existing.offset
            existingOffset == null || existingOffset > offset
        }
        if (insertAt < 0) {
            positionedAttachments.add(entry)
        } else {
            positionedAttachments.add(insertAt, entry)
        }
    }

    private fun shiftAttachmentOffsets(old: String, updated: String) {
        if (positionedAttachments.none { it.offset != null }) return
        var head = 0
        while (head < old.length && head < updated.length && old[head] == updated[head]) {
            head += 1
        }
        var tail = 0
        while (
            tail < old.length - head &&
            tail < updated.length - head &&
            old[old.length - 1 - tail] == updated[updated.length - 1 - tail]
        ) {
            tail += 1
        }
        val oldEnd = old.length - tail
        val newEnd = updated.length - tail
        val delta = updated.length - old.length
        for (i in positionedAttachments.indices) {
            val entry = positionedAttachments[i]
            val offset = entry.offset ?: continue
            val nextOffset = when {
                offset <= head -> offset
                offset >= oldEnd -> (offset + delta).coerceIn(0, updated.length)
                else -> newEnd.coerceIn(0, updated.length)
            }
            if (nextOffset != offset) {
                positionedAttachments[i] = entry.copy(offset = nextOffset)
            }
        }
    }

    private fun takeBack(refused: RefusedPrompt) {
        source.draftOf(refused.inputId)?.let { restore(it) }
        notice = notSent(refused.reason)
        source.discard(refused.inputId)
    }

    private fun publish() {
        _state.value = ChatUiState(
            keys = mutableKeys.toList(),
            sequence = sequence,
            revision = revision,
            empty = empty,
            heldCount = heldCount,
            frame = frame,
            ask = ask,
            overview = overview,
            overviewChangesComparison = overviewChangesComparison,
            overviewChangesLoading = overviewChangesLoading,
            overviewChangesError = overviewChangesError,
            settings = settings,
            openedAgents = openedAgents,
            liveSubagents = liveSubagents,
            groupStates = groupStates,
            expanded = expanded.toSet(),
            paging = paging,
            loadingHint = loadingHint,
            following = following,
            toNewest = toNewest,
            draft = draftText,
            attachments = positionedAttachments.map { it.attachment },
            uploading = uploading,
            sending = sending,
            notice = notice,
            comparison = comparison,
            asks = asks,
            openedAttachment = openedAttachment,
            dictation = dictation,
            slashMatches = slashMatches,
        )
    }

    private class BlobLruCache(private val maxBytes: Long) {
        private val entries = LinkedHashMap<List<Int>, ByteArray>(16, 0.75f, true)
        var totalBytes: Long = 0L
            private set

        fun get(key: List<Int>): ByteArray? = entries[key]

        fun put(key: List<Int>, value: ByteArray) {
            val size = value.size.toLong()
            entries.remove(key)?.let { previous ->
                totalBytes -= previous.size.toLong()
            }
            if (maxBytes <= 0L || size > maxBytes) {
                return
            }
            entries[key] = value
            totalBytes += size
            val iterator = entries.entries.iterator()
            while (totalBytes > maxBytes && iterator.hasNext()) {
                val eldest = iterator.next()
                if (eldest.key == key && entries.size == 1) break
                totalBytes -= eldest.value.size.toLong()
                iterator.remove()
            }
        }
    }

    companion object {
        /** A page of older rows, and the default paging window step. */
        const val PAGE_ROWS: Int = 40

        /** Upper bound when paging into a collapsed run that continues below the window. */
        const val LARGEST_PAGE: Int = 1000

        /** How long an empty chat waits before showing the loading hint. */
        const val LOADING_HINT_DELAY_MS: Long = 300L

        /** Maximum slash-command or skill autocomplete suggestions shown at once. */
        const val SLASH_ROWS: Int = 5

        /** Standard attachment byte cap (64 MiB), matching the Rust runtime limit. */
        const val ATTACHMENT_LIMIT_BYTES: Long = 64L * 1024L * 1024L

        /** Default in-memory LRU cache budget for fetched attachment blobs (32 MiB). */
        const val DEFAULT_BLOB_CACHE_BYTES: Long = 32L * 1024L * 1024L

        const val NOT_CONFIRMED: String = "The connection dropped before the agent answered."
        const val CLOSED: String = "This chat is closed."

        /** Lines as the chat counts them: a trailing newline ends a line rather than starting one. */
        fun lines(text: String): Int {
            val parts = text.split('\n')
            val count = if (parts.lastOrNull()?.isEmpty() == true) parts.size - 1 else parts.size
            return count
        }

        fun notSent(refused: Refusal): String = "Not sent: ${refusal(refused)}"

        fun refusal(refused: Refusal): String {
            refused.detail?.let { return it }
            return when (refused.reason) {
                RefusalReason.Exited -> "it had exited"
                RefusalReason.Exiting -> "it was exiting"
                RefusalReason.AgentClosed -> "it is settled"
                RefusalReason.Draining -> "its host is shutting down"
                RefusalReason.HostUnreachable -> "its host is away"
                RefusalReason.Unsupported -> "this agent can’t take it"
                RefusalReason.ClosedAsk -> "the ask had already closed"
                RefusalReason.NotQueued -> "it was no longer queued"
                RefusalReason.ConversationElsewhere ->
                    "Claude’s own view switched to another conversation"
                RefusalReason.TooLong -> "it is longer than the agent takes"
                RefusalReason.Unspecified -> "it was refused"
            }
        }

        fun refusedCommand(
            command: String,
            offered: List<String>,
            hint: String? = null,
            kind: Kind? = null,
        ): String {
            val name = if (command.startsWith("/")) command else "/$command"
            val agent = if (kind == Kind.Codex) "Codex" else "This agent"
            if (hint != null) {
                return "$agent takes a skill as a mention: type $hint."
            }
            val runs = offered.map { item ->
                if (item.startsWith("/") || item.startsWith("$")) item else "/$item"
            }
            return if (runs.isEmpty()) {
                "$agent does not run $name here."
            } else {
                "$agent does not run $name here. It runs ${runs.joinToString(", ")}."
            }
        }

        fun unknownCommand(
            command: ClientCommand,
            value: String,
            offered: List<String>,
        ): String {
            val list = offered.joinToString(", ")
            return when (command) {
                ClientCommand.Effort -> if (offered.isEmpty()) {
                    "No effort level named $value."
                } else {
                    "No effort level named $value. It offers $list."
                }
                ClientCommand.Model,
                ClientCommand.Rename,
                -> if (offered.isEmpty()) {
                    "No model named $value."
                } else {
                    "No model named $value. It offers $list."
                }
            }
        }
    }
}
