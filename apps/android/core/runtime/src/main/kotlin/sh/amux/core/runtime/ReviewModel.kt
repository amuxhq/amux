package sh.amux.core.runtime

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import sh.amux.values.Comparison
import sh.amux.values.DiffLine
import sh.amux.values.DraftAttachment
import sh.amux.values.FrozenReview
import sh.amux.values.LineKind
import sh.amux.values.ReviewComment
import sh.amux.values.ReviewDoc

/**
 * Where a diff line sits in a review document: its file, its hunk, and its
 * place in that hunk, ordered as the page draws them.
 */
data class ReviewLine(
    val file: Int,
    val hunk: Int,
    val line: Int,
) : Comparable<ReviewLine> {
    override fun compareTo(other: ReviewLine): Int {
        if (file != other.file) return file.compareTo(other.file)
        if (hunk != other.hunk) return hunk.compareTo(other.hunk)
        return line.compareTo(other.line)
    }
}

/**
 * Where a comment is drawn: under one of a file's lines, or on the file itself
 * when [line] is null.
 */
data class CommentPlace(
    val file: Int,
    val line: ReviewLine? = null,
)

/** Observable snapshot of a review page in progress. */
data class ReviewUiState(
    val comparison: Comparison = Comparison.Uncommitted,
    val doc: ReviewDoc,
    val comments: List<ReviewComment> = emptyList(),
    val placed: Map<CommentPlace, List<Int>> = emptyMap(),
    val activeFile: Int = 0,
    val folded: Set<Int> = emptySet(),
    val anchor: ReviewLine? = null,
    val head: ReviewLine? = null,
    val loading: Boolean = false,
    val error: String? = null,
) {
    /** The selected lines, first to last, or null when nothing is selected. */
    val selection: ClosedRange<ReviewLine>?
        get() {
            val a = anchor ?: return null
            val h = head ?: return null
            return if (a <= h) a..h else h..a
        }

    /** Comments written so far, across every file. */
    val count: Int
        get() = comments.size

    /** Whether the review can be attached: at least one comment and nothing half-selected. */
    val canAttach: Boolean
        get() = comments.isNotEmpty() && selection == null

    fun isSelected(at: ReviewLine): Boolean = selection?.contains(at) == true

    fun countInFile(index: Int): Int {
        val path = doc.files.getOrNull(index)?.path ?: return 0
        return comments.count { it.path == path }
    }
}

/**
 * One review page: an agent's working-tree or branch diff frozen when the page
 * opened, the comments written on it, the active file, and the lines selected
 * for the next comment.
 *
 * Files, counts, and hunks are parsed from the frozen patch in Rust via
 * [Bridge.reviewDoc], and every comment change re-parses so Android and the
 * terminal agree on where each comment sits.
 */
class ReviewModel(
    review: FrozenReview,
    comparison: Comparison = Comparison.Uncommitted,
    initialComments: List<ReviewComment> = emptyList(),
    private val renderDoc: (FrozenReview, List<ReviewComment>) -> ReviewDoc? = Bridge::reviewDoc,
    initialDoc: ReviewDoc? = null,
) {
    var review: FrozenReview = review
        private set

    private val _state: MutableStateFlow<ReviewUiState>
    val state: StateFlow<ReviewUiState>

    init {
        val parsed = initialDoc
            ?: renderDoc(review, initialComments)
            ?: ReviewDoc(
                base = "",
                head = review.diff.head,
                files = emptyList(),
                added = 0,
                removed = 0,
            )
        val start = ReviewUiState(
            comparison = comparison,
            doc = parsed,
            comments = initialComments.toList(),
            placed = places(initialComments, parsed),
            activeFile = 0,
        )
        _state = MutableStateFlow(start)
        state = _state.asStateFlow()
    }

    val snapshot: ReviewUiState get() = _state.value
    val comparison: Comparison get() = _state.value.comparison
    val doc: ReviewDoc get() = _state.value.doc
    val comments: List<ReviewComment> get() = _state.value.comments
    val placed: Map<CommentPlace, List<Int>> get() = _state.value.placed
    val activeFile: Int get() = _state.value.activeFile
    val folded: Set<Int> get() = _state.value.folded
    val anchor: ReviewLine? get() = _state.value.anchor
    val head: ReviewLine? get() = _state.value.head
    val selection: ClosedRange<ReviewLine>? get() = _state.value.selection
    val count: Int get() = _state.value.count
    val canAttach: Boolean get() = _state.value.canAttach

    /** The draft token for this review: the frozen diff and every comment. */
    val attachment: DraftAttachment
        get() = DraftAttachment.Review(diff = review.diff, comments = _state.value.comments)

    // --- Reading -----------------------------------------------------------

    fun line(at: ReviewLine): DiffLine? {
        val file = _state.value.doc.files.getOrNull(at.file) ?: return null
        val hunk = file.hunks.getOrNull(at.hunk) ?: return null
        return hunk.lines.getOrNull(at.line)
    }

    fun isSelected(at: ReviewLine): Boolean = _state.value.isSelected(at)

    fun countInFile(index: Int): Int = _state.value.countInFile(index)

    /**
     * What a heading calls a file: its basename, or its parent folder and name
     * when another changed file shares that basename.
     */
    fun labelOfFile(index: Int): String {
        val files = _state.value.doc.files
        val path = files.getOrNull(index)?.path ?: return ""
        val name = path.substringAfterLast('/')
        val shared = files.indices.any { other ->
            other != index && files[other].path.substringAfterLast('/') == name
        }
        if (!shared) return name
        val dir = path.substringBeforeLast('/', "")
        val parent = dir.substringAfterLast('/')
        return if (parent.isEmpty()) name else "$parent/$name"
    }

    // --- Acting ------------------------------------------------------------

    fun selectFile(index: Int) {
        if (index !in _state.value.doc.files.indices) return
        _state.value = _state.value.copy(activeFile = index)
    }

    fun toggleFile(index: Int) {
        val current = _state.value.folded
        val updated = if (index in current) current - index else current + index
        _state.value = _state.value.copy(folded = updated)
    }

    /** Starts a selection on one line. */
    fun begin(at: ReviewLine) {
        if (line(at) == null) return
        _state.value = _state.value.copy(anchor = at, head = at, activeFile = at.file)
    }

    /** Extends the selection to a line in the same file. */
    fun extend(to: ReviewLine) {
        val start = _state.value.anchor ?: return
        if (to.file != start.file || line(to) == null) return
        _state.value = _state.value.copy(head = to)
    }

    fun clearSelection() {
        if (_state.value.anchor == null && _state.value.head == null) return
        _state.value = _state.value.copy(anchor = null, head = null)
    }

    /**
     * Adds a comment on the line the selection ends on and clears the
     * selection. Blank text adds nothing.
     */
    fun comment(text: String): Boolean {
        val trimmed = text.trim()
        val last = _state.value.selection?.endInclusive ?: return false
        val diffLine = line(last) ?: return false
        if (trimmed.isEmpty()) return false
        val newLineVal = diffLine.newLine
        val oldLineVal = diffLine.oldLine
        val newLine: Int
        val oldLine: Int
        when {
            newLineVal != null -> {
                newLine = newLineVal
                oldLine = 0
            }
            oldLineVal != null -> {
                newLine = 0
                oldLine = oldLineVal
            }
            else -> {
                newLine = 0
                oldLine = 0
            }
        }
        val path = _state.value.doc.files[last.file].path
        val updated = _state.value.comments + ReviewComment(
            path = path,
            line = newLine,
            oldLine = oldLine,
            text = trimmed,
        )
        relayout(updated, clearSelection = true)
        return true
    }

    /**
     * Adds a comment on the file as a whole (`line = 0, oldLine = 0`).
     * Blank text or an invalid file index adds nothing.
     */
    fun commentOnFile(fileIndex: Int, text: String): Boolean {
        val trimmed = text.trim()
        val file = _state.value.doc.files.getOrNull(fileIndex) ?: return false
        if (trimmed.isEmpty()) return false
        val updated = _state.value.comments + ReviewComment(
            path = file.path,
            line = 0,
            oldLine = 0,
            text = trimmed,
        )
        relayout(updated, clearSelection = false)
        return true
    }

    /**
     * Adds a comment on the specified [path] and line coordinates (`line = 0`
     * and `oldLine = 0` for a file-level comment). Blank text adds nothing.
     */
    fun addComment(path: String, line: Int, oldLine: Int, text: String): Boolean {
        val trimmed = text.trim()
        if (trimmed.isEmpty() || _state.value.doc.files.none { it.path == path }) return false
        val updated = _state.value.comments + ReviewComment(
            path = path,
            line = line,
            oldLine = oldLine,
            text = trimmed,
        )
        relayout(updated, clearSelection = true)
        return true
    }

    /** Rewrites a comment in place. Blank text or an invalid index changes nothing. */
    fun rewrite(index: Int, text: String): Boolean {
        val trimmed = text.trim()
        val current = _state.value.comments
        if (trimmed.isEmpty() || index !in current.indices) return false
        val updated = current.toMutableList()
        updated[index] = updated[index].copy(text = trimmed)
        relayout(updated, clearSelection = false)
        return true
    }

    /** Removes the comment at [index]. */
    fun remove(index: Int) {
        val current = _state.value.comments
        if (index !in current.indices) return
        val updated = current.toMutableList()
        updated.removeAt(index)
        relayout(updated, clearSelection = false)
    }

    /**
     * Switches between [Comparison.Uncommitted] and [Comparison.OnBranch] by
     * asking [source] for a fresh [FrozenReview] and re-parsing its document.
     *
     * Keeps existing comments if the returned patch has the same hash; resets
     * comments, selection, and folded files when switching to a different diff.
     */
    suspend fun switchComparison(
        comparison: Comparison,
        source: ChatSource,
    ): Result<ReviewDoc?> {
        _state.value = _state.value.copy(loading = true, error = null)
        val fetched = source.review(comparison)
        val frozen = fetched.getOrElse { failure ->
            _state.value = _state.value.copy(loading = false, error = failure.failureDescription)
            return Result.failure(failure)
        }
        val samePatch = review.diff.patch?.hash != null &&
            review.diff.patch?.hash == frozen.diff.patch?.hash
        val nextComments = if (samePatch) _state.value.comments else emptyList()
        val parsed = renderDoc(frozen, nextComments)
        if (parsed == null) {
            val failure = RuntimeFailure(COULD_NOT_READ_DIFF)
            _state.value = _state.value.copy(loading = false, error = failure.description)
            return Result.failure(failure)
        }
        if (parsed.files.isEmpty() && frozen.diff.files.isNotEmpty()) {
            val failure = RuntimeFailure(MISSING_LISTED_FILES)
            _state.value = _state.value.copy(loading = false, error = failure.description)
            return Result.failure(failure)
        }
        review = frozen
        _state.value = ReviewUiState(
            comparison = comparison,
            doc = parsed,
            comments = nextComments,
            placed = places(nextComments, parsed),
            activeFile = if (samePatch) _state.value.activeFile.coerceIn(0, (parsed.files.size - 1).coerceAtLeast(0)) else 0,
            folded = if (samePatch) _state.value.folded else emptySet(),
            anchor = if (samePatch) _state.value.anchor else null,
            head = if (samePatch) _state.value.head else null,
            loading = false,
            error = null,
        )
        return Result.success(parsed.takeIf { it.files.isNotEmpty() })
    }

    private fun relayout(nextComments: List<ReviewComment>, clearSelection: Boolean) {
        // Keep the existing document if re-parsing fails rather than going blank.
        val nextDoc = renderDoc(review, nextComments) ?: _state.value.doc
        _state.value = _state.value.copy(
            doc = nextDoc,
            comments = nextComments,
            placed = places(nextComments, nextDoc),
            anchor = if (clearSelection) null else _state.value.anchor,
            head = if (clearSelection) null else _state.value.head,
        )
    }

    companion object {
        internal const val COULD_NOT_READ_DIFF = "this app could not read the host’s diff"
        internal const val MISSING_LISTED_FILES = "the host’s patch is missing the files it lists"

        fun document(
            review: FrozenReview,
            comments: List<ReviewComment>,
            renderDoc: (FrozenReview, List<ReviewComment>) -> ReviewDoc? = Bridge::reviewDoc,
        ): ReviewDoc? = renderDoc(review, comments)

        /**
         * Fetches and parses a [ReviewModel] for [comparison] from [source].
         *
         * Returns `Result.success(null)` when there are no changes; returns
         * `Result.failure` when the host failed or the diff could not be parsed.
         */
        suspend fun load(
            source: ChatSource,
            comparison: Comparison,
            renderDoc: (FrozenReview, List<ReviewComment>) -> ReviewDoc? = Bridge::reviewDoc,
        ): Result<ReviewModel?> {
            val frozen = source.review(comparison).getOrElse { return Result.failure(it) }
            val doc = renderDoc(frozen, emptyList())
                ?: return Result.failure(RuntimeFailure(COULD_NOT_READ_DIFF))
            if (doc.files.isEmpty()) {
                return if (frozen.diff.files.isEmpty()) {
                    Result.success(null)
                } else {
                    Result.failure(RuntimeFailure(MISSING_LISTED_FILES))
                }
            }
            return Result.success(
                ReviewModel(
                    review = frozen,
                    comparison = comparison,
                    renderDoc = renderDoc,
                    initialDoc = doc,
                ),
            )
        }

        /**
         * Where the runtime's document draws each comment: on the line it names
         * (the new side's, or a removed line's old one), else on its file.
         */
        internal fun places(
            comments: List<ReviewComment>,
            doc: ReviewDoc,
        ): Map<CommentPlace, List<Int>> {
            val placed = mutableMapOf<CommentPlace, MutableList<Int>>()
            for ((index, comment) in comments.withIndex()) {
                val fileIndex = doc.files.indexOfFirst { it.path == comment.path }
                if (fileIndex < 0) continue
                var at: ReviewLine? = null
                if (comment.line > 0 || comment.oldLine > 0) {
                    val hunks = doc.files[fileIndex].hunks
                    search@ for ((hunkIndex, hunk) in hunks.withIndex()) {
                        for ((lineIndex, diffLine) in hunk.lines.withIndex()) {
                            val hit = if (comment.line > 0) {
                                diffLine.newLine == comment.line
                            } else {
                                diffLine.kind == LineKind.Removed && diffLine.oldLine == comment.oldLine
                            }
                            if (hit) {
                                at = ReviewLine(file = fileIndex, hunk = hunkIndex, line = lineIndex)
                                break@search
                            }
                        }
                    }
                }
                placed.getOrPut(CommentPlace(file = fileIndex, line = at)) { mutableListOf() }.add(index)
            }
            return placed.mapValues { it.value.toList() }
        }
    }
}
