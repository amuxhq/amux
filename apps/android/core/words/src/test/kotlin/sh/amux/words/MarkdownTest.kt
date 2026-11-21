package sh.amux.words

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Duration
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

class MarkdownTest {
    private fun blocks(source: String): List<MarkdownBlock> = MarkdownDocument.parse(source).blocks

    @Test
    fun headingsKeepTheirLevel() {
        val heading = blocks("## Why it broke").first() as MarkdownBlock.Heading
        assertEquals(2, heading.level)
        assertEquals("Why it broke", heading.text.plainText)
    }

    @Test
    fun fencedCodeKeepsItsLinesAndItsLanguage() {
        val code = blocks("```rust\nlet x = 1;\n    let y = 2;\n```").first() as MarkdownBlock.Code
        assertEquals("rust", code.language)
        assertEquals("let x = 1;\n    let y = 2;", code.text)
    }

    @Test
    fun aFenceWithNoLanguageSaysSoRatherThanGuessing() {
        val code = blocks("```\nplain\n```").first() as MarkdownBlock.Code
        assertNull(code.language)
        assertEquals("plain", code.text)
    }

    @Test
    fun listsKeepTheirMarkersAndTheirDepth() {
        val list = blocks("- one\n- two\n  - nested").first() as MarkdownBlock.ListBlock
        assertFalse(list.ordered)
        assertEquals(listOf(0, 0, 1), list.items.map { it.depth })
        assertEquals(listOf("one", "two", "nested"), list.items.map { it.text.plainText })
    }

    @Test
    fun aNumberedListIsNumberedRatherThanBulleted() {
        val list = blocks("1. first\n2. second").first() as MarkdownBlock.ListBlock
        assertTrue(list.ordered)
        assertEquals(listOf("1.", "2."), list.items.map { it.marker })
    }

    @Test
    fun aTablesAlignmentRuleIsNotARow() {
        val table = blocks(
            """
            | Layer | Reads |
            | --- | ---: |
            | claude | yes |
            | codex | no |
            """.trimIndent(),
        ).first() as MarkdownBlock.Table
        assertEquals(listOf("Layer", "Reads"), table.header.map { it.plainText })
        assertEquals(2, table.rows.size)
        assertEquals(listOf("claude", "yes"), table.rows[0].map { it.plainText })
        assertEquals(listOf("codex", "no"), table.rows[1].map { it.plainText })
    }

    @Test
    fun quotesKeepTheirLinesAndDropTheirMarker() {
        val quote = blocks("> first\n> second").first() as MarkdownBlock.Quote
        assertEquals(listOf("first", "second"), quote.lines.map { it.plainText })
    }

    @Test
    fun linesWithNoBlankLineBetweenThemAreOneParagraph() {
        val parsed = blocks("one line\nand its continuation\n\na second paragraph")
        assertEquals(2, parsed.size)
        val first = parsed[0] as MarkdownBlock.Paragraph
        assertEquals("one line\nand its continuation", first.text.plainText)
    }

    @Test
    fun emphasisCodeSpansAndStrikethroughResolveRatherThanShowingTheirPunctuation() {
        val para = blocks("run `cargo test` **now** and *soon* ~~not later~~").first() as MarkdownBlock.Paragraph
        assertEquals("run cargo test now and soon not later", para.text.plainText)
        assertTrue(para.text.spans.any { it.code && it.text == "cargo test" })
        assertTrue(para.text.spans.any { it.bold && it.text == "now" })
        assertTrue(para.text.spans.any { it.italic && it.text == "soon" })
        assertTrue(para.text.spans.any { it.strikethrough && it.text == "not later" })
    }

    @Test
    fun snakeCaseIdentifiersKeepTheirUnderscores() {
        val para = blocks("call create_issue and some_var_name").first() as MarkdownBlock.Paragraph
        assertEquals("call create_issue and some_var_name", para.text.plainText)
        assertTrue(para.text.spans.none { it.italic })
    }

    @Test
    fun aLinkKeepsItsTextAndItsDestination() {
        val para = blocks("see [the spec](https://example.com)").first() as MarkdownBlock.Paragraph
        assertEquals("see the spec", para.text.plainText)
        assertEquals(listOf("https://example.com"), para.text.links.map { it.url })
        assertEquals(1, para.text.spans.count { it.underlined })
    }

    @Test
    fun onlyWebAndMailLinksStayLinks() {
        val source =
            "[page](https://example.com) [plain](http://example.com) " +
                "[mail](mailto:ada@example.com) [sign-in](amux://callback?code=abc) " +
                "[call](tel:123) [file](file:///etc/hosts) <amux://callback?code=abc>"
        val para = blocks(source).first() as MarkdownBlock.Paragraph
        assertEquals(
            listOf("https://example.com", "http://example.com", "mailto:ada@example.com"),
            para.text.links.map { it.url },
        )
        assertEquals(3, para.text.spans.count { it.underlined })
        assertTrue(para.text.plainText.contains("sign-in"))
        assertTrue(para.text.plainText.contains("amux://callback?code=abc"))
    }

    @Test
    fun anOpenableLinkIsAWebPageOrAnEmailAddress() {
        assertNotNull(OpenableLink.parse("HTTPS://example.com"))
        assertNotNull(OpenableLink.parse("mailto:ada@example.com"))
        assertNull(OpenableLink.parse("amux://callback?code=abc"))
        assertNull(OpenableLink.parse("shortcuts://run-shortcut?name=x"))
        assertNull(OpenableLink.parse("javascript:alert(1)"))
        assertNull(OpenableLink.parse("example.com"))
        assertNull(OpenableLink.parse(""))
    }

    @Test
    fun malformedInlineMarkupFallsBackToLiteralText() {
        val para = blocks("a * b").first() as MarkdownBlock.Paragraph
        assertEquals("a * b", para.text.plainText)
    }

    @Test
    fun aHorizontalRuleIsARuleAndNotAList() {
        val parsed = blocks("---")
        assertEquals(1, parsed.size)
        assertEquals(MarkdownBlock.Rule, parsed[0])
    }

    @Test
    fun aPlanStreamingInIsParsedWhileItsNextHeadingIsHalfWritten() {
        val parsed = blocksWithin("## Summary\n\nCreate `README.md` with `UPDATED`.\n\n##")
        val first = parsed.first() as MarkdownBlock.Heading
        assertEquals(2, first.level)
        assertEquals("Summary", first.text.plainText)
        assertTrue(
            parsed.any { block ->
                block is MarkdownBlock.Paragraph && block.text.plainText == "Create README.md with UPDATED."
            },
        )
    }

    @Test
    fun aHashWithNoSpaceAfterItStartsAParagraphNotAHeading() {
        val para = blocksWithin("#42 is fixed").first() as MarkdownBlock.Paragraph
        assertEquals("#42 is fixed", para.text.plainText)
    }

    @Test
    fun aPipeLineThatIsNotATableIsAParagraph() {
        val para = blocksWithin("| one cell").first() as MarkdownBlock.Paragraph
        assertEquals("| one cell", para.text.plainText)
    }

    @Test
    fun anAgentsSingleLineBreaksStayLineBreaks() {
        val parsed = blocksWithin("alpha\nbeta\ngamma")
        assertEquals(1, parsed.size)
        val para = parsed.first() as MarkdownBlock.Paragraph
        assertEquals(listOf("alpha", "beta", "gamma"), para.text.plainText.split('\n'))

        val paragraphs = blocksWithin("alpha\n\nbeta")
        assertEquals(2, paragraphs.size)
    }

    @Test
    fun elapsedSpellsSecondsMinutesHoursAndDays() {
        assertEquals("0s", Elapsed.spelled(-3.0))
        assertEquals("0s", Elapsed.spelled(0.4))
        assertEquals("59s", Elapsed.spelled(59.9))
        assertEquals("1m", Elapsed.spelled(60.0))
        assertEquals("59m", Elapsed.spelled(3_599.0))
        assertEquals("1h", Elapsed.spelled(3_600.0))
        assertEquals("23h", Elapsed.spelled(86_399.0))
        assertEquals("1d", Elapsed.spelled(86_400.0))
        assertEquals("3d", Elapsed.spelled(Duration.ofDays(3)))
    }

    private fun blocksWithin(source: String): List<MarkdownBlock> {
        val executor = Executors.newSingleThreadExecutor()
        return try {
            executor.submit<List<MarkdownBlock>> { MarkdownDocument.parse(source).blocks }
                .get(2, TimeUnit.SECONDS)
        } finally {
            executor.shutdownNow()
        }
    }
}
