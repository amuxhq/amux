package sh.amux.features

import sh.amux.design.IconSize
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import sh.amux.design.AmuxColors
import sh.amux.design.AmuxFonts
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxTheme
import sh.amux.design.AmuxTypography
import sh.amux.design.Motion
import sh.amux.design.Space
import sh.amux.words.MarkdownBlock
import sh.amux.words.MarkdownDocument
import sh.amux.words.MarkdownText
import sh.amux.words.OpenableLink

/**
 * Local test tags for [MarkdownView] components.
 */
object MarkdownViewTags {
    const val ROOT: String = "markdown.root"
    const val CODE_BLOCK: String = "markdown.code"
    const val CODE_COPY: String = "markdown.code.copy"
    const val QUOTE: String = "markdown.quote"
    const val TABLE: String = "markdown.table"
    const val RULE: String = "markdown.rule"
}

/**
 * Bounded LRU cache of parsed markdown documents so recompositions and scroll
 * recycling never re-parse unchanged prose on the UI thread.
 */
private object ParsedMarkdownCache {
    private const val MAX_ENTRIES = 64
    private val cache = object : LinkedHashMap<String, MarkdownDocument>(MAX_ENTRIES, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, MarkdownDocument>?): Boolean =
            size > MAX_ENTRIES
    }

    @Synchronized
    fun parse(source: String): MarkdownDocument {
        cache[source]?.let { return it }
        val doc = MarkdownDocument.parse(source)
        cache[source] = doc
        return doc
    }
}

/**
 * Renders agent or prompt prose parsed by [MarkdownDocument.parse].
 *
 * Supports headings (levels 1–6), paragraphs with inline styling and clickable
 * [OpenableLink]s, ordered/unordered nested lists, horizontally scrollable
 * fenced code blocks with copy action and diff line colouring, block quotes,
 * horizontally scrollable tables, horizontal rules and Codex [workingNote] ink.
 *
 * A [streaming] reply draws nothing extra: the composer's activity line
 * already says the turn is working. TalkBack hears the reply as one stop
 * whose state says it is still streaming.
 */
@Composable
fun MarkdownView(
    markdown: String,
    modifier: Modifier = Modifier,
    workingNote: Boolean = false,
    streaming: Boolean = false,
    title: String? = null,
    inBubble: Boolean = false,
    onOpenLink: (OpenableLink) -> Unit = {},
    onCopyCode: (String) -> Unit = {},
) {
    val document = remember(markdown) { ParsedMarkdownCache.parse(markdown) }
    val widthModifier = if (inBubble) Modifier else Modifier.fillMaxWidth()

    val streamingLabel = stringResource(R.string.markdown_streaming)
    Column(
        modifier = modifier
            .then(widthModifier)
            .then(
                when {
                    !streaming -> Modifier
                    // Nothing written yet: the reply is only its state, said
                    // as what the stop is.
                    document.blocks.isEmpty() && title.isNullOrEmpty() ->
                        Modifier.semantics { contentDescription = streamingLabel }
                    // While it streams, the reply is one stop that says so;
                    // its blocks part again into stops of their own once it
                    // is done.
                    else -> Modifier.semantics(mergeDescendants = true) { stateDescription = streamingLabel }
                },
            )
            .testTag(MarkdownViewTags.ROOT),
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        if (!title.isNullOrEmpty()) {
            MarkdownHeadingBlock(
                level = 1,
                text = remember(title) { MarkdownText.plain(title) },
                workingNote = workingNote,
                onOpenLink = onOpenLink,
            )
        }
        document.blocks.forEach { block ->
            MarkdownBlockView(
                block = block,
                workingNote = workingNote,
                onOpenLink = onOpenLink,
                onCopyCode = onCopyCode,
            )
        }
    }
}

/**
 * Parses and converts an inline markdown string into a Compose [AnnotatedString]
 * styled with amux's bundled faces and clickable [OpenableLink] annotations.
 */
@Composable
fun rememberInlineMarkdown(
    text: String,
    workingNote: Boolean = false,
    onOpenLink: (OpenableLink) -> Unit = {},
): AnnotatedString {
    val parsed = remember(text) { MarkdownDocument.inline(text) }
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    return remember(parsed, colors, typography, workingNote, onOpenLink) {
        buildInlineMarkdownAnnotatedString(
            markdownText = parsed,
            colors = colors,
            typography = typography,
            workingNote = workingNote,
            onOpenLink = onOpenLink,
        )
    }
}

/**
 * Converts a parsed [MarkdownText] into a Compose [AnnotatedString].
 */
fun buildInlineMarkdownAnnotatedString(
    markdownText: MarkdownText,
    colors: AmuxColors,
    typography: AmuxTypography,
    workingNote: Boolean = false,
    onOpenLink: (OpenableLink) -> Unit = {},
): AnnotatedString = buildAnnotatedString {
    val baseInk = if (workingNote) colors.inkMuted else colors.ink
    for (span in markdownText.spans) {
        val start = length
        append(span.text)
        val end = length

        val decorations = mutableListOf<TextDecoration>()
        if (span.strikethrough) decorations.add(TextDecoration.LineThrough)
        if (span.underlined) decorations.add(TextDecoration.Underline)

        val spanStyle = SpanStyle(
            color = when {
                span.link != null -> colors.accent
                span.code -> colors.inkMuted
                else -> baseInk
            },
            // Inline code is plain mono text in the size of the words around
            // it, with no box: a box butts against the letters beside it and
            // breaks into slabs where a span wraps.
            fontFamily = if (span.code) AmuxFonts.Mono else null,
            fontWeight = if (span.bold) FontWeight.SemiBold else null,
            fontStyle = if (span.italic) FontStyle.Italic else null,
            textDecoration = if (decorations.isEmpty()) null else TextDecoration.combine(decorations),
        )
        addStyle(spanStyle, start, end)

        val link = span.link
        if (link != null) {
            addLink(
                clickable = LinkAnnotation.Clickable(
                    tag = link.url,
                    styles = TextLinkStyles(
                        style = SpanStyle(
                            color = colors.accent,
                            textDecoration = TextDecoration.Underline,
                        ),
                    ),
                    linkInteractionListener = { onOpenLink(link) },
                ),
                start = start,
                end = end,
            )
        }
    }
}

@Composable
private fun MarkdownBlockView(
    block: MarkdownBlock,
    workingNote: Boolean,
    onOpenLink: (OpenableLink) -> Unit,
    onCopyCode: (String) -> Unit,
) {
    when (block) {
        is MarkdownBlock.Heading -> MarkdownHeadingBlock(
            level = block.level,
            text = block.text,
            workingNote = workingNote,
            onOpenLink = onOpenLink,
        )
        is MarkdownBlock.Paragraph -> MarkdownParagraphBlock(
            text = block.text,
            workingNote = workingNote,
            onOpenLink = onOpenLink,
        )
        is MarkdownBlock.ListBlock -> MarkdownListBlockView(
            items = block.items,
            workingNote = workingNote,
            onOpenLink = onOpenLink,
        )
        is MarkdownBlock.Code -> MarkdownCodeBlockView(
            language = block.language,
            text = block.text,
            onCopyCode = onCopyCode,
        )
        is MarkdownBlock.Quote -> MarkdownQuoteBlockView(
            lines = block.lines,
            onOpenLink = onOpenLink,
        )
        is MarkdownBlock.Table -> MarkdownTableBlockView(
            header = block.header,
            rows = block.rows,
            onOpenLink = onOpenLink,
        )
        MarkdownBlock.Rule -> AmuxHairline(
            modifier = Modifier
                .fillMaxWidth()
                .testTag(MarkdownViewTags.RULE),
        )
    }
}

@Composable
private fun MarkdownHeadingBlock(
    level: Int,
    text: MarkdownText,
    workingNote: Boolean,
    onOpenLink: (OpenableLink) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val annotated = remember(text, colors, typography, workingNote, onOpenLink) {
        buildInlineMarkdownAnnotatedString(text, colors, typography, workingNote, onOpenLink)
    }
    val style = headingStyle(level)
    Text(
        text = annotated,
        style = style,
        color = if (workingNote) colors.inkMuted else colors.ink,
        modifier = Modifier.semantics { heading() },
    )
}

private fun headingStyle(level: Int): TextStyle = when (level) {
    1 -> TextStyle(
        fontFamily = AmuxFonts.Sans,
        fontWeight = FontWeight.SemiBold,
        fontSize = 21.sp,
        lineHeight = 27.sp,
        letterSpacing = (-0.3).sp,
    )
    2 -> TextStyle(
        fontFamily = AmuxFonts.Sans,
        fontWeight = FontWeight.SemiBold,
        fontSize = 18.sp,
        lineHeight = 24.sp,
        letterSpacing = (-0.2).sp,
    )
    3 -> TextStyle(
        fontFamily = AmuxFonts.Sans,
        fontWeight = FontWeight.SemiBold,
        fontSize = 16.sp,
        lineHeight = 22.sp,
    )
    4 -> TextStyle(
        fontFamily = AmuxFonts.Sans,
        fontWeight = FontWeight.SemiBold,
        fontSize = 15.sp,
        lineHeight = 21.sp,
    )
    else -> TextStyle(
        fontFamily = AmuxFonts.Sans,
        fontWeight = FontWeight.Medium,
        fontSize = 14.sp,
        lineHeight = 20.sp,
    )
}

@Composable
private fun MarkdownParagraphBlock(
    text: MarkdownText,
    workingNote: Boolean,
    onOpenLink: (OpenableLink) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val annotated = remember(text, colors, typography, workingNote, onOpenLink) {
        buildInlineMarkdownAnnotatedString(text, colors, typography, workingNote, onOpenLink)
    }
    SelectionContainer {
        Text(
            text = annotated,
            style = typography.body,
            color = if (workingNote) colors.inkMuted else colors.ink,
        )
    }
}

@Composable
private fun MarkdownListBlockView(
    items: List<MarkdownBlock.Item>,
    workingNote: Boolean,
    onOpenLink: (OpenableLink) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    Column(verticalArrangement = Arrangement.spacedBy(Space.xs + Space.xxs)) {
        items.forEach { item ->
            val annotated = remember(item.text, colors, typography, workingNote, onOpenLink) {
                buildInlineMarkdownAnnotatedString(item.text, colors, typography, workingNote, onOpenLink)
            }
            Row(
                modifier = Modifier.padding(start = (item.depth * 16).dp),
                horizontalArrangement = Arrangement.spacedBy(Space.s),
                verticalAlignment = Alignment.Top,
            ) {
                Text(
                    text = item.marker,
                    style = typography.body,
                    color = colors.inkFaint,
                )
                SelectionContainer(modifier = Modifier.weight(1f, fill = false)) {
                    Text(
                        text = annotated,
                        style = typography.body,
                        color = if (workingNote) colors.inkMuted else colors.ink,
                    )
                }
            }
        }
    }
}

@Suppress("DEPRECATION")
@Composable
private fun MarkdownCodeBlockView(
    language: String?,
    text: String,
    onCopyCode: (String) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val metrics = AmuxTheme.metrics
    val clipboard = LocalClipboardManager.current
    var copied by remember(text) { mutableStateOf(false) }

    LaunchedEffect(copied) {
        if (copied) {
            delay(1500)
            copied = false
        }
    }

    val isDiff = remember(language) { language?.equals("diff", ignoreCase = true) == true }
    val copyLabel = stringResource(if (copied) R.string.markdown_code_copied else R.string.markdown_copy_code)
    val shape = RoundedCornerShape(metrics.controlRadius)

    Surface(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(MarkdownViewTags.CODE_BLOCK),
        shape = shape,
        color = colors.sunken,
        contentColor = colors.ink,
        border = BorderStroke(metrics.hairline, colors.hairline),
    ) {
        Column(modifier = Modifier.fillMaxWidth()) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(start = Space.m, end = Space.xs),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (!language.isNullOrEmpty()) {
                    Text(
                        text = language,
                        style = typography.sectionTitle,
                        color = colors.inkFaint,
                    )
                }
                Spacer(modifier = Modifier.weight(1f))
                IconButton(
                    onClick = {
                        clipboard.setText(AnnotatedString(text))
                        copied = true
                        onCopyCode(text)
                    },
                    modifier = Modifier.testTag(MarkdownViewTags.CODE_COPY),
                ) {
                    Icon(
                        imageVector = if (copied) Icons.Outlined.Check else Icons.Outlined.ContentCopy,
                        contentDescription = copyLabel,
                        tint = if (copied) colors.added else colors.inkMuted,
                        modifier = Modifier.size(IconSize.Inline),
                    )
                }
            }
            AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))
            SelectionContainer {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .horizontalScroll(rememberScrollState())
                        .padding(horizontal = Space.m, vertical = Space.s + Space.xxs),
                ) {
                    if (isDiff) {
                        DiffCodeLines(text = text)
                    } else {
                        Text(
                            text = text,
                            style = typography.mono,
                            color = colors.ink,
                            softWrap = false,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun DiffCodeLines(text: String) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val lines = remember(text) { text.split('\n') }

    Column(modifier = Modifier.width(IntrinsicSize.Max)) {
        lines.forEach { line ->
            val isAdded = line.startsWith("+") && !line.startsWith("+++")
            val isRemoved = line.startsWith("-") && !line.startsWith("---")
            val isHunk = line.startsWith("@@")
            val lineColor = when {
                isAdded -> colors.added
                isRemoved -> colors.removed
                isHunk -> colors.inkFaint
                else -> colors.ink
            }
            val lineBackground = when {
                isAdded -> colors.added.copy(alpha = 0.10f)
                isRemoved -> colors.removed.copy(alpha = 0.10f)
                else -> Color.Transparent
            }
            Text(
                text = line,
                style = typography.mono,
                color = lineColor,
                softWrap = false,
                modifier = Modifier
                    .fillMaxWidth()
                    .background(lineBackground)
                    .padding(horizontal = Space.xs),
            )
        }
    }
}

@Composable
private fun MarkdownQuoteBlockView(
    lines: List<MarkdownText>,
    onOpenLink: (OpenableLink) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(IntrinsicSize.Min)
            .testTag(MarkdownViewTags.QUOTE),
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Box(
            modifier = Modifier
                .width(2.5.dp)
                .fillMaxHeight()
                .clip(RoundedCornerShape(2.dp))
                .background(colors.inkFaint.copy(alpha = 0.45f)),
        )
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(Space.xs),
        ) {
            lines.forEach { line ->
                val annotated = remember(line, colors, typography, onOpenLink) {
                    buildInlineMarkdownAnnotatedString(line, colors, typography, workingNote = true, onOpenLink = onOpenLink)
                }
                Text(
                    text = annotated,
                    style = typography.body,
                    color = colors.inkMuted,
                )
            }
        }
    }
}

@Composable
private fun MarkdownTableBlockView(
    header: List<MarkdownText>,
    rows: List<List<MarkdownText>>,
    onOpenLink: (OpenableLink) -> Unit,
) {
    val colors = AmuxTheme.colors
    val metrics = AmuxTheme.metrics
    val shape = RoundedCornerShape(metrics.controlRadius)

    Box(
        modifier = Modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState())
            .testTag(MarkdownViewTags.TABLE),
    ) {
        Surface(
            shape = shape,
            color = colors.sunken.copy(alpha = 0.65f),
            contentColor = colors.ink,
            border = BorderStroke(metrics.hairline, colors.hairline),
        ) {
            Column(modifier = Modifier.width(IntrinsicSize.Max)) {
                MarkdownTableRow(cells = header, isHeader = true, onOpenLink = onOpenLink)
                rows.forEach { row ->
                    AmuxHairline()
                    MarkdownTableRow(cells = row, isHeader = false, onOpenLink = onOpenLink)
                }
            }
        }
    }
}

@Composable
private fun MarkdownTableRow(
    cells: List<MarkdownText>,
    isHeader: Boolean,
    onOpenLink: (OpenableLink) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    Row(verticalAlignment = Alignment.Top) {
        cells.forEachIndexed { index, cell ->
            val annotated = remember(cell, colors, typography, isHeader, onOpenLink) {
                buildInlineMarkdownAnnotatedString(
                    markdownText = cell,
                    colors = colors,
                    typography = typography,
                    workingNote = isHeader,
                    onOpenLink = onOpenLink,
                )
            }
            Text(
                text = annotated,
                style = if (isHeader) typography.caption.copy(fontWeight = FontWeight.SemiBold) else typography.monoSmall,
                color = if (isHeader) colors.inkMuted else colors.ink,
                modifier = Modifier
                    .width(if (index == 0) 146.dp else 128.dp)
                    .padding(horizontal = Space.s + Space.xxs, vertical = Space.s),
            )
        }
    }
}

