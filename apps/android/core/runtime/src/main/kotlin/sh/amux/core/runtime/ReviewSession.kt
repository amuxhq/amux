package sh.amux.core.runtime

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import sh.amux.values.ReviewAnchor
import sh.amux.values.ReviewBanner
import sh.amux.values.ReviewChange
import sh.amux.values.ReviewChoice
import sh.amux.values.ReviewContextAsk
import sh.amux.values.ReviewOutcome
import sh.amux.values.ReviewPage
import sh.amux.values.ReviewView
import sh.amux.words.ChatWords

/** What a review page draws: the page as the runtime handed it last, and what the person is doing on it. */
data class ReviewSessionState(
    val page: ReviewPage = ReviewSession.OPENING,
    /** The rows selected, first to last, by index into [ReviewPage.rows]. */
    val selected: IntRange? = null,
    /**
     * Where a comment on the selection goes; null with nothing selected,
     * and for a selection with no line or across files.
     */
    val anchor: ReviewAnchor? = null,
    /** Acts on their way to the agent's host. */
    val waiting: Int = 0,
    /** Why the last act came to nothing, when the page does not say it itself; cleared by the next act. */
    val notice: String? = null,
    /** The page was closed: nothing more is asked of the host for it. */
    val closed: Boolean = false,
) {
    fun isSelected(row: Int): Boolean = selected?.contains(row) == true

    /** A comment can be written on what is selected. */
    val canComment: Boolean
        get() = anchor != null && !closed
}

/**
 * The agent's review page as the Android app draws it: the page the shared
 * runtime composes, handed over whole each time it moves, and what the
 * person does on it, sent to the agent's host.
 *
 * Nothing here works the page out again. Its rows, banners and header are
 * drawn as handed; what the page still needs is read from its `toFetch` and
 * asked for; and a selection of rows becomes the lines a comment is on
 * through the runtime, which counts the rows of the page it handed over
 * last. A selection is held by its rows' keys, which name a row across
 * compositions, so a page that moves under it keeps it on the same rows or
 * drops it when they go.
 *
 * The chat model owns one while its page is open and hands it every page
 * the chat's deltas carry. Each act returns the job that waits on the
 * agent's host, which answers what it came to; a failure the page itself
 * does not say is put into [ReviewSessionState.notice]. Every call is made
 * on the main thread, where [scope] runs: the acts suspend on the core
 * thread's answer, and the few reads that do not (the page, an anchor) are
 * made from events, never while a screen is composed.
 */
class ReviewSession(
    val source: ChatSource,
    private val scope: CoroutineScope,
) {
    private val _state = MutableStateFlow(ReviewSessionState(page = source.reviewPage() ?: OPENING))
    val state: StateFlow<ReviewSessionState> = _state.asStateFlow()

    val page: ReviewPage get() = _state.value.page
    val closed: Boolean get() = _state.value.closed

    /** Where the selection started and the row it reaches, by key. */
    private var from: String? = null
    private var to: String? = null

    /** Each row's index in the page, by key. */
    private var places: Map<String, Int> = emptyMap()

    /**
     * The files asked for, each as the comparison (the commits and view) and
     * path its body is read for, so a page that moves before they land asks
     * nothing twice. A fetch that failed stays here until the page is opened
     * again or the host can be reached again, so a failing host is not asked
     * in a loop.
     */
    private val asked = mutableSetOf<Asked>()

    /** The newest act, so a test can wait for its answer rather than guess how long the host takes. */
    internal var latest: Job? = null
        private set

    init {
        placeRows()
    }

    private inline fun update(change: (ReviewSessionState) -> ReviewSessionState) {
        _state.value = change(_state.value)
    }

    // --- Following the chat ------------------------------------------------

    /**
     * The page as the runtime handed it now, from the chat's delta. The
     * selection is kept on its rows, and the files the page still needs are
     * asked for.
     */
    internal fun apply(page: ReviewPage) {
        val unreachable = ReviewBanner.Unreachable in this.page.banners
        if (page != this.page) update { it.copy(page = page) }
        placeRows()
        // The host answers again: what failed to come from it is asked for
        // once more.
        if (unreachable && ReviewBanner.Unreachable !in page.banners) asked.clear()
        if (from != null) resolve()
        fetchWanted()
    }

    private fun placeRows() {
        val placed = HashMap<String, Int>(page.rows.size)
        page.rows.forEachIndexed { index, row -> placed.putIfAbsent(row.key, index) }
        places = placed
    }

    // --- Opening -----------------------------------------------------------

    /**
     * Opens the review at the agent's host for this look: on a new snapshot
     * when the tree changed and no drafts wait, else on the drafts' own,
     * which the page says has changed since. [base] picks the branch the
     * change is counted against anew.
     */
    fun open(base: String? = null): Job? = reopen(base = base, refresh = false)

    /** Takes a new snapshot even with drafts, moving them onto it; the page says how many moved. */
    fun refresh(): Job? = reopen(base = null, refresh = true)

    private fun reopen(base: String?, refresh: Boolean): Job? {
        asked.clear()
        return act(changing = false) { it.openReview(baseBranch = base, refresh = refresh) }
    }

    // --- Reading more ------------------------------------------------------

    /**
     * Asks for every file the page lists as needed and not yet asked for, in
     * page order, which is reading order: the runtime asks the host in
     * batches and the page moves as each lands, the first on screen first.
     */
    private fun fetchWanted() {
        val header = page.header
        if (closed || header == null) return
        val keys = page.toFetch
            .map { Asked(from = header.comparedWith, snapshot = header.snapshot, view = header.view, path = it) }
            .filter { it !in asked }
        if (keys.isEmpty()) return
        asked += keys
        latest = scope.launch {
            when (val outcome = source.reviewFiles(keys.map { it.path })) {
                // Fetched for whichever view the runtime showed when they
                // were asked, which a choice may have moved since: one a
                // later page still needs is asked for again. The page that
                // says what landed came before this answer, so nothing is
                // asked again for it now.
                ReviewOutcome.Done -> asked -= keys.toSet()
                // The host's own words. A host out of reach, or a review
                // that cannot be had, the page's banners say instead.
                is ReviewOutcome.Failed -> update { it.copy(notice = ChatWords.reviewNotice(outcome)) }
                else -> Unit
            }
        }
    }

    /** A file's body as asked for: the comparison it is read for. */
    private data class Asked(
        val from: String,
        val snapshot: String,
        val view: ReviewView,
        val path: String,
    )

    /** Loads the lines one of a gap's asks names; the gap says it is loading until they arrive. */
    fun moreContext(ask: ReviewContextAsk): Job? = act(changing = false) { it.reviewContext(ask) }

    // --- Choosing ----------------------------------------------------------

    /** Switches to one of the views the header offers. */
    fun show(view: ReviewView) = choose(ReviewChoice.View(view))

    /** Folds an open file or opens a folded one. */
    fun toggleFile(path: String) = choose(ReviewChoice.ToggleFile(path))

    /** Opens a folded outdated comment, or folds it again. */
    fun toggleComment(id: List<Int>) = choose(ReviewChoice.ToggleComment(id))

    private fun choose(choice: ReviewChoice) {
        if (closed) return
        source.chooseInReview(choice)
    }

    // --- Selecting ---------------------------------------------------------

    /** Starts a selection on one row: a line, or a file's head for a comment on the whole file. */
    fun select(row: Int) {
        val key = page.rows.getOrNull(row)?.key ?: return
        from = key
        to = key
        resolve()
    }

    /** Extends the selection to the row reached; across files it holds no lines, and nothing can be written on it. */
    fun extend(to: Int) {
        if (from == null) return
        this.to = page.rows.getOrNull(to)?.key ?: return
        resolve()
    }

    fun clearSelection() {
        from = null
        to = null
        val now = _state.value
        if (now.selected != null || now.anchor != null) update { it.copy(selected = null, anchor = null) }
    }

    /**
     * The selection as indices into the page handed over last, and the lines
     * the runtime says they are; a row that left the page ends it.
     */
    private fun resolve() {
        val first = from?.let(places::get)
        val last = to?.let(places::get)
        if (first == null || last == null) {
            clearSelection()
            return
        }
        val range = minOf(first, last)..maxOf(first, last)
        val anchor = source.reviewAnchor(from = first, to = last)
        val now = _state.value
        if (now.selected != range || now.anchor != anchor) update { it.copy(selected = range, anchor = anchor) }
    }

    // --- Commenting --------------------------------------------------------

    /** Writes a draft on the selection; once the host has it the selection ends. Blank words write nothing. */
    fun comment(text: String): Job? {
        val words = text.trim()
        val anchor = _state.value.anchor
        if (words.isEmpty() || anchor == null || closed) return null
        val selection = from to to
        return act(done = { if ((from to to) == selection) clearSelection() }) {
            it.editReview(listOf(ReviewChange.Comment(anchor = anchor, text = words)))
        }
    }

    /** New words for a comment, draft or sent; it stays where it was written. Blank words change nothing. */
    fun reword(id: List<Int>, text: String): Job? {
        val words = text.trim()
        if (words.isEmpty()) return null
        return act { it.editReview(listOf(ReviewChange.Reword(id = id, text = words))) }
    }

    /** Deletes a comment, draft or sent; the chat keeps a sent one. */
    fun delete(id: List<Int>): Job? = act { it.editReview(listOf(ReviewChange.Delete(id))) }

    /** Deletes every draft, leaving what was sent, once the person said so. */
    fun discardDrafts(): Job? = act { it.editReview(listOf(ReviewChange.DiscardDrafts)) }

    /** Deletes the review, once the person said so: the next look is a first look. */
    fun startOver(): Job? = act { it.editReview(listOf(ReviewChange.StartOver)) }

    // --- Closing -----------------------------------------------------------

    /**
     * The page left the screen: the chat stops following the review for it.
     * Acts already on their way are answered all the same.
     */
    fun close() {
        if (closed) return
        clearSelection()
        update { it.copy(closed = true) }
        source.closeReview()
    }

    // --- Acting ------------------------------------------------------------

    /**
     * Starts an act on the agent's host; what it moved arrives in the chat's
     * deltas before its answer does. An act that changes nothing says no
     * more of a host out of reach than the page's banner does.
     */
    private fun act(
        changing: Boolean = true,
        done: (() -> Unit)? = null,
        body: suspend (ChatSource) -> ReviewOutcome?,
    ): Job? {
        if (closed) return null
        update { it.copy(notice = null, waiting = it.waiting + 1) }
        val job = scope.launch {
            val outcome = try {
                body(source)
            } finally {
                update { it.copy(waiting = it.waiting - 1) }
            }
            if (outcome == ReviewOutcome.Done) done?.invoke()
            if (changing || outcome != ReviewOutcome.Unreachable) {
                update { it.copy(notice = ChatWords.reviewNotice(outcome)) }
            }
        }
        latest = job
        return job
    }

    companion object {
        /** What the runtime draws before the review's summary arrives: the page opening, with nothing wrong. */
        val OPENING: ReviewPage = ReviewPage(
            banners = emptyList(),
            rows = emptyList(),
            toFetch = emptyList(),
            opening = true,
            empty = null,
            header = null,
        )
    }
}
