package sh.amux.core.runtime

import java.io.File
import java.io.IOException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.DisposableHandle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import sh.amux.values.ActOutcome
import sh.amux.values.ActivityKind
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
import sh.amux.values.GroupState
import sh.amux.values.KeysMoved
import sh.amux.values.Kind
import sh.amux.values.Lands
import sh.amux.values.OpenedAgent
import sh.amux.values.Overview
import sh.amux.values.PageOutcome
import sh.amux.values.QuestionResponse
import sh.amux.values.QueuedRow
import sh.amux.values.RefusedPrompt
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
import sh.amux.words.ChatWords
import sh.amux.words.ChatNotices
import sh.amux.words.EMPTY_GROUP_STATE

/**
 * Stateless view-helper bridge used by [ChatModel].
 *
 * Defaults to [NativeChatBridge] (the real Rust library via JNI); tests or
 * host previews without JNI loaded may pass a lightweight stand-in.
 */
interface ChatBridge {
    val attachmentLimit: Long
    fun commandMatches(commands: List<CommandView>, draft: String): List<CommandMatch>
    fun pastesAsAttachment(text: String): Boolean
    fun formProblems(fields: List<FormField>, values: List<FormValue>): List<FormProblem>?
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
    /** What the running activity is working on, from its row, or null. */
    val activitySubject: String? = null,
    /** The bytes of the draft's images that this phone holds, by blob hash. */
    val draftImages: Map<List<Int>, ByteArray> = emptyMap(),
    /**
     * How many edits [ChatModel.type] has taken from the composer. Each one
     * publishes, even when it leaves [draft] as it was (a paste that became a
     * chip), so the composer can tell which of its edits [draft] answers.
     */
    val typedEdits: Int = 0,
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
 *
 * [scope] must run on the main thread, where the model's state is read and
 * written. The model's work runs in a scope of its own on the same thread,
 * cancelled by [close] or with [scope], so sends, uploads and listings stop
 * with the chat.
 */
class ChatModel(
    val source: ChatSource,
    scope: CoroutineScope,
    private val bridge: ChatBridge = NativeChatBridge,
    private val attachmentDir: File = File(System.getProperty("java.io.tmpdir", "/tmp"), "amux-attachments"),
    loadingHintAfterMs: Long = LOADING_HINT_DELAY_MS,
    blobCacheMaxBytes: Long = DEFAULT_BLOB_CACHE_BYTES,
    private val ioDispatcher: CoroutineDispatcher = Dispatchers.IO,
) {
    private val work: Job = SupervisorJob()

    // Cancelled with the scope it was given, without being its child: a child
    // job stays active until completed, which would hold that scope open.
    private val cancelledWithParent: DisposableHandle? =
        scope.coroutineContext[Job]?.invokeOnCompletion { work.cancel() }

    // Initialised from the constructor's scope, which it shadows from here on.
    private val scope: CoroutineScope = CoroutineScope(scope.coroutineContext + work)

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
    private var savingAttachment: Job? = null
    private var dictation: DictationState = DictationState()
    private var slashMatches: List<CommandMatch> = emptyList()

    private var questionKept: Pair<String, QuestionDraft>? = null
    private var formKept: Pair<String, List<FormValue>>? = null

    private data class Listed(val comparison: Comparison, val totals: ChangeTotals?)
    private var overviewShown: Boolean = false
    private var listed: Listed? = null
    private var listingGeneration: Long = 0L
    var listingJob: Job? = null
        private set

    private data class PastedRun(val at: Int, val text: String)

    /**
     * One edit of the draft: the characters from [start] to [removedEnd] of
     * the old text became those from [start] to [insertedEnd] of the new.
     */
    private data class EditSpan(val start: Int, val removedEnd: Int, val insertedEnd: Int) {
        /** Whether this is an edit that takes [old] to [updated]. */
        fun fits(old: String, updated: String): Boolean =
            start in 0..removedEnd && removedEnd <= old.length && start <= insertedEnd &&
                insertedEnd <= updated.length && old.length - removedEnd == updated.length - insertedEnd &&
                old.regionMatches(0, updated, 0, start) &&
                old.regionMatches(removedEnd, updated, insertedEnd, old.length - removedEnd)

        companion object {
            /**
             * The edit inferred from the texts alone: the characters between
             * what the two share at the start and at the end.
             */
            fun between(old: String, updated: String): EditSpan {
                var head = 0
                while (head < old.length && head < updated.length && old[head] == updated[head]) head += 1
                var tail = 0
                while (
                    tail < old.length - head &&
                    tail < updated.length - head &&
                    old[old.length - 1 - tail] == updated[updated.length - 1 - tail]
                ) {
                    tail += 1
                }
                return EditSpan(head, old.length - tail, updated.length - tail)
            }
        }
    }
    private var pasted: PastedRun? = null

    private var loadingHintJob: Job? = null
    private val blobCache = BlobLruCache(maxBytes = blobCacheMaxBytes)

    private var typedEdits = 0
    private var activityKey: String? = null
    private var activitySubject: String? = null

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

    /** Stops everything this model started: sends, uploads, listings and timers. */
    fun close() {
        loadingHintJob = null
        listingGeneration += 1
        listingJob = null
        savingAttachment = null
        cancelledWithParent?.dispose()
        work.cancel()
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
        savingAttachment?.cancel()
        savingAttachment = null
        publish()
    }

    private fun openIfLanded() {
        val target = openingBlob ?: return
        val data = blob(target.hash)
        if (data != null) {
            openingBlob = null
            savingAttachment?.cancel()
            // An attachment runs to 64 MB, too much to write on the main thread.
            savingAttachment = scope.launch {
                val saved = withContext(ioDispatcher) { saveAttachmentFile(data, target) }
                savingAttachment = null
                openedAttachment = saved
                publish()
            }
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
        } catch (_: IOException) {
            null
        } catch (_: SecurityException) {
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
            val keepChanges = overview?.changes?.takeIf {
                overviewChangesComparison == comparison &&
                    listed == Listed(comparison = comparison, totals = totals(comparison))
            }
            overview = if (keepChanges != null) {
                incomingOverview.copy(changes = keepChanges)
            } else if (
                incomingOverview.changes != null &&
                !overviewChangesLoading &&
                overviewChangesError == null &&
                listingGeneration == 0L
            ) {
                overviewChangesComparison = comparison
                incomingOverview
            } else {
                overviewChangesComparison = null
                incomingOverview.copy(changes = null)
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
                null -> ChatPaging.Failed(ChatNotices.PAGING_CLOSED)
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
        if (overviewShown) {
            listChangedFiles()
        } else {
            listingGeneration += 1
            listingJob?.cancel()
            listingJob = null
            overviewChangesLoading = false
        }
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
            listingGeneration += 1
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
        val gen = ++listingGeneration
        listingJob?.cancel()
        if (this.comparison == comparison) {
            overviewChangesLoading = true
            overviewChangesError = null
            publish()
        }
        val deferred = scope.async {
            val result = source.openOverview(comparison)
            if (gen == listingGeneration && this@ChatModel.comparison == comparison) {
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
        val gen = ++listingGeneration
        listingJob?.cancel()
        overviewChangesLoading = true
        overviewChangesError = null
        publish()
        listingJob = scope.launch {
            val result = source.openOverview(asked.comparison)
            if (gen != listingGeneration || comparison != asked.comparison) return@launch
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

    fun answerQuestions(responses: List<QuestionResponse>, expectedKey: String? = null): Job? {
        val current = ask ?: return null
        if (expectedKey != null && current.key != expectedKey) return null
        return act(body = { it.answerQuestions(current.key, responses) })
    }

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
        setDraft(text, edit = null)
    }

    private fun setDraft(text: String, edit: EditSpan?) {
        if (draftText == text) return
        shiftAttachmentOffsets(draftText, text, edit)
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
    fun type(text: String, edit: DraftEdit? = null) {
        typedEdits += 1
        var updated = text.replace("\r\n", "\n").replace("\r", "\n")
        // The field's own account of the edit holds only for the text it
        // edited: the draft as the model has it, unaltered by line endings.
        var span = edit?.takeIf { it.original == draftText && updated == text }
            ?.let { EditSpan(it.start, it.removedEnd, it.insertedEnd) }
        val previousPaste = pasted
        if (
            previousPaste != null &&
            updated.length >= previousPaste.at + previousPaste.text.length &&
            updated.substring(previousPaste.at, previousPaste.at + previousPaste.text.length) == previousPaste.text
        ) {
            updated = updated.removeRange(previousPaste.at, previousPaste.at + previousPaste.text.length)
            span = null
        } else {
            pasted = null
        }

        val old = draftText
        val (head, oldEnd, newEnd) = span?.takeIf { it.fits(old, updated) } ?: EditSpan.between(old, updated)
        var inserted = updated.substring(head, newEnd)
        if (!bridge.pastesAsAttachment(inserted)) {
            // An edit that leaves the draft as it was still answers the
            // composer, which counts on hearing back about every edit.
            if (draftText == updated) publish() else setDraft(updated, EditSpan(head, oldEnd, newEnd))
            return
        }
        var before = updated.substring(0, head)
        var after = updated.substring(newEnd)
        if (inserted.startsWith(' ') && before.lastOrNull()?.isWhitespace() == false) {
            inserted = inserted.substring(1)
            before += " "
        }
        if (inserted.endsWith(' ') && after.firstOrNull()?.isWhitespace() == false) {
            inserted = inserted.dropLast(1)
            after = " $after"
        }
        val nextText = before + after
        // The paste leaves the draft; what it replaced goes, and a space
        // kept to part it from a word stays as the edit's own.
        shiftAttachmentOffsets(old, nextText, EditSpan(head, oldEnd, nextText.length - (old.length - oldEnd)))
        draftText = nextText
        insertPositionedAttachment(
            attachment = DraftAttachment.Text(name = ChatNotices.PASTED_TEXT, text = inserted),
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
                    attachment = DraftAttachment.Text(name = ChatNotices.PASTED_TEXT, text = normalized),
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
     * Marks a picked attachment as being read/prepared before [attach] begins
     * uploading, keeping `uploading > 0` so Send and Resume stay disabled.
     */
    fun beginAttachmentPreparation() {
        uploading += 1
        notice = null
        publish()
    }

    /** Releases a preparation reservation started by [beginAttachmentPreparation]. */
    fun endAttachmentPreparation() {
        if (uploading <= 0) return
        uploading -= 1
        publish()
    }

    /** Runs [block] while keeping `uploading > 0` until handoff, failure, or cancellation. */
    suspend inline fun <T> withAttachmentPreparation(block: () -> T): T {
        beginAttachmentPreparation()
        return try {
            block()
        } finally {
            endAttachmentPreparation()
        }
    }

    /**
     * Checks whether a file of [size] bytes may be attached before reading or
     * uploading it. Refuses oversized files with a notice naming the file and
     * the limit.
     */
    fun admits(name: String, size: Long): Boolean {
        val limit = bridge.attachmentLimit
        if (size <= limit) return true
        val megabytes = limit / (1024L * 1024L)
        notice = ChatNotices.overLimit(name, megabytes)
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
            } catch (c: CancellationException) {
                uploading -= 1
                throw c
            } catch (t: Throwable) {
                uploading -= 1
                notice = ChatNotices.notAttached(name, t.failureDescription)
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
                    notice = ChatNotices.notAttached(name, failure.failureDescription)
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
                        is ActOutcome.Rejected -> notice = ChatNotices.notSent(acted.value)
                        is ActOutcome.Failed -> notice = acted.value
                        ActOutcome.NotConfirmed -> notice = ChatNotices.NOT_CONFIRMED
                    }
                }
                is CommandOutcome.Sent -> {
                    if (composed == typed) clearDraftInternal()
                    setFollowing(true)
                    toNewest += 1
                }
                is CommandOutcome.Refused -> {
                    notice = ChatNotices.refusedCommand(
                        command = outcome.command,
                        offered = outcome.offered,
                        hint = outcome.hint,
                        kind = frame?.kind,
                    )
                }
                is CommandOutcome.Unknown -> {
                    notice = ChatNotices.unknownCommand(
                        command = outcome.command,
                        value = outcome.value,
                        offered = outcome.offered,
                    )
                }
                null -> notice = ChatNotices.CLOSED
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
                is ActOutcome.Rejected -> notice = ChatNotices.notSent(outcome.value)
                is ActOutcome.Failed -> notice = outcome.value
                ActOutcome.NotConfirmed -> notice = ChatNotices.NOT_CONFIRMED
                null -> notice = ChatNotices.CLOSED
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

    private fun act(
        body: suspend (ChatSource) -> ActOutcome?,
        onDone: (() -> Unit)? = null,
    ): Job {
        notice = null
        publish()
        return scope.launch {
            when (val outcome = body(source)) {
                ActOutcome.Done -> onDone?.invoke()
                is ActOutcome.Rejected -> notice = ChatNotices.notSent(outcome.value)
                is ActOutcome.Failed -> notice = outcome.value
                ActOutcome.NotConfirmed -> {
                    if (ask?.state != CardState.NotConfirmed) {
                        notice = ChatNotices.NOT_CONFIRMED
                    }
                }
                null -> notice = ChatNotices.CLOSED
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
        val anchorTrailing = draftText.isNotEmpty() || positionedAttachments.any { it.offset != null }
        // What the composer holds moves past the words put back ahead of
        // it, and past the line break that parts them from its own words.
        val prefixShift = when {
            words.isEmpty() -> 0
            draftText.isEmpty() -> words.length
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
        val restoredAttachments = sent.positionedAttachments().map { entry ->
            if (anchorTrailing && entry.offset == null) {
                entry.copy(offset = words.length)
            } else {
                entry
            }
        }
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

    /**
     * Moves each placed attachment with the words around it as the draft goes
     * from [old] to [updated]: one before the edit stays, one after it moves
     * by what the edit added or took away, and one inside words the edit
     * replaced goes to the end of what replaced them. The edit is the field's
     * own when it has one; only without it is it inferred from the two texts,
     * which cannot tell which of a run of repeated characters was typed.
     */
    private fun shiftAttachmentOffsets(old: String, updated: String, edit: EditSpan? = null) {
        if (positionedAttachments.none { it.offset != null }) return
        val (head, oldEnd, newEnd) = edit?.takeIf { it.fits(old, updated) } ?: EditSpan.between(old, updated)
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
        notice = ChatNotices.notSent(refused.reason)
        source.discard(refused.inputId)
    }

    /**
     * Names what the running activity works on, from the row it runs in. A
     * row in the window is read from its cell; one outside it is asked of the
     * session once per activity, not on every publish.
     */
    private fun refreshActivitySubject() {
        val key = (frame?.composer?.activity?.kind as? ActivityKind.Running)?.key
        if (key == null) {
            activityKey = null
            activitySubject = null
            return
        }
        val row = cells[key]?.row
        if (row == null && key == activityKey) return
        activityKey = key
        activitySubject = (row ?: rowOrNull(key))?.let(ChatWords::subject)
    }

    /** The draft's images this phone already holds, read from the cache and the session. */
    private fun draftImages(): Map<List<Int>, ByteArray> {
        if (positionedAttachments.none { it.attachment is DraftAttachment.Image }) return emptyMap()
        return buildMap {
            for (entry in positionedAttachments) {
                val image = entry.attachment as? DraftAttachment.Image ?: continue
                blob(image.value.hash)?.let { put(image.value.hash, it) }
            }
        }
    }

    private fun publish() {
        refreshActivitySubject()
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
            activitySubject = activitySubject,
            draftImages = draftImages(),
            typedEdits = typedEdits,
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

        /** Lines as the chat counts them: a trailing newline ends a line rather than starting one. */
        fun lines(text: String): Int {
            val parts = text.split('\n')
            val count = if (parts.lastOrNull()?.isEmpty() == true) parts.size - 1 else parts.size
            return count
        }
    }
}
