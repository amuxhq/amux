package sh.amux.features

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.words.OpenableLink

@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class MarkdownViewTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun rendersHeadingsInlineFormattingAndLists() {
        val md = """
            # Release Plan
            ## Verification Steps
            Check **bold**, *italic*, ~~old~~, and `cargo test` inline.

            - First bullet
              - Nested bullet
            1. Step one
            2. Step two

            ---
        """.trimIndent()

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                MarkdownView(markdown = md)
            }
        }

        composeRule.onNodeWithTag(MarkdownViewTags.ROOT).assertIsDisplayed()
        composeRule.onNodeWithText("Release Plan").assertIsDisplayed()
        composeRule.onNodeWithText("Verification Steps").assertIsDisplayed()
        composeRule.onNodeWithText("Check bold, italic, old, and cargo test inline.").assertIsDisplayed()
        composeRule.onNodeWithText("First bullet").assertIsDisplayed()
        composeRule.onNodeWithText("Nested bullet").assertIsDisplayed()
        composeRule.onNodeWithText("Step one").assertIsDisplayed()
        composeRule.onNodeWithText("Step two").assertIsDisplayed()
        composeRule.onNodeWithTag(MarkdownViewTags.RULE).assertIsDisplayed()
    }

    @Test
    fun rendersFencedCodeBlockAndInvokesCopyCallback() {
        val code = "fn main() {\n    println!(\"hello\");\n}"
        val md = "```rust\n$code\n```"
        val copied = mutableListOf<String>()

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                MarkdownView(
                    markdown = md,
                    onCopyCode = { copied += it },
                )
            }
        }

        composeRule.onNodeWithTag(MarkdownViewTags.CODE_BLOCK).assertIsDisplayed()
        composeRule.onNodeWithText("rust").assertIsDisplayed()
        composeRule.onNodeWithText(code).assertIsDisplayed()
        composeRule.onNodeWithTag(MarkdownViewTags.CODE_COPY).performClick()
        assertEquals(listOf(code), copied)
    }

    @Test
    fun rendersDiffFenceLines() {
        val md = """
            ```diff
            @@ -1,2 +1,2 @@
            -old line
            +new line
             context line
            ```
        """.trimIndent()

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                MarkdownView(markdown = md)
            }
        }

        composeRule.onNodeWithTag(MarkdownViewTags.CODE_BLOCK).assertIsDisplayed()
        composeRule.onNodeWithText("diff").assertIsDisplayed()
        composeRule.onNodeWithText("-old line").assertIsDisplayed()
        composeRule.onNodeWithText("+new line").assertIsDisplayed()
        composeRule.onNodeWithText(" context line").assertIsDisplayed()
    }

    @Test
    fun rendersQuoteAndTable() {
        val md = """
            > Keep surfaces neutral.
            > Spend colour only on attention.

            | Crate | Role |
            | --- | --- |
            | app-bridge | Shared runtime |
            | app-jni | Android JNI |
        """.trimIndent()

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                MarkdownView(markdown = md)
            }
        }

        composeRule.onNodeWithTag(MarkdownViewTags.QUOTE).assertIsDisplayed()
        composeRule.onNodeWithText("Keep surfaces neutral.").assertIsDisplayed()
        composeRule.onNodeWithText("Spend colour only on attention.").assertIsDisplayed()
        composeRule.onNodeWithTag(MarkdownViewTags.TABLE).assertIsDisplayed()
        composeRule.onNodeWithText("Crate").assertIsDisplayed()
        composeRule.onNodeWithText("Role").assertIsDisplayed()
        composeRule.onNodeWithText("app-bridge").assertIsDisplayed()
        composeRule.onNodeWithText("Shared runtime").assertIsDisplayed()
    }

    @Test
    fun rendersClickableOpenableLinkAndStreamingIndicator() {
        val opened = mutableListOf<OpenableLink>()
        val md = "See [amux docs](https://amux.sh/docs) and [ignored](amux://callback)."

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                MarkdownView(
                    markdown = md,
                    workingNote = true,
                    streaming = true,
                    onOpenLink = { opened += it },
                )
            }
        }

        composeRule.onNodeWithText("See amux docs and ignored.").assertIsDisplayed()
        composeRule.onNodeWithTag(MarkdownViewTags.STREAMING).assertIsDisplayed()

        // Verify the built AnnotatedString carries only the openable https link and invokes onOpenLink.
        val annotated = buildInlineMarkdownAnnotatedString(
            markdownText = sh.amux.words.MarkdownDocument.inline(md),
            colors = sh.amux.design.AmuxDesign.App.colors(Appearance.Light),
            typography = sh.amux.design.AmuxDesign.App.typography,
            onOpenLink = { opened += it },
        )
        val links = annotated.getLinkAnnotations(0, annotated.length)
        assertEquals(1, links.size)
        val clickable = links.single().item as androidx.compose.ui.text.LinkAnnotation.Clickable
        clickable.linkInteractionListener?.onClick(clickable)
        assertTrue(opened.any { it.url == "https://amux.sh/docs" })
    }

    @Test
    fun nonStreamingHidesStreamingIndicator() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                MarkdownView(markdown = "All done.", streaming = false)
            }
        }

        assertEquals(0, composeRule.onAllNodesWithTag(MarkdownViewTags.STREAMING).fetchSemanticsNodes().size)
    }
}
