package sh.amux.words

/**
 * One styled run within parsed inline markdown.
 *
 * Links are underlined rather than coloured: colour is reserved for attention
 * and failures, and links this phone will not open stay as plain text.
 */
data class MarkdownSpan(
    val text: String,
    val bold: Boolean = false,
    val italic: Boolean = false,
    val strikethrough: Boolean = false,
    val code: Boolean = false,
    val link: OpenableLink? = null,
) {
    val underlined: Boolean
        get() = link != null

    internal fun sameStyle(other: MarkdownSpan): Boolean =
        bold == other.bold &&
            italic == other.italic &&
            strikethrough == other.strikethrough &&
            code == other.code &&
            link == other.link
}

/**
 * Resolved inline markdown: a flat sequence of styled [MarkdownSpan] runs.
 */
data class MarkdownText(
    val spans: List<MarkdownSpan>,
) {
    val plainText: String
        get() = spans.joinToString("") { it.text }

    val links: List<OpenableLink>
        get() = spans.mapNotNull { it.link }

    companion object {
        val EMPTY: MarkdownText = MarkdownText(emptyList())

        fun plain(text: String): MarkdownText =
            if (text.isEmpty()) EMPTY else MarkdownText(listOf(MarkdownSpan(text)))
    }
}

/**
 * One block of an agent's prose.
 */
sealed interface MarkdownBlock {
    data class Heading(val level: Int, val text: MarkdownText) : MarkdownBlock
    data class Paragraph(val text: MarkdownText) : MarkdownBlock

    /**
     * A list. [ordered] decides whether the marker is a number or a dot, and
     * [Item.depth] how far each item is indented.
     */
    data class ListBlock(val ordered: Boolean, val items: List<Item>) : MarkdownBlock

    /**
     * Fenced code, with whatever the fence named as its language. Never
     * wrapped: code that wraps stops being code, so it scrolls sideways.
     */
    data class Code(val language: String?, val text: String) : MarkdownBlock
    data class Quote(val lines: List<MarkdownText>) : MarkdownBlock
    data class Table(
        val header: List<MarkdownText>,
        val rows: List<List<MarkdownText>>,
    ) : MarkdownBlock

    data object Rule : MarkdownBlock

    data class Item(
        val id: Int,
        val depth: Int,
        val marker: String,
        val text: MarkdownText,
    )
}

/**
 * Prose, parsed into blocks and styled inline spans on a pure JVM model so
 * parsing can run off the main thread before Compose renders a frame.
 */
data class MarkdownDocument(
    val blocks: List<MarkdownBlock>,
) {
    companion object {
        private data class ListMarker(
            val ordered: Boolean,
            val depth: Int,
            val marker: String,
            val rest: String,
        )

        private data class InlineStyle(
            val bold: Boolean = false,
            val italic: Boolean = false,
            val strikethrough: Boolean = false,
            val link: OpenableLink? = null,
        )

        fun parse(source: String): MarkdownDocument {
            val normalized = source.replace("\r\n", "\n")
            val lines = normalized.split('\n')
            val blocks = mutableListOf<MarkdownBlock>()
            var index = 0
            var counter = 0

            fun nextId(): Int {
                counter += 1
                return counter
            }

            while (index < lines.size) {
                val line = lines[index]
                val trimmed = trimSpaces(line)

                if (trimmed.isEmpty()) {
                    index += 1
                    continue
                }

                val fenceLang = fence(trimmed)
                if (fenceLang != null) {
                    index += 1
                    val body = mutableListOf<String>()
                    while (index < lines.size && fence(trimSpaces(lines[index])) == null) {
                        body.add(lines[index])
                        index += 1
                    }
                    if (index < lines.size) {
                        index += 1
                    }
                    blocks.add(
                        MarkdownBlock.Code(
                            language = fenceLang.ifEmpty { null },
                            text = body.joinToString("\n"),
                        ),
                    )
                    continue
                }

                val level = if (trimmed.startsWith("#")) headingLevel(trimmed) else null
                if (level != null) {
                    index += 1
                    val text = trimSpaces(trimmed.drop(level))
                    blocks.add(MarkdownBlock.Heading(level = level, text = inline(text)))
                    continue
                }

                if (isRule(trimmed)) {
                    index += 1
                    blocks.add(MarkdownBlock.Rule)
                    continue
                }

                if (trimmed.startsWith("|") && isTableRow(trimmed)) {
                    val rows = mutableListOf<List<String>>()
                    while (index < lines.size) {
                        val rowTrimmed = trimSpaces(lines[index])
                        if (!isTableRow(rowTrimmed)) break
                        rows.add(cells(rowTrimmed))
                        index += 1
                    }
                    // The second row of a markdown table is the alignment rule,
                    // which is punctuation rather than content and is dropped.
                    val body = if (rows.size > 1 && rows[1].all(::isAlignmentCell)) {
                        rows.drop(2)
                    } else {
                        rows.drop(1)
                    }
                    blocks.add(
                        MarkdownBlock.Table(
                            header = (rows.firstOrNull() ?: emptyList()).map(::inline),
                            rows = body.map { row -> row.map(::inline) },
                        ),
                    )
                    continue
                }

                if (trimmed.startsWith(">")) {
                    val quoted = mutableListOf<MarkdownText>()
                    while (index < lines.size) {
                        val qTrimmed = trimSpaces(lines[index])
                        if (!qTrimmed.startsWith(">")) break
                        val text = trimSpaces(qTrimmed.drop(1))
                        quoted.add(inline(text))
                        index += 1
                    }
                    blocks.add(MarkdownBlock.Quote(quoted))
                    continue
                }

                val firstMarker = listMarker(line)
                if (firstMarker != null) {
                    val items = mutableListOf<MarkdownBlock.Item>()
                    val ordered = firstMarker.ordered
                    while (index < lines.size) {
                        val marker = listMarker(lines[index]) ?: break
                        if (marker.ordered != ordered) break
                        items.add(
                            MarkdownBlock.Item(
                                id = nextId(),
                                depth = marker.depth,
                                marker = marker.marker,
                                text = inline(marker.rest),
                            ),
                        )
                        index += 1
                    }
                    blocks.add(MarkdownBlock.ListBlock(ordered = ordered, items = items))
                    continue
                }

                // A paragraph always takes the line that opened it so no line
                // can ever be left unconsumed. Later lines join it until one
                // opens a block of its own.
                val paragraph = mutableListOf(trimmed)
                index += 1
                while (index < lines.size && !opensBlock(lines[index])) {
                    paragraph.add(trimSpaces(lines[index]))
                    index += 1
                }
                // A single newline stays a line break. A blank line still parts
                // two paragraphs.
                blocks.add(MarkdownBlock.Paragraph(inline(paragraph.joinToString("\n"))))
            }

            return MarkdownDocument(blocks)
        }

        /**
         * Resolves inline markdown — emphasis, strikethrough, code spans, and
         * links — while preserving single newlines and falling back to literal
         * text for unmatched delimiters.
         */
        fun inline(source: String): MarkdownText {
            if (source.isEmpty()) return MarkdownText.EMPTY
            val spans = mutableListOf<MarkdownSpan>()
            parseInlineInto(source, InlineStyle(), spans)
            return MarkdownText(coalesce(spans))
        }

        private fun parseInlineInto(
            source: String,
            style: InlineStyle,
            out: MutableList<MarkdownSpan>,
        ) {
            val buffer = StringBuilder()
            var i = 0
            val n = source.length

            fun flush() {
                if (buffer.isNotEmpty()) {
                    out.add(
                        MarkdownSpan(
                            text = buffer.toString(),
                            bold = style.bold,
                            italic = style.italic,
                            strikethrough = style.strikethrough,
                            code = false,
                            link = style.link,
                        ),
                    )
                    buffer.setLength(0)
                }
            }

            while (i < n) {
                val ch = source[i]

                // Backslash escape for ASCII punctuation.
                if (ch == '\\' && i + 1 < n && isEscapable(source[i + 1])) {
                    buffer.append(source[i + 1])
                    i += 2
                    continue
                }

                // Code span (`...` or ``...``).
                if (ch == '`') {
                    var tickCount = 0
                    while (i + tickCount < n && source[i + tickCount] == '`') tickCount++
                    val closeIndex = findClosingBackticks(source, i + tickCount, tickCount)
                    if (closeIndex >= 0) {
                        flush()
                        val raw = source.substring(i + tickCount, closeIndex)
                        val content = if (raw.length >= 2 && raw.startsWith(' ') && raw.endsWith(' ') && raw.any { it != ' ' }) {
                            raw.substring(1, raw.length - 1)
                        } else {
                            raw
                        }
                        if (content.isNotEmpty()) {
                            out.add(
                                MarkdownSpan(
                                    text = content,
                                    bold = style.bold,
                                    italic = style.italic,
                                    strikethrough = style.strikethrough,
                                    code = true,
                                    link = style.link,
                                ),
                            )
                        }
                        i = closeIndex + tickCount
                        continue
                    } else {
                        buffer.append(source, i, i + tickCount)
                        i += tickCount
                        continue
                    }
                }

                // Inline link: [label](destination)
                if (ch == '[' && style.link == null) {
                    val parsed = parseInlineLink(source, i)
                    if (parsed != null) {
                        flush()
                        val openable = OpenableLink.parse(parsed.destination)
                        val childStyle = if (openable != null) style.copy(link = openable) else style
                        parseInlineInto(parsed.label, childStyle, out)
                        i = parsed.nextIndex
                        continue
                    }
                }

                // Autolink: <scheme:...>
                if (ch == '<' && style.link == null) {
                    val parsed = parseAutolink(source, i)
                    if (parsed != null) {
                        flush()
                        val openable = OpenableLink.parse(parsed.target)
                        val text = parsed.displayText
                        out.add(
                            MarkdownSpan(
                                text = text,
                                bold = style.bold,
                                italic = style.italic,
                                strikethrough = style.strikethrough,
                                code = false,
                                link = openable,
                            ),
                        )
                        i = parsed.nextIndex
                        continue
                    }
                }

                // Strikethrough: ~~text~~
                if (ch == '~' && i + 1 < n && source[i + 1] == '~' && !style.strikethrough) {
                    val close = findClosingDelimiter(source, i + 2, "~~", checkWordBoundary = false)
                    if (close >= 0) {
                        flush()
                        parseInlineInto(source.substring(i + 2, close), style.copy(strikethrough = true), out)
                        i = close + 2
                        continue
                    }
                }

                // Emphasis / Strong: ***, **, *, ___, __, _
                if (ch == '*' || ch == '_') {
                    val checkWordBoundary = ch == '_'
                    val prevIsWord = i > 0 && source[i - 1].isLetterOrDigit()
                    if (!checkWordBoundary || !prevIsWord) {
                        val delimLen = when {
                            i + 2 < n && source[i + 1] == ch && source[i + 2] == ch && !style.bold && !style.italic -> 3
                            i + 1 < n && source[i + 1] == ch && !style.bold -> 2
                            !style.italic -> 1
                            else -> 0
                        }
                        if (delimLen > 0) {
                            val delim = ch.toString().repeat(delimLen)
                            val close = findClosingDelimiter(source, i + delimLen, delim, checkWordBoundary)
                            if (close >= 0) {
                                flush()
                                val childStyle = when (delimLen) {
                                    3 -> style.copy(bold = true, italic = true)
                                    2 -> style.copy(bold = true)
                                    else -> style.copy(italic = true)
                                }
                                parseInlineInto(source.substring(i + delimLen, close), childStyle, out)
                                i = close + delimLen
                                continue
                            }
                        }
                    }
                }

                buffer.append(ch)
                i += 1
            }

            flush()
        }

        private data class ParsedLink(
            val label: String,
            val destination: String,
            val nextIndex: Int,
        )

        private data class ParsedAutolink(
            val target: String,
            val displayText: String,
            val nextIndex: Int,
        )

        private fun parseInlineLink(source: String, start: Int): ParsedLink? {
            val n = source.length
            var bracketDepth = 0
            var i = start
            var closeBracket = -1
            while (i < n) {
                val c = source[i]
                if (c == '\\' && i + 1 < n) {
                    i += 2
                    continue
                }
                if (c == '`') {
                    var ticks = 0
                    while (i + ticks < n && source[i + ticks] == '`') ticks++
                    val close = findClosingBackticks(source, i + ticks, ticks)
                    if (close >= 0) {
                        i = close + ticks
                        continue
                    }
                    i += ticks
                    continue
                }
                if (c == '[') {
                    bracketDepth++
                } else if (c == ']') {
                    bracketDepth--
                    if (bracketDepth == 0) {
                        closeBracket = i
                        break
                    }
                }
                i++
            }
            if (closeBracket < 0 || closeBracket + 1 >= n || source[closeBracket + 1] != '(') {
                return null
            }

            val destStart = closeBracket + 2
            var j = destStart
            // Optional <destination> form.
            if (j < n && source[j] == '<') {
                val angleEnd = source.indexOf('>', j + 1)
                if (angleEnd < 0 || angleEnd + 1 >= n || source[angleEnd + 1] != ')') return null
                val dest = source.substring(j + 1, angleEnd).trim()
                val label = source.substring(start + 1, closeBracket)
                return ParsedLink(label = label, destination = dest, nextIndex = angleEnd + 2)
            }

            var parenDepth = 1
            var closeParen = -1
            while (j < n) {
                val c = source[j]
                if (c == '\\' && j + 1 < n) {
                    j += 2
                    continue
                }
                if (c == '\n') return null
                if (c == '(') {
                    parenDepth++
                } else if (c == ')') {
                    parenDepth--
                    if (parenDepth == 0) {
                        closeParen = j
                        break
                    }
                }
                j++
            }
            if (closeParen < 0) return null
            val rawDest = source.substring(destStart, closeParen).trim()
            if (rawDest.isEmpty()) return null
            // Strip optional trailing title (`url "title"`) if present.
            val destination = rawDest.takeWhile { !it.isWhitespace() }
            val label = source.substring(start + 1, closeBracket)
            return ParsedLink(label = label, destination = destination, nextIndex = closeParen + 1)
        }

        private fun parseAutolink(source: String, start: Int): ParsedAutolink? {
            val close = source.indexOf('>', start + 1)
            if (close < 0) return null
            val inner = source.substring(start + 1, close)
            if (inner.isEmpty() || inner.any { it.isWhitespace() || it == '<' }) return null
            val colon = inner.indexOf(':')
            if (colon <= 0) return null
            val scheme = inner.substring(0, colon)
            if (!scheme[0].isLetter() || !scheme.all { it.isLetterOrDigit() || it == '+' || it == '-' || it == '.' }) {
                return null
            }
            return ParsedAutolink(target = inner, displayText = inner, nextIndex = close + 1)
        }

        private fun findClosingBackticks(source: String, from: Int, tickCount: Int): Int {
            var i = from
            val n = source.length
            while (i < n) {
                if (source[i] == '`') {
                    var count = 0
                    while (i + count < n && source[i + count] == '`') count++
                    if (count == tickCount) return i
                    i += count
                } else {
                    i++
                }
            }
            return -1
        }

        private fun findClosingDelimiter(
            source: String,
            contentStart: Int,
            delim: String,
            checkWordBoundary: Boolean,
        ): Int {
            val n = source.length
            if (contentStart >= n || source[contentStart].isWhitespace()) return -1
            val delimLen = delim.length
            var i = contentStart
            while (i + delimLen <= n) {
                val c = source[i]
                if (c == '\\' && i + 1 < n) {
                    i += 2
                    continue
                }
                if (c == '`') {
                    var ticks = 0
                    while (i + ticks < n && source[i + ticks] == '`') ticks++
                    val close = findClosingBackticks(source, i + ticks, ticks)
                    if (close >= 0) {
                        i = close + ticks
                        continue
                    }
                    i += ticks
                    continue
                }
                if (source.startsWith(delim, i)) {
                    val before = source[i - 1]
                    val afterIdx = i + delimLen
                    val afterIsWord = afterIdx < n && source[afterIdx].isLetterOrDigit()
                    if (!before.isWhitespace() && (!checkWordBoundary || !afterIsWord)) {
                        // Avoid matching a longer run of the same character when looking for a shorter one,
                        // unless it's closing an inner span before an outer one.
                        val sameCharAfter = afterIdx < n && source[afterIdx] == delim[0]
                        if (delimLen == 1 && sameCharAfter) {
                            // Skip double delimiter inside single emphasis (e.g. *a **b** c*).
                            var run = 0
                            while (i + run < n && source[i + run] == delim[0]) run++
                            if (run == 2) {
                                i += 2
                                continue
                            }
                        }
                        return i
                    }
                }
                i++
            }
            return -1
        }

        private fun coalesce(spans: List<MarkdownSpan>): List<MarkdownSpan> {
            if (spans.size <= 1) return spans
            val merged = mutableListOf<MarkdownSpan>()
            for (span in spans) {
                if (span.text.isEmpty()) continue
                val last = merged.lastOrNull()
                if (last != null && last.sameStyle(span)) {
                    merged[merged.lastIndex] = last.copy(text = last.text + span.text)
                } else {
                    merged.add(span)
                }
            }
            return merged
        }

        private fun isEscapable(ch: Char): Boolean =
            ch in "\\`*_{}[]()#+-.!~<>"

        private fun opensBlock(line: String): Boolean {
            val text = trimSpaces(line)
            return text.isEmpty() ||
                fence(text) != null ||
                headingLevel(text) != null ||
                isRule(text) ||
                isTableRow(text) ||
                text.startsWith(">") ||
                listMarker(line) != null
        }

        private fun fence(line: String): String? {
            if (!line.startsWith("```") && !line.startsWith("~~~")) return null
            return trimSpaces(line.drop(3))
        }

        private fun headingLevel(line: String): Int? {
            // The marks are followed by a space or end the line: `#42` and a
            // shebang are prose, and `##` alone is a heading whose title has not
            // been written yet, as it is while a reply streams in.
            val hashes = line.takeWhile { it == '#' }.length
            if (hashes !in 1..6) return null
            val after = line.drop(hashes)
            return if (after.isEmpty() || after.startsWith(" ")) hashes else null
        }

        private fun isRule(trimmed: String): Boolean =
            trimmed.length >= 3 && trimmed.all { it == '-' || it == '*' || it == '_' }

        private fun isTableRow(line: String): Boolean =
            line.startsWith("|") && line.drop(1).contains('|')

        private fun isAlignmentCell(cell: String): Boolean =
            cell.isNotEmpty() && cell.all { it == '-' || it == ':' || it == ' ' }

        private fun cells(line: String): List<String> {
            var text = line
            if (text.startsWith("|")) text = text.drop(1)
            if (text.endsWith("|")) text = text.dropLast(1)
            return text.split('|').map(::trimSpaces)
        }

        private fun listMarker(line: String): ListMarker? {
            val indent = line.takeWhile { it == ' ' || it == '\t' }.length
            val body = line.drop(indent)
            val depth = minOf(2, indent / 2)
            val first = body.firstOrNull()
            if (first != null && (first == '-' || first == '*' || first == '+') && body.drop(1).startsWith(" ")) {
                return ListMarker(
                    ordered = false,
                    depth = depth,
                    marker = "\u2022",
                    rest = trimSpaces(body.drop(2)),
                )
            }
            val digits = body.takeWhile { it.isDigit() }
            if (digits.isNotEmpty() && body.drop(digits.length).startsWith(". ")) {
                return ListMarker(
                    ordered = true,
                    depth = depth,
                    marker = "$digits.",
                    rest = trimSpaces(body.drop(digits.length + 2)),
                )
            }
            return null
        }

        private fun trimSpaces(text: String): String =
            text.trim { it == ' ' || it == '\t' }
    }
}
