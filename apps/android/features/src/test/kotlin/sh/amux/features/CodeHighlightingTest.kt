package sh.amux.features

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.junit4.createComposeRule
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import sh.amux.values.CodeRun
import sh.amux.values.HighlightedCode
import sh.amux.values.TokenKind
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class CodeHighlightingTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val block = CodeHighlighting.Request.Block(code = "let x", info = "rust", streaming = false)
    private val coloured = HighlightedCode(
        lines = listOf(listOf(CodeRun("let", TokenKind.Keyword), CodeRun(" x", TokenKind.Plain))),
        settled = true,
    )

    /** An unsettled answer is drawn and asked for again; a settled one is kept and never asked for twice. */
    @Test
    fun anUnsettledAnswerIsAskedAgainAndASettledOneKept() = runBlocking {
        var asked = 0
        val highlighting = CodeHighlighting { _ ->
            asked += 1
            if (asked == 1) coloured.copy(settled = false) else coloured
        }
        val drawn = mutableListOf<HighlightedCode?>()
        highlighting.follow(block) { drawn += it }
        assertEquals(listOf(false, true), drawn.map { it?.settled })
        assertEquals(2, asked)
        assertEquals(coloured, highlighting.kept(block))

        highlighting.follow(block) { drawn += it }
        assertEquals("a kept answer does not cross the bridge again", 2, asked)
    }

    @Test
    fun nothingThatCanColourDrawsPlain() = runBlocking {
        val drawn = mutableListOf<HighlightedCode?>()
        CodeHighlighting { null }.follow(block) { drawn += it }
        assertEquals(listOf<HighlightedCode?>(null), drawn)
        assertNull(CodeHighlighting.Plain.kept(block))
        // A failing call draws plain rather than failing the screen.
        CodeHighlighting { error("no bridge") }.follow(block) { drawn += it }
        assertEquals(listOf<HighlightedCode?>(null, null), drawn)
    }

    /** An answer for code that has moved on since never draws the wrong words: such a line is plain. */
    @Test
    fun aLineTheAnswerDoesNotMatchIsDrawnPlain() {
        val runs = coloured.runsFor(listOf("let x", "let y"))
        assertEquals(coloured.lines[0], runs[0])
        assertEquals(listOf(CodeRun("let y", TokenKind.Plain)), runs[1])
        assertEquals(listOf(emptyList<CodeRun>()), null.runsFor(listOf("")))
    }

    /**
     * Streaming a long block asks for a new text on every delta; none of it
     * is kept, so the answers worth keeping are not pushed out by it.
     */
    @Test
    fun streamingALongBlockDoesNotPushOutSettledAnswers() = runBlocking {
        val asked = mutableMapOf<CodeHighlighting.Request, Int>()
        val highlighting = CodeHighlighting { request ->
            asked[request] = (asked[request] ?: 0) + 1
            coloured
        }
        highlighting.follow(block) {}
        val code = StringBuilder()
        repeat(400) { line ->
            code.append("let x$line = $line;\n")
            val growing = CodeHighlighting.Request.Block(code = code.toString(), info = "rust", streaming = true)
            highlighting.follow(growing) {}
            assertNull("a streaming block's answer is not kept", highlighting.kept(growing))
        }
        assertEquals(coloured, highlighting.kept(block))
        highlighting.follow(block) {}
        assertEquals("the settled block is still kept", 1, asked[block])
    }

    /**
     * A block that moves on, as a streaming message's does on every delta,
     * draws on with its last colours until the new text is answered rather
     * than flashing plain in between.
     */
    @Test
    fun aBlockThatMovesOnKeepsItsColoursUntilTheNewTextIsAnswered() {
        val first = CodeHighlighting.Request.Block(code = "let x", info = "rust", streaming = true)
        val second = CodeHighlighting.Request.Block(code = "let x\nlet", info = "rust", streaming = true)
        val grown = HighlightedCode(lines = coloured.lines + listOf(listOf(CodeRun("let", TokenKind.Keyword))), settled = true)
        val answered = CountDownLatch(1)
        val highlighting = CodeHighlighting { request ->
            if (request == second) {
                answered.await(10, TimeUnit.SECONDS)
                grown
            } else {
                coloured
            }
        }
        var request by mutableStateOf<CodeHighlighting.Request>(first)
        val drawn = mutableListOf<HighlightedCode?>()
        composeRule.setContent {
            CompositionLocalProvider(LocalCodeHighlighting provides highlighting) {
                drawn += rememberHighlighted(request)
            }
        }
        composeRule.waitUntil(5_000) { drawn.lastOrNull() == coloured }

        request = second
        composeRule.waitForIdle()
        assertEquals("the moved block draws its last colours while asked", coloured, drawn.last())

        answered.countDown()
        composeRule.waitUntil(5_000) { drawn.last() == grown }
    }
}
