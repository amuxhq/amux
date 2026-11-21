package sh.amux.testsupport

import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock
import kotlinx.coroutines.CompletableDeferred
import sh.amux.core.runtime.ChatModel
import sh.amux.core.runtime.OpenChat
import sh.amux.core.runtime.RuntimeFailure
import sh.amux.core.runtime.passesThroughRuns
import sh.amux.values.ActOutcome
import sh.amux.values.Activity
import sh.amux.values.AgentKey
import sh.amux.values.AskCard
import sh.amux.values.AskMoved
import sh.amux.values.BackgroundKind
import sh.amux.values.BlobRef
import sh.amux.values.Changes
import sh.amux.values.ChatAgents
import sh.amux.values.ChatDelta
import sh.amux.values.ChatFrame
import sh.amux.values.CommandOutcome
import sh.amux.values.Comparison
import sh.amux.values.Composer
import sh.amux.values.ComposerView
import sh.amux.values.Connection
import sh.amux.values.ContextView
import sh.amux.values.ControlsSummary
import sh.amux.values.Decision
import sh.amux.values.Draft
import sh.amux.values.DraftPart
import sh.amux.values.ExploreVerb
import sh.amux.values.FormValue
import sh.amux.values.Git
import sh.amux.values.GroupState
import sh.amux.values.InputState
import sh.amux.values.JobRow
import sh.amux.values.JobStop
import sh.amux.values.KeysMoved
import sh.amux.values.Kind
import sh.amux.values.Overview
import sh.amux.values.PageOutcome
import sh.amux.values.PermissionChoice
import sh.amux.values.PhaseView
import sh.amux.values.QuestionResponse
import sh.amux.values.QueuedRow
import sh.amux.values.RefusedPrompt
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Run
import sh.amux.values.RunCounts
import sh.amux.values.ProsePart
import sh.amux.values.Segment
import sh.amux.values.SendOutcome
import sh.amux.values.SentPrompt
import sh.amux.values.ServerView
import sh.amux.values.SettingChange
import sh.amux.values.SettingsView
import sh.amux.values.SignInView
import sh.amux.values.SubagentGroup
import sh.amux.values.TasksView
import sh.amux.values.UsageView
import sh.amux.words.EMPTY_GROUP_STATE
import sh.amux.values.SentReviewRef
import sh.amux.values.ReviewPage
import sh.amux.values.ReviewOutcome
import sh.amux.values.ReviewContextAsk
import sh.amux.values.ReviewChoice
import sh.amux.values.ReviewChip
import sh.amux.values.ReviewChange
import sh.amux.values.ReviewAnchor

/**
 * A chat with no native runtime behind it: scripted rows, frame, ask card,
 * overview, settings, review diff, blobs, and recorded acts.
 *
 * Storybook stories and Robolectric Compose tests construct realistic chats
 * through this without loading JNI.
 */
class ScriptedChat(
    rows: List<Row>,
    frame: ChatFrame,
    card: AskCard? = null,
    strip: Surroundings = strip(),
    settings: SettingsView? = null,
    agents: ChatAgents = ChatAgents(
        agents = emptyList(),
        groups = emptyList(),
        live = EMPTY_GROUP_STATE,
    ),
    images: Map<List<Int>, ByteArray> = emptyMap(),
    override val id: Long = 0L,
    keptDraft: Draft? = null,
) : OpenChat {

    /** Facts around a chat's rows: controls, context, sign-in, git, and overview. */
    data class Surroundings(
        val overview: Overview,
        val controls: ControlsSummary,
        val context: ContextView? = null,
        val signIn: SignInView? = null,
        val git: Git? = null,
        val changes: Changes? = null,
    ) {
        internal fun appliedTo(frame: ChatFrame): ChatFrame = frame.copy(
            controls = controls,
            context = context,
            signIn = signIn,
            git = git,
        )
    }

    sealed interface RecordedAnswer {
        data class Choice(val ask: String, val choice: Int, val note: String?) : RecordedAnswer
        data class Questions(val ask: String, val responses: List<QuestionResponse>) : RecordedAnswer
        data class ReplyInstead(
            val ask: String,
            val text: String,
            val soFar: List<QuestionResponse>,
        ) : RecordedAnswer
        data class Form(val ask: String, val values: List<FormValue>) : RecordedAnswer
    }

    private data class Pending(
        val above: MutableList<String> = mutableListOf(),
        val below: MutableList<String> = mutableListOf(),
        var dropped: Int = 0,
        val rows: MutableList<String> = mutableListOf(),
        var reloaded: Boolean = false,
        var session: Boolean = false,
        /** The review page moved. */
        var review: Boolean = false,
    ) {
        fun arrived(keys: List<String>) {
            above.addAll(keys)
        }

        fun droppedKey(key: String) {
            val aboveIndex = above.indexOf(key)
            if (aboveIndex >= 0) {
                above.removeAt(aboveIndex)
                return
            }
            val belowIndex = below.indexOf(key)
            if (belowIndex >= 0) {
                below.removeAt(belowIndex)
                return
            }
            dropped += 1
        }
    }

    private val lock = ReentrantLock()
    private val ordered = rows.toMutableList()
    private var current: ChatFrame = strip.appliedTo(frame)
    private var card: AskCard? = card
    private var facts: Overview = strip.overview
    private var files: Changes? = strip.changes
    private var offered: SettingsView? = settings
    private var agents: ChatAgents = agents
    private val images = images.toMutableMap()
    private val failed = mutableSetOf<List<Int>>()
    private val retried = mutableListOf<List<Int>>()
    private var pending = Pending()
    private var following = true
    private val held = mutableListOf<Row>()
    private val openRuns = mutableSetOf<String>()
    private val openGroups = mutableSetOf<String>()

    private val _sent = mutableListOf<Draft>()
    private val _resumed = mutableListOf<Draft>()
    private val _changed = mutableListOf<SettingChange>()
    private val _stoppedJobs = mutableListOf<String>()
    private val _follows = mutableListOf<Boolean>()
    private val _paged = mutableListOf<Int>()
    private val _answers = mutableListOf<RecordedAnswer>()
    private val _withdrawn = mutableListOf<List<Int>>()
    private val _sentNow = mutableListOf<List<Int>>()
    private val _resent = mutableListOf<List<Int>>()
    private val _discarded = mutableListOf<List<Int>>()
    private val _commands = mutableListOf<Draft>()
    private val _keptDrafts = mutableListOf<Draft>()
    private val _toggledRuns = mutableListOf<String>()
    private val _toggledGroups = mutableListOf<String>()
    private val _storedBlobs = mutableListOf<String>()

    var closed: Boolean = false
        private set

    val sent: List<Draft> get() = lock.withLock { _sent.toList() }
    val resumed: List<Draft> get() = lock.withLock { _resumed.toList() }
    val changed: List<SettingChange> get() = lock.withLock { _changed.toList() }
    val stoppedJobs: List<String> get() = lock.withLock { _stoppedJobs.toList() }
    val follows: List<Boolean> get() = lock.withLock { _follows.toList() }
    val paged: List<Int> get() = lock.withLock { _paged.toList() }
    val answers: List<RecordedAnswer> get() = lock.withLock { _answers.toList() }
    val withdrawn: List<List<Int>> get() = lock.withLock { _withdrawn.toList() }
    val sentNow: List<List<Int>> get() = lock.withLock { _sentNow.toList() }
    val resent: List<List<Int>> get() = lock.withLock { _resent.toList() }
    val discarded: List<List<Int>> get() = lock.withLock { _discarded.toList() }
    val commands: List<Draft> get() = lock.withLock { _commands.toList() }
    val keptDrafts: List<Draft> get() = lock.withLock { _keptDrafts.toList() }
    val toggledRuns: List<String> get() = lock.withLock { _toggledRuns.toList() }
    val toggledGroups: List<String> get() = lock.withLock { _toggledGroups.toList() }
    val storedBlobs: List<String> get() = lock.withLock { _storedBlobs.toList() }

    var interrupts: Int = 0
        private set
    var switchBacks: Int = 0
        private set

    var failedBlobs: Set<List<Int>>
        get() = lock.withLock { failed.toSet() }
        set(value) = lock.withLock {
            failed.clear()
            failed.addAll(value)
        }

    val retriedBlobs: List<List<Int>>
        get() = lock.withLock { retried.toList() }

    var cap: Int = Int.MAX_VALUE
    var pagedOutcome: PageOutcome? = PageOutcome.Arrived(0)
    var holdsPage: Boolean = false
    private var heldPage: CompletableDeferred<Unit>? = null

    /** Optional older rows that [pageOlder] automatically prepends when non-empty. */
    val earlier: MutableList<Row> = mutableListOf()

    var initialKeptDraft: Draft? = keptDraft
    var commandHandler: (Draft) -> CommandOutcome? = { CommandOutcome.Send }
    var actOutcome: ActOutcome? = ActOutcome.Done
    var sendFailure: RuntimeFailure? = null
    var blobFailure: RuntimeFailure? = null
    val refusedWords: MutableMap<List<Int>, String> = mutableMapOf()
    val drafts: MutableMap<List<Int>, Draft> = mutableMapOf()

    override fun close() {
        lock.withLock {
            closed = true
            heldPage?.complete(Unit)
            heldPage = null
        }
    }

    // --- Scripting changes -------------------------------------------------

    fun append(rows: List<Row>) {
        lock.withLock {
            if (following) {
                ordered.addAll(rows)
                pending.arrived(rows.map { it.id })
                trim()
            } else {
                held.addAll(rows)
                current = current.copy(arrivalsHeld = true)
                pending.session = true
            }
        }
    }

    private fun trim() {
        while (ordered.size > cap) {
            pending.droppedKey(ordered.removeAt(0).id)
        }
    }

    fun prepend(rows: List<Row>) {
        lock.withLock {
            ordered.addAll(0, rows)
            pending.below.addAll(0, rows.map { it.id })
        }
    }

    fun revise(rows: List<Row>) {
        lock.withLock {
            for (row in rows) {
                val index = ordered.indexOfFirst { it.id == row.id }
                if (index >= 0) ordered[index] = row
            }
            pending.rows.addAll(rows.map { it.id })
        }
    }

    fun reload(rows: List<Row>) {
        lock.withLock {
            ordered.clear()
            ordered.addAll(rows)
            pending.reloaded = true
            pending.session = true
        }
    }

    fun show(
        frame: ChatFrame? = null,
        card: AskCard? = null,
        clearCard: Boolean = false,
        strip: Surroundings? = null,
        settings: SettingsView? = null,
        agents: ChatAgents? = null,
    ) {
        lock.withLock {
            if (frame != null) current = frame
            if (agents != null) this.agents = agents
            if (clearCard) {
                this.card = null
            } else if (card != null) {
                this.card = card
            }
            if (strip != null) {
                facts = strip.overview
                files = strip.changes
                current = strip.appliedTo(current)
            }
            if (settings != null) offered = settings
            pending.session = true
        }
    }

    fun delta(): ChatDelta? = lock.withLock {
        val taken = pending
        pending = Pending()
        val keys: KeysMoved? = when {
            taken.reloaded -> KeysMoved.Reload(ordered.map { it.id })
            taken.above.isNotEmpty() || taken.below.isNotEmpty() || taken.dropped > 0 ->
                KeysMoved.Edges(
                    above = taken.above.toList(),
                    below = taken.below.toList(),
                    dropped = taken.dropped,
                )
            else -> null
        }
        val wanted = if (taken.reloaded) {
            emptySet()
        } else {
            taken.rows.toSet() - taken.below.toSet() - taken.above.toSet()
        }
        val rows = ordered.filter { it.id in wanted }.map { folding(it, openRuns, openGroups) }
        val review = if (taken.review) reviewShown else null
        if (keys == null && rows.isEmpty() && !taken.session && review == null) return null
        val session = taken.session
        ChatDelta(
            chat = id,
            rows = rows,
            agents = if (session) agents else null,
            ask = if (session) AskMoved(card = card) else null,
            frame = if (session) current else null,
            keys = keys,
            overview = if (session) facts else null,
            review = review,
            settings = if (session) offered else null,
        )
    }

    fun deliver(to: ChatModel) {
        delta()?.let { to.apply(it) }
    }

    fun answerPage() {
        val waiter = lock.withLock {
            holdsPage = false
            heldPage.also { heldPage = null }
        }
        waiter?.complete(Unit)
    }

    // --- ChatSource --------------------------------------------------------

    override fun opening(): ChatDelta = lock.withLock {
        pending = Pending()
        ChatDelta(
            chat = id,
            rows = emptyList(),
            agents = agents,
            ask = AskMoved(card = card),
            frame = current,
            keys = KeysMoved.Reload(ordered.map { it.id }),
            overview = facts,
            review = reviewShown,
            settings = offered,
        )
    }

    override fun follow(following: Boolean) {
        lock.withLock {
            _follows.add(following)
            this.following = following
            if (!following || held.isEmpty()) return
            if (held.size > cap) {
                val combined = (ordered + held).takeLast(cap)
                ordered.clear()
                ordered.addAll(combined)
                pending.reloaded = true
            } else {
                ordered.addAll(held)
                pending.arrived(held.map { it.id })
                trim()
            }
            held.clear()
            current = current.copy(arrivalsHeld = false)
            pending.session = true
        }
    }

    override fun rowsFor(keys: List<String>): List<Row> = lock.withLock {
        val wanted = keys.toSet()
        ordered.filter { it.id in wanted }.map { folding(it, openRuns, openGroups) }
    }

    override suspend fun send(draft: Draft): Result<SendOutcome> = lock.withLock {
        _sent.add(draft)
        sendFailure?.let { return Result.failure(it) }
        Result.success(SendOutcome(inputId = listOf(_sent.size), state = InputState.Sent))
    }

    override suspend fun answer(ask: String, choice: Int, note: String?): ActOutcome? = lock.withLock {
        _answers.add(RecordedAnswer.Choice(ask, choice, note))
        actOutcome
    }

    override suspend fun answerQuestions(
        ask: String,
        responses: List<QuestionResponse>,
    ): ActOutcome? = lock.withLock {
        _answers.add(RecordedAnswer.Questions(ask, responses))
        actOutcome
    }

    override suspend fun replyInstead(
        ask: String,
        text: String,
        soFar: List<QuestionResponse>,
    ): ActOutcome? = lock.withLock {
        _answers.add(RecordedAnswer.ReplyInstead(ask, text, soFar))
        actOutcome
    }

    override suspend fun answerForm(ask: String, values: List<FormValue>): ActOutcome? = lock.withLock {
        _answers.add(RecordedAnswer.Form(ask, values))
        actOutcome
    }

    override fun toggleRun(member: String) {
        lock.withLock {
            _toggledRuns.add(member)
            val last = ordered.firstOrNull { it.id == member }?.run?.last ?: return
            if (!openRuns.remove(last)) openRuns.add(last)
            pending.rows.addAll(ordered.filter { it.run?.last == last }.map { it.id })
        }
    }

    override fun toggleGroup(member: String) {
        lock.withLock {
            _toggledGroups.add(member)
            val first = ordered.firstOrNull { it.id == member }?.group?.first ?: return
            if (!openGroups.remove(first)) openGroups.add(first)
            pending.rows.addAll(ordered.filter { it.group?.first == first }.map { it.id })
        }
    }

    override fun foldTurns(on: Boolean) {}

    override fun toggleTurn(end: String) {}

    override suspend fun withdraw(inputId: List<Int>): ActOutcome? = lock.withLock {
        _withdrawn.add(inputId)
        actOutcome
    }

    override fun draftOf(inputId: List<Int>): Draft? = lock.withLock {
        drafts[inputId]?.let { return it }
        refusedWords[inputId]?.let { return Draft(listOf(DraftPart.Text(it))) }
        // A queued prompt holds its words as typed; one on its way holds them
        // parsed, with their source beside the blocks.
        val words = current.queue.firstOrNull { it.inputId == inputId }?.text
            ?.mapNotNull { (it as? Segment.Text)?.value }
            ?: current.underway.firstOrNull { it.inputId == inputId }?.text
                ?.mapNotNull { (it as? ProsePart.Text)?.value?.source }
            ?: return null
        val joined = words.joinToString(separator = "")
        Draft(if (joined.isEmpty()) emptyList() else listOf(DraftPart.Text(joined)))
    }

    override suspend fun sendNow(inputId: List<Int>): ActOutcome? = lock.withLock {
        _sentNow.add(inputId)
        actOutcome
    }

    override suspend fun resend(inputId: List<Int>): SendOutcome? = lock.withLock {
        _resent.add(inputId)
        null
    }

    override fun discard(inputId: List<Int>) {
        lock.withLock {
            _discarded.add(inputId)
            current = current.copy(
                underway = current.underway.filterNot { it.inputId == inputId },
                refused = current.refused.filterNot { it.inputId == inputId },
            )
            pending.session = true
        }
    }

    override suspend fun interrupt(): ActOutcome? = lock.withLock {
        interrupts += 1
        actOutcome
    }

    override suspend fun stopJob(key: String): ActOutcome? = lock.withLock {
        _stoppedJobs.add(key)
        actOutcome
    }

    override suspend fun changeSetting(setting: SettingChange): ActOutcome? = lock.withLock {
        _changed.add(setting)
        actOutcome
    }

    override suspend fun resume(draft: Draft): ActOutcome? = lock.withLock {
        _resumed.add(draft)
        actOutcome
    }

    override suspend fun pageOlder(rows: Int): PageOutcome? {
        val gate = lock.withLock {
            _paged.add(rows)
            if (holdsPage) {
                CompletableDeferred<Unit>().also { heldPage = it }
            } else {
                null
            }
        }
        gate?.await()
        return lock.withLock {
            if (earlier.isNotEmpty() && pagedOutcome is PageOutcome.Arrived) {
                val count = rows.coerceAtMost(earlier.size)
                val page = earlier.subList(earlier.size - count, earlier.size).toList()
                earlier.subList(earlier.size - count, earlier.size).clear()
                ordered.addAll(0, page)
                pending.below.addAll(0, page.map { it.id })
                PageOutcome.Arrived(page.size)
            } else {
                pagedOutcome
            }
        }
    }

    override suspend fun putBlob(data: ByteArray, name: String, mime: String): Result<BlobRef> = lock.withLock {
        _storedBlobs.add(name)
        blobFailure?.let { return Result.failure(it) }
        val blob = BlobRef(
            hash = name.encodeToByteArray().map { it.toInt() and 0xFF },
            name = name,
            mime = mime,
            size = data.size.toLong(),
        )
        images[blob.hash] = data
        Result.success(blob)
    }

    override fun blob(hash: List<Int>): ByteArray? = lock.withLock { images[hash] }

    override fun blobFailed(hash: List<Int>): Boolean = lock.withLock { hash in failed }

    override fun fetchBlob(hash: List<Int>) {
        lock.withLock { retried.add(hash) }
    }

    override suspend fun openOverview(comparison: Comparison): Result<Overview> = lock.withLock {
        facts = facts.copy(changes = files)
        Result.success(facts)
    }

    override suspend fun command(draft: Draft): CommandOutcome? = lock.withLock {
        _commands.add(draft)
        commandHandler(draft)
    }

    override suspend fun switchBack(): ActOutcome? = lock.withLock {
        switchBacks += 1
        actOutcome
    }

    // --- The review page ---------------------------------------------------

    /** The page opening the review shows. The script's page never composes itself. */
    var reviewOnOpening: ReviewPage?
        get() = lock.withLock { scriptedReview }
        set(value) = lock.withLock { scriptedReview = value }

    /** Where any selection's comment goes, while a page is open. */
    var anchorForAnySelection: ReviewAnchor?
        get() = lock.withLock { anchoring }
        set(value) = lock.withLock { anchoring = value }

    /** What every act on the review answers. */
    var reviewAnswer: ReviewOutcome? = ReviewOutcome.Done

    /** Holds every act on the review until [answerReview], for a test that watches one waiting. */
    var holdsReview: Boolean = false
    private var heldReview: CompletableDeferred<Unit>? = null

    private var scriptedReview: ReviewPage? = null
    private var anchoring: ReviewAnchor? = null

    /** The page open on the chat, as handed over last. */
    private var reviewShown: ReviewPage? = null
    private val _reviewOpens = mutableListOf<Pair<String?, Boolean>>()
    private val _reviewEdits = mutableListOf<List<ReviewChange>>()
    private val _reviewFetches = mutableListOf<List<String>>()
    private val _reviewContexts = mutableListOf<ReviewContextAsk>()
    private val _reviewChoices = mutableListOf<ReviewChoice>()
    private val _sentWithReview = mutableListOf<Draft>()
    private val _roundsOpened = mutableListOf<SentReviewRef>()
    private var scriptedRound: ReviewPage? = null

    /** Each open of the review, as its base branch and whether it refreshed. */
    val reviewOpens: List<Pair<String?, Boolean>> get() = lock.withLock { _reviewOpens.toList() }
    val reviewEdits: List<List<ReviewChange>> get() = lock.withLock { _reviewEdits.toList() }
    val reviewFetches: List<List<String>> get() = lock.withLock { _reviewFetches.toList() }
    val reviewContexts: List<ReviewContextAsk> get() = lock.withLock { _reviewContexts.toList() }
    val reviewChoices: List<ReviewChoice> get() = lock.withLock { _reviewChoices.toList() }
    val sentWithReview: List<Draft> get() = lock.withLock { _sentWithReview.toList() }
    val roundsOpened: List<SentReviewRef> get() = lock.withLock { _roundsOpened.toList() }
    var reviewCloses: Int = 0
        private set

    /** What opening a sent review's round shows: null for a host that no longer holds it. */
    var roundOnOpening: ReviewPage?
        get() = lock.withLock { scriptedRound }
        set(value) = lock.withLock { scriptedRound = value }

    /**
     * A page already open on the chat, as a model opening on the chat finds
     * it: for a story drawn with no open to wait for.
     */
    fun openedReview(page: ReviewPage) {
        lock.withLock {
            scriptedReview = page
            reviewShown = page
        }
    }

    /** The runtime composed the open page anew: the next delta carries it. */
    fun showReview(page: ReviewPage) {
        lock.withLock {
            scriptedReview = page
            reviewShown = page
            pending.review = true
        }
    }

    fun answerReview() {
        val waiter = lock.withLock {
            holdsReview = false
            heldReview.also { heldReview = null }
        }
        waiter?.complete(Unit)
    }

    private suspend fun reviewAnswered(): ReviewOutcome? {
        val waiter = lock.withLock {
            if (!holdsReview) return@withLock null
            heldReview ?: CompletableDeferred<Unit>().also { heldReview = it }
        }
        waiter?.await()
        return reviewAnswer
    }

    override suspend fun sendWithReview(draft: Draft): Result<SendOutcome> {
        lock.withLock { _sentWithReview.add(draft) }
        sendFailure?.let { return Result.failure(it) }
        return Result.success(SendOutcome(inputId = listOf(1), state = InputState.Sent))
    }

    override suspend fun openReview(baseBranch: String?, refresh: Boolean): ReviewOutcome? {
        lock.withLock {
            _reviewOpens.add(baseBranch to refresh)
            reviewShown = scriptedReview
            pending.review = scriptedReview != null
        }
        return reviewAnswered()
    }

    override suspend fun reviewFiles(paths: List<String>): ReviewOutcome? {
        lock.withLock { _reviewFetches.add(paths) }
        return reviewAnswered()
    }

    override suspend fun reviewContext(ask: ReviewContextAsk): ReviewOutcome? {
        lock.withLock { _reviewContexts.add(ask) }
        return reviewAnswered()
    }

    override suspend fun editReview(changes: List<ReviewChange>): ReviewOutcome? {
        lock.withLock { _reviewEdits.add(changes) }
        return reviewAnswered()
    }

    override fun chooseInReview(choice: ReviewChoice) {
        lock.withLock { _reviewChoices.add(choice) }
    }

    override fun reviewPage(): ReviewPage? = lock.withLock { reviewShown }

    override fun reviewAnchor(from: Int, to: Int): ReviewAnchor? =
        lock.withLock { if (reviewShown == null) null else anchoring }

    override fun closeReview() {
        lock.withLock {
            reviewShown = null
            reviewCloses += 1
        }
    }

    override suspend fun reviewRound(sent: SentReviewRef): ReviewPage? = lock.withLock {
        _roundsOpened.add(sent)
        scriptedRound
    }

    override fun keptDraft(): Draft? = lock.withLock { initialKeptDraft }

    override fun keep(draft: Draft) {
        lock.withLock { _keptDrafts.add(draft) }
    }

    companion object {
        val agent: AgentKey = AgentKey(
            host = List(16) { 1 },
            agent = List(16) { 2 },
        )

        fun frame(
            name: String = "refactor-auth",
            kind: Kind = Kind.ClaudeSdk,
            phase: PhaseView = PhaseView.Idle,
            mode: Composer = Composer.Send,
            activity: Activity? = null,
            caughtUp: Boolean = true,
            hasOlder: Boolean = false,
            queue: List<QueuedRow> = emptyList(),
            underway: List<SentPrompt> = emptyList(),
            refused: List<RefusedPrompt> = emptyList(),
            switchedAway: Boolean = false,
            ended: String? = null,
            reviewChip: ReviewChip? = null,
        ): ChatFrame = ChatFrame(
            agent = agent,
            name = name,
            kind = kind,
            phase = phase,
            composer = ComposerView(
                mode = mode,
                switchedAway = switchedAway,
                activity = activity,
            ),
            controls = ControlsSummary(
                effort = null,
                mode = null,
                model = null,
                permission = null,
            ),
            connection = Connection.Live,
            caughtUp = caughtUp,
            hasOlder = hasOlder,
            arrivalsHeld = false,
            queue = queue,
            underway = underway,
            refused = refused,
            askInput = null,
            context = null,
            ended = ended,
            git = null,
            reviewChip = reviewChip,
            signIn = null,
        )

        fun strip(
            tasks: TasksView? = null,
            context: ContextView? = null,
            model: String? = null,
            effort: String? = null,
            mode: String? = null,
            usage: UsageView? = null,
            failedServers: List<ServerView> = emptyList(),
            signIn: SignInView? = null,
            background: Int? = null,
            git: Git? = null,
            changes: Changes? = null,
        ): Surroundings {
            val jobs = List(background ?: 0) { index ->
                JobRow(
                    command = "job ${index + 1}",
                    startedAtMs = 0L,
                    kind = BackgroundKind.Shell,
                    stop = JobStop.Unavailable,
                    step = null,
                )
            }
            return Surroundings(
                overview = Overview(
                    jobs = jobs,
                    failedServers = failedServers,
                    changes = null,
                    tasks = tasks,
                    usageNearLimit = usage,
                ),
                controls = ControlsSummary(
                    effort = effort,
                    mode = null,
                    model = model,
                    permission = mode?.let(::unusualPermission),
                ),
                context = context,
                signIn = signIn,
                git = git,
                changes = changes,
            )
        }

        private fun unusualPermission(value: String): PermissionChoice? {
            val names = mapOf(
                "acceptEdits" to "Accept edits",
                "plan" to "Plan",
                "auto" to "Auto",
                "bypassPermissions" to "Bypass permissions",
                "read-only" to "Read only",
                "full-access" to "Full access",
            )
            val displayName = names[value] ?: return null
            return PermissionChoice(
                value = value,
                displayName = displayName,
                custom = false,
                current = true,
                unlisted = false,
                normal = false,
                neverAsks = value == "bypassPermissions" || value == "full-access",
                settable = true,
            )
        }

        fun row(
            id: String,
            order: Long,
            kind: RowKind,
            attention: Boolean = false,
            decision: Decision? = null,
            run: Run? = null,
            parent: String? = null,
            collapsed: Boolean = false,
            opens: List<Int>? = null,
            group: SubagentGroup? = null,
        ): Row = Row(
            id = id,
            order = order,
            atMs = 1_790_000_000_000L + order * 1_000L,
            kind = kind,
            collapsed = collapsed || parent != null,
            attention = attention,
            decision = decision,
            group = group,
            opens = opens,
            parent = parent,
            run = run,
            turn = null,
        )

        /** Folds calls of one turn that opened agents into one [SubagentGroup]. */
        fun group(rows: List<Row>): List<Row> {
            val calls = rows.filter { it.opens != null }
            val first = calls.firstOrNull() ?: return rows
            val agents = mutableListOf<List<Int>>()
            for (call in calls) {
                val target = call.opens!!
                if (target !in agents) agents.add(target)
            }
            val group = SubagentGroup(first = first.id, agents = agents, open = false)
            return rows.map { row ->
                if (row.opens != null) row.copy(group = group) else row
            }
        }

        /** Stamps [Run] metadata onto tool-step rows matching the shared view fold. */
        fun run(
            rows: List<Row>,
            live: Boolean = false,
            openBelow: Boolean = false,
            failed: Set<String> = emptySet(),
        ): List<Row> {
            val steps = rows.filter { !it.kind.passesThroughRuns }
            val first = steps.firstOrNull() ?: return rows
            val last = steps.lastOrNull() ?: return rows
            val total = steps.size
            val hidden = if (steps.size > 1) counted(steps.dropLast(1)) else null
            return rows.map { row ->
                val index = steps.indexOfFirst { it.id == row.id }.takeIf { it >= 0 }
                val after = index?.let { total - 1 - it }
                val newest = row.id == last.id
                row.copy(
                    run = Run(
                        first = first.id,
                        last = last.id,
                        steps = total,
                        live = live,
                        openBelow = openBelow,
                        liveShown = live && newest,
                        unresolvedFailure = row.id in failed,
                        open = false,
                        counts = if (newest) counted(steps) else null,
                        hidden = if (live && newest) hidden else null,
                        recent = after?.takeIf { it < 2 },
                    ),
                )
            }
        }

        internal fun folding(
            row: Row,
            open: Set<String>,
            groups: Set<String> = emptySet(),
        ): Row {
            row.group?.let { grp ->
                val updatedGroup = grp.copy(open = grp.first in groups)
                return row.copy(
                    group = updatedGroup,
                    collapsed = row.collapsed || (!updatedGroup.open && row.id != updatedGroup.first),
                )
            }
            val run = row.run ?: return row
            val shown = if (run.live) {
                run.liveShown
            } else {
                run.recent == 0 || run.unresolvedFailure
            }
            val updatedRun = run.copy(open = run.last in open)
            return row.copy(
                run = updatedRun,
                collapsed = row.collapsed || (!shown && !updatedRun.open),
            )
        }

        private fun counted(steps: List<Row>): RunCounts {
            var commands = 0
            var edits = 0
            var reads = 0
            var searches = 0
            var lists = 0
            var fetches = 0
            var subagents = 0
            var other = 0
            val editedPaths = mutableSetOf<String>()
            val readSubjects = mutableSetOf<String>()
            for (step in steps) {
                when (val k = step.kind) {
                    is RowKind.Command, is RowKind.Background -> commands += 1
                    is RowKind.FileChange -> {
                        for (file in k.files) {
                            if (editedPaths.add(file.path)) edits += 1
                        }
                    }
                    is RowKind.Explore -> when (k.verb) {
                        ExploreVerb.Read -> if (readSubjects.add(k.subject)) reads += 1
                        ExploreVerb.Search, ExploreVerb.WebSearch -> searches += 1
                        ExploreVerb.List -> lists += 1
                        ExploreVerb.Fetch -> fetches += 1
                    }
                    is RowKind.Subagent, is RowKind.Child -> subagents += 1
                    RowKind.Hidden -> Unit
                    else -> other += 1
                }
            }
            return RunCounts(
                commands = commands,
                edits = edits,
                reads = reads,
                searches = searches,
                lists = lists,
                fetches = fetches,
                subagents = subagents,
                other = other,
            )
        }
    }
}
