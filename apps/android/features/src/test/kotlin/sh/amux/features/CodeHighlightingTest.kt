package sh.amux.features

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import sh.amux.values.CodeRun
import sh.amux.values.HighlightedCode
import sh.amux.values.TokenKind

class CodeHighlightingTest {
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
}
