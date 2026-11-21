package sh.amux.features

import androidx.compose.foundation.BorderStroke
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
import androidx.compose.ui.platform.LocalDensity
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
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import sh.amux.design.AmuxColors
import sh.amux.design.AmuxFonts
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxTheme
import sh.amux.design.IconSize
import sh.amux.design.Space
import sh.amux.values.InlineRun
import sh.amux.values.Markdown
import sh.amux.values.ProseBlock
import sh.amux.values.ProseItem
import sh.amux.values.TableAlign
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

    /** One list item's words, beside its marker. */
    fun item(depth: Int, index: Int): String = "markdown.item.$depth.$index"
}

/**
 * Draws agent or prompt prose as the shared view layer parsed it: rows
 * arrive with their words already in blocks, so nothing here reads
 * markdown syntax.
 *
 * Draws headings (levels 1–6), paragraphs with inline styling and clickable
 * [OpenableLink]s, ordered, bulleted and task lists whose items hold blocks,
 * horizontally scrollable fenced code blocks with a copy action and diff line
 * colouring, quotes holding blocks, horizontally scrollable tables,
 * horizontal rules and Codex [workingNote] ink.
 *
 * A [streaming] reply draws nothing extra: the composer's activity line
 * already says the turn is working. TalkBack hears the reply as one stop
 * whose state says it is still streaming.
 */
@Composable
fun MarkdownView(
    markdown: Markdown,
    modifier: Modifier = Modifier,
    workingNote: Boolean = false,
    streaming: Boolean = false,
    title: String? = null,
    inBubble: Boolean = false,
    onOpenLink: (OpenableLink) -> Unit = {},
    onCopyCode: (String) -> Unit = {},
) {
    val colors = AmuxTheme.colors
    val widthModifier = if (inBubble) Modifier else Modifier.fillMaxWidth()
    val ink = if (workingNote) colors.inkMuted else colors.ink

    val streamingLabel = stringResource(R.string.markdown_streaming)
    Column(
        modifier = modifier
            .then(widthModifier)
            .then(
                when {
                    !streaming -> Modifier
                    // Nothing written yet: the reply is only its state, said
                    // as what the stop is.
                    markdown.blocks.isEmpty() && title.isNullOrEmpty() ->
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
                text = remember(title) { listOf(plainRun(title)) },
                ink = ink,
                onOpenLink = onOpenLink,
            )
        }
        MarkdownBlocks(
            blocks = markdown.blocks,
            ink = ink,
            streaming = streaming,
            onOpenLink = onOpenLink,
            onCopyCode = onCopyCode,
        )
    }
}

/** [text] in no style: words amux writes itself, such as a plan's lifted title. */
internal fun plainRun(text: String): InlineRun =
    InlineRun(text = text, strong = false, emphasis = false, strikethrough = false, code = false)

/** The runs' words without their styling: what TalkBack says and a test reads. */
fun List<InlineRun>.plainText(): String = joinToString("") { it.text }

/**
 * Inline runs as a Compose [AnnotatedString] in amux's bundled faces, with
 * clickable [OpenableLink] annotations.
 */
@Composable
fun rememberInlineRuns(
    runs: List<InlineRun>,
    workingNote: Boolean = false,
    onOpenLink: (OpenableLink) -> Unit = {},
): AnnotatedString {
    val colors = AmuxTheme.colors
    return remember(runs, colors, workingNote, onOpenLink) {
        buildInlineAnnotatedString(runs = runs, colors = colors, workingNote = workingNote, onOpenLink = onOpenLink)
    }
}

/**
 * Converts inline runs into a Compose [AnnotatedString]. A link is
 * underlined and coloured only when the phone will open it; any other
 * target stays as its words.
 */
fun buildInlineAnnotatedString(
    runs: List<InlineRun>,
    colors: AmuxColors,
    workingNote: Boolean = false,
    onOpenLink: (OpenableLink) -> Unit = {},
): AnnotatedString = annotatedRuns(runs, colors, if (workingNote) colors.inkMuted else colors.ink, onOpenLink)

private fun annotatedRuns(
    runs: List<InlineRun>,
    colors: AmuxColors,
    ink: Color,
    onOpenLink: (OpenableLink) -> Unit,
): AnnotatedString = buildAnnotatedString {
    for (run in runs) {
        val start = length
        append(run.text)
        val end = length
        val link = run.link?.let(OpenableLink::parse)

        val decorations = mutableListOf<TextDecoration>()
        if (run.strikethrough) decorations.add(TextDecoration.LineThrough)
        if (link != null) decorations.add(TextDecoration.Underline)

        val spanStyle = SpanStyle(
            color = when {
                link != null -> colors.accent
                run.code -> colors.inkMuted
                else -> ink
            },
            // Inline code is plain mono text in the size of the words around
            // it, with no box: a box butts against the letters beside it and
            // breaks into slabs where a span wraps.
            fontFamily = if (run.code) AmuxFonts.Mono else null,
            fontWeight = if (run.strong) FontWeight.SemiBold else null,
            fontStyle = if (run.emphasis) FontStyle.Italic else null,
            textDecoration = if (decorations.isEmpty()) null else TextDecoration.combine(decorations),
        )
        addStyle(spanStyle, start, end)

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

/** Blocks in [ink], as a reply, a list item or a quote holds them. */
@Composable
private fun MarkdownBlocks(
    blocks: List<ProseBlock>,
    ink: Color,
    streaming: Boolean,
    onOpenLink: (OpenableLink) -> Unit,
    onCopyCode: (String) -> Unit,
) {
    blocks.forEach { block ->
        MarkdownBlockView(
            block = block,
            ink = ink,
            streaming = streaming,
            onOpenLink = onOpenLink,
            onCopyCode = onCopyCode,
        )
    }
}

@Composable
private fun MarkdownBlockView(
    block: ProseBlock,
    ink: Color,
    streaming: Boolean,
    onOpenLink: (OpenableLink) -> Unit,
    onCopyCode: (String) -> Unit,
) {
    when (block) {
        is ProseBlock.Heading -> MarkdownHeadingBlock(
            level = block.level,
            text = block.text,
            ink = ink,
            onOpenLink = onOpenLink,
        )
        is ProseBlock.Paragraph -> MarkdownParagraphBlock(
            text = block.text,
            ink = ink,
            onOpenLink = onOpenLink,
        )
        is ProseBlock.List -> MarkdownListBlockView(
            ordered = block.ordered,
            depth = block.depth,
            start = block.start,
            items = block.items,
            ink = ink,
            streaming = streaming,
            onOpenLink = onOpenLink,
            onCopyCode = onCopyCode,
        )
        is ProseBlock.Code -> MarkdownCodeBlockView(
            language = block.language,
            text = block.code,
            onCopyCode = onCopyCode,
            // Only a fence the text ends inside is still being written; every
            // other block of a streaming reply is finished.
            streaming = streaming && !block.closed,
        )
        is ProseBlock.Quote -> MarkdownQuoteBlockView(
            blocks = block.blocks,
            streaming = streaming,
            onOpenLink = onOpenLink,
            onCopyCode = onCopyCode,
        )
        is ProseBlock.Table -> MarkdownTableBlockView(
            alignments = block.alignments,
            header = block.header,
            rows = block.rows,
            onOpenLink = onOpenLink,
        )
        ProseBlock.Rule -> AmuxHairline(
            modifier = Modifier
                .fillMaxWidth()
                .testTag(MarkdownViewTags.RULE),
        )
    }
}

@Composable
private fun MarkdownHeadingBlock(
    level: Int,
    text: List<InlineRun>,
    ink: Color,
    onOpenLink: (OpenableLink) -> Unit,
) {
    val colors = AmuxTheme.colors
    val annotated = remember(text, colors, ink, onOpenLink) { annotatedRuns(text, colors, ink, onOpenLink) }
    Text(
        text = annotated,
        style = headingStyle(level),
        color = ink,
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
    text: List<InlineRun>,
    ink: Color,
    onOpenLink: (OpenableLink) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val annotated = remember(text, colors, ink, onOpenLink) { annotatedRuns(text, colors, ink, onOpenLink) }
    SelectionContainer(modifier = modifier) {
        Text(
            text = annotated,
            style = typography.body,
            color = ink,
        )
    }
}

/**
 * A list: each item's marker sits in a column as wide as the list's widest
 * marker, numbers right-aligned in it (`9.` over `10.`), so every item's
 * words start at one edge. Sized to each marker alone, item 10's words
 * would start further right than item 9's.
 */
@Composable
private fun MarkdownListBlockView(
    ordered: Boolean,
    depth: Int,
    start: Long,
    items: List<ProseItem>,
    ink: Color,
    streaming: Boolean,
    onOpenLink: (OpenableLink) -> Unit,
    onCopyCode: (String) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val markers = remember(ordered, start, items) {
        items.mapIndexed { index, item ->
            when (item.task) {
                true -> "\u2713"
                false -> "\u25CB"
                null -> if (ordered) "${start + index}." else "\u2022"
            }
        }
    }
    val measurer = rememberTextMeasurer()
    val density = LocalDensity.current
    val column = remember(markers, typography.body, density) {
        with(density) {
            markers.maxOfOrNull { measurer.measure(it, style = typography.body).size.width }?.toDp() ?: 0.dp
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(Space.xs + Space.xxs)) {
        items.forEachIndexed { index, item ->
            // A task that is done recedes, as its marker does.
            val itemInk = if (item.task == true) colors.inkFaint else ink
            Row(
                horizontalArrangement = Arrangement.spacedBy(Space.s),
                verticalAlignment = Alignment.Top,
            ) {
                Text(
                    text = markers[index],
                    style = typography.body,
                    color = colors.inkFaint,
                    textAlign = TextAlign.End,
                    modifier = Modifier.width(column),
                )
                Column(
                    modifier = Modifier
                        .weight(1f, fill = false)
                        .testTag(MarkdownViewTags.item(depth, index)),
                    verticalArrangement = Arrangement.spacedBy(Space.xs + Space.xxs),
                ) {
                    MarkdownBlocks(
                        blocks = item.blocks,
                        ink = itemInk,
                        streaming = streaming,
                        onOpenLink = onOpenLink,
                        onCopyCode = onCopyCode,
                    )
                }
            }
        }
    }
}

@Suppress("DEPRECATION")
@Composable
private fun MarkdownCodeBlockView(
    language: String,
    text: String,
    onCopyCode: (String) -> Unit,
    /** The block still being written: the open fence of text still arriving. */
    streaming: Boolean = false,
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

    val isDiff = remember(language) { language.equals("diff", ignoreCase = true) }
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
                if (language.isNotEmpty()) {
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
                        // Drawn plain at once, then in the colours of its
                        // tokens as the shared view answers.
                        val answer = rememberHighlighted(
                            CodeHighlighting.Request.Block(code = text, info = language, streaming = streaming),
                        )
                        val coloured = remember(text, answer, colors) {
                            answer?.let { colors.codeText(it.runsFor(codeLines(text))) }
                        }
                        Text(
                            text = coloured ?: AnnotatedString(text),
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
    blocks: List<ProseBlock>,
    streaming: Boolean,
    onOpenLink: (OpenableLink) -> Unit,
    onCopyCode: (String) -> Unit,
) {
    val colors = AmuxTheme.colors
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
            MarkdownBlocks(
                blocks = blocks,
                ink = colors.inkMuted,
                streaming = streaming,
                onOpenLink = onOpenLink,
                onCopyCode = onCopyCode,
            )
        }
    }
}

@Composable
private fun MarkdownTableBlockView(
    alignments: List<TableAlign>,
    header: List<List<InlineRun>>,
    rows: List<List<List<InlineRun>>>,
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
                MarkdownTableRow(cells = header, alignments = alignments, isHeader = true, onOpenLink = onOpenLink)
                rows.forEach { row ->
                    AmuxHairline()
                    MarkdownTableRow(cells = row, alignments = alignments, isHeader = false, onOpenLink = onOpenLink)
                }
            }
        }
    }
}

@Composable
private fun MarkdownTableRow(
    cells: List<List<InlineRun>>,
    alignments: List<TableAlign>,
    isHeader: Boolean,
    onOpenLink: (OpenableLink) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val ink = if (isHeader) colors.inkMuted else colors.ink
    Row(verticalAlignment = Alignment.Top) {
        cells.forEachIndexed { index, cell ->
            val annotated = remember(cell, colors, ink, onOpenLink) { annotatedRuns(cell, colors, ink, onOpenLink) }
            Text(
                text = annotated,
                style = if (isHeader) typography.caption.copy(fontWeight = FontWeight.SemiBold) else typography.monoSmall,
                color = ink,
                textAlign = when (alignments.getOrNull(index)) {
                    TableAlign.Right -> TextAlign.End
                    TableAlign.Center -> TextAlign.Center
                    else -> TextAlign.Start
                },
                modifier = Modifier
                    .width(if (index == 0) 146.dp else 128.dp)
                    .padding(horizontal = Space.s + Space.xxs, vertical = Space.s),
            )
        }
    }
}
