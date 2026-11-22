package sh.amux.features

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.withStyle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext
import sh.amux.design.AmuxColors
import sh.amux.values.CodeRun
import sh.amux.values.HighlightedCode
import sh.amux.values.PatchHunk
import sh.amux.values.TokenKind
import kotlin.coroutines.coroutineContext

/**
 * How the app gets code coloured: a fenced block in an agent's markdown, and
 * the patch an edit card shows. The colouring is the shared view's, so a
 * block reads with the same runs here as on the other apps and in the
 * terminal; [ink] maps each kind of token to this app's palette.
 *
 * A screen reads it from [LocalCodeHighlighting]. The app puts in one that
 * asks the bridge; a screen drawn without it, as the storybook and the
 * screen tests draw them, draws its code plain.
 *
 * An answer can come back unsettled: a grammar is being built in the
 * background on its first use, or a block still streaming has lines not yet
 * passed over. It is drawn as it is and asked for again shortly. Settled
 * answers are kept by what they answer, so a block scrolled back into view
 * draws coloured at once without crossing the bridge again.
 *
 * A view draws the kept answer for what it shows, else the answer its own
 * asking handed it last, which it holds while what it shows moves on: a block
 * still streaming keeps its colours on the lines that read the same rather
 * than drawing plain until the new text is answered. Reading what is kept
 * is not observed; a view learns of a new answer from its own asking, which
 * looks in what is kept before every crossing. Asking is never done while a
 * screen is composed: answers are fetched from an effect, off the main
 * thread.
 */
@Stable
class CodeHighlighting(private val answer: ((Request) -> HighlightedCode?)?) {
    /** What colours are asked for. */
    sealed interface Request {
        /**
         * A fenced block: its code, the fence's info string, and whether it
         * is the block still being written (the open fence of a message
         * still streaming), whose completed lines are coloured at a bounded
         * rate so the cost does not grow with the square of its length.
         */
        data class Block(val code: String, val info: String, val streaming: Boolean) : Request

        /**
         * One file's change in an edit card, coloured as the file at [path]
         * reads: its hunks as the shared view handed them. The answer has a
         * line for each of the hunks' lines, hunk after hunk.
         */
        data class Patch(val path: String, val hunks: List<PatchHunk>) : Request
    }

    /** Whether anything colours code; without, a view draws plain and asks nothing. */
    val colours: Boolean get() = answer != null

    /**
     * Settled answers by request, touched only on the main thread. Started
     * again when full: what is on screen is asked for again within a frame,
     * and nothing here is worth an eviction order. A view drawing a dropped
     * answer keeps the colours its own asking handed it.
     *
     * A block still streaming is never kept: its text is gone by the next
     * delta, so its answers would only push out the ones worth keeping.
     */
    private val kept = HashMap<Request, HighlightedCode>()

    private fun keep(request: Request, code: HighlightedCode) {
        if (request is Request.Block && request.streaming) return
        if (kept.size >= LIMIT && request !in kept) kept.clear()
        kept[request] = code
    }

    /** The settled answer kept for [request], without asking. */
    fun kept(request: Request): HighlightedCode? = kept[request]

    /**
     * Asks for [request]'s colours away from the main thread and hands each
     * answer to [draw], asking again shortly while it is unsettled, and keeps
     * a settled one. Returns once an answer is settled, or nothing could
     * colour the code (null is handed, so a view stops drawing an earlier
     * answer), or the caller is cancelled.
     */
    suspend fun follow(request: Request, draw: (HighlightedCode?) -> Unit) {
        val answer = answer ?: return
        while (coroutineContext.isActive) {
            // Asked before every crossing: another view showing the same
            // code may have settled it while this one waited.
            kept[request]?.let {
                draw(it)
                return
            }
            val code = withContext(Dispatchers.Default) { runCatching { answer(request) }.getOrNull() }
            if (code == null) {
                draw(null)
                return
            }
            if (code.settled) keep(request, code)
            draw(code)
            if (code.settled) return
            delay(AGAIN_MS)
        }
    }

    companion object {
        /**
         * How long to wait before asking again for an unsettled answer:
         * under the shared view's tenth of a second between passes over a
         * streaming block, so a due pass starts and a finished one shows
         * without waiting for more text.
         */
        const val AGAIN_MS: Long = 50L

        private const val LIMIT = 256

        /** Nothing coloured. */
        val Plain: CodeHighlighting = CodeHighlighting(null)
    }
}

/** How code on screen is coloured; plain unless the app provides the bridge's. */
val LocalCodeHighlighting = staticCompositionLocalOf { CodeHighlighting.Plain }

/**
 * The colours for [request], kept or as its own asking handed them last;
 * null while there are none, when the code is drawn plain.
 *
 * The last answer outlives the request it answered, so a block that moves on
 * (a delta of a streaming message, a patch edited) draws on with it until the
 * new text is answered. It may be for earlier text: the caller draws it
 * through [runsFor], which colours only the lines that still read the same.
 */
@Composable
internal fun rememberHighlighted(request: Request): HighlightedCode? {
    val highlighting = LocalCodeHighlighting.current
    if (!highlighting.colours) return null
    var own by remember(highlighting) { mutableStateOf<HighlightedCode?>(null) }
    LaunchedEffect(highlighting, request) { highlighting.follow(request) { own = it } }
    return highlighting.kept(request) ?: own
}

private typealias Request = CodeHighlighting.Request

/** Code's text as lines, as the shared view counts them. */
internal fun codeLines(text: String): List<String> = text.split('\n')

/**
 * Each of [lines] as runs: the answer's own where its line reads the same,
 * else the line plain, so an answer for code that has moved on since never
 * draws the wrong words.
 */
internal fun HighlightedCode?.runsFor(lines: List<String>): List<List<CodeRun>> = lines.mapIndexed { index, line ->
    val answered = this?.lines?.getOrNull(index)
    if (answered != null && answered.joinToString("") { it.text } == line) {
        answered
    } else {
        plainRuns(line)
    }
}

private fun plainRuns(line: String): List<CodeRun> =
    if (line.isEmpty()) emptyList() else listOf(CodeRun(text = line, kind = TokenKind.Plain))

/** Runs as text, each in its kind's colour from this app's palette. */
internal fun AmuxColors.codeText(runs: List<CodeRun>): AnnotatedString = buildAnnotatedString {
    appendRuns(runs, this@codeText)
}

/** Lines of runs as one text, a line to each line. */
internal fun AmuxColors.codeText(lines: List<List<CodeRun>>, separator: String = "\n"): AnnotatedString =
    buildAnnotatedString {
        lines.forEachIndexed { index, runs ->
            if (index > 0) append(separator)
            appendRuns(runs, this@codeText)
        }
    }

private fun AnnotatedString.Builder.appendRuns(runs: List<CodeRun>, colors: AmuxColors) {
    for (run in runs) {
        if (run.kind == TokenKind.Plain) {
            append(run.text)
        } else {
            withStyle(SpanStyle(color = colors.ink(run.kind))) { append(run.text) }
        }
    }
}
