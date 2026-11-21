package sh.amux.testsupport

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.text.TextLayoutResult
import kotlin.math.abs
import kotlin.math.max
import kotlin.math.min
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import kotlinx.serialization.json.putJsonObject

/**
 * Where a story's pieces sit, read from its semantics tree, and the
 * alignment near-misses among them: a centre a dp or two off its row, an
 * edge a few dp off a column everything else shares, a control drawn taller
 * than iOS draws one, a glyph sitting inside the edge its neighbours keep.
 *
 * It is a report for a person or an agent comparing pictures, so a near-miss
 * is seen without zooming; it never fails anything. Rails and other drawn
 * connectors have no semantics and are not in it.
 */
object StoryGeometry {
    /** Two centres on one row further apart than this are flagged. */
    const val ROW_CENTRE_TOLERANCE_DP = 1f

    /** Edges this close count as one column. */
    const val SAME_COLUMN_DP = 0.5f

    /** An edge this far off a shared column (inclusive) is a near-miss rather than a column of its own. */
    const val NEAR_MISS_MIN_DP = 1f
    const val NEAR_MISS_MAX_DP = 12f

    /** iOS draws its buttons no taller than this (its button label's 44 pt minimum), and its pills and chips shorter. */
    const val CONTROL_HEIGHT_DP = 44f

    /** Leaves overlapping by this share of the taller one's height sit on one row. */
    private const val ROW_OVERLAP = 0.6f

    /** Edges of fewer than this many other leaves are no column. */
    private const val COLUMN_MEMBERS = 2

    enum class Kind { Text, Icon, Control }

    /**
     * One piece of a story, in dp from the screen's top left.
     *
     * [drawnTop] and [drawnBottom] are the shape a control draws (its
     * background or border), which Material's minimum touch size can make
     * shorter than its bounds; null when it draws none. [texts] counts the
     * text pieces inside it, so a one-label button can be told from a row.
     */
    data class Element(
        val id: Int,
        val kind: Kind,
        val label: String,
        val left: Float,
        val top: Float,
        val right: Float,
        val bottom: Float,
        val leaf: Boolean = true,
        val drawnTop: Float? = null,
        val drawnBottom: Float? = null,
        val texts: Int = if (kind == Kind.Text) 1 else 0,
        /** A text's first baseline, in dp from the screen's top; null for anything else. */
        val firstBaseline: Float? = null,
    ) {
        val height: Float get() = bottom - top
        val centreY: Float get() = (top + bottom) / 2f
    }

    enum class Rule(val wire: String, val what: String) {
        RowCentre("row-centre", "siblings on one row whose vertical centres differ"),
        NearColumn("near-column", "an edge just off a column other pieces share"),
        TallControl("tall-control", "a control drawn taller than 44 dp"),
        IconInset("icon-inset", "a glyph inset from the edge its neighbours share"),
    }

    data class Flag(val rule: Rule, val elements: List<Int>, val offDp: Float, val detail: String)

    // --- Reading --------------------------------------------------------------

    /**
     * The pieces of the unmerged semantics tree under [root] that are on
     * screen and not cut off by it: text, described glyphs, and controls.
     */
    fun read(root: SemanticsNode, density: Float, viewportWidthPx: Float, viewportHeightPx: Float): List<Element> {
        val out = mutableListOf<Element>()

        fun walk(node: SemanticsNode): Int {
            var texts = 0
            var hasElementBelow = false
            // What a node clears is hidden from TalkBack, as the screen behind
            // a sheet is; the unmerged tree still holds it, so it is not read.
            val children = if (node.config.isClearingSemantics) emptyList() else node.children
            for (child in children) {
                val below = walk(child)
                if (below >= 0) hasElementBelow = true
                texts += maxOf(below, 0)
            }
            val kind = kindOf(node) ?: return if (hasElementBelow) texts else -1
            val bounds = node.boundsInRoot
            val whole = bounds.width >= node.size.width - 1f && bounds.height >= node.size.height - 1f
            val onScreen = bounds.width > 0f && bounds.height > 0f &&
                bounds.right <= viewportWidthPx + 1f && bounds.bottom <= viewportHeightPx + 1f
            val ownTexts = texts + if (kind == Kind.Text) 1 else 0
            if (whole && onScreen) {
                val drawn = if (kind == Kind.Control) drawnShape(node) else null
                out += Element(
                    id = node.id,
                    kind = kind,
                    label = labelOf(node),
                    left = bounds.left / density,
                    top = bounds.top / density,
                    right = bounds.right / density,
                    bottom = bounds.bottom / density,
                    leaf = !hasElementBelow,
                    drawnTop = drawn?.first?.div(density),
                    drawnBottom = drawn?.second?.div(density),
                    texts = ownTexts,
                    firstBaseline = if (kind == Kind.Text) {
                        firstBaselineOf(node)?.let { (bounds.top + it) / density }
                    } else {
                        null
                    },
                )
            }
            return ownTexts
        }

        walk(root)
        return out.sortedBy { it.id }
    }

    private fun kindOf(node: SemanticsNode): Kind? {
        val config = node.config
        if (config.getOrNull(SemanticsProperties.HideFromAccessibility) != null) return null
        val interactive = config.getOrNull(SemanticsActions.OnClick) != null ||
            config.getOrNull(SemanticsProperties.ToggleableState) != null ||
            config.getOrNull(SemanticsActions.SetText) != null
        return when {
            interactive -> Kind.Control
            config.getOrNull(SemanticsProperties.Role) == Role.Image -> Kind.Icon
            !config.getOrNull(SemanticsProperties.Text).isNullOrEmpty() -> Kind.Text
            !config.getOrNull(SemanticsProperties.ContentDescription).isNullOrEmpty() && node.children.isEmpty() -> Kind.Icon
            else -> null
        }
    }

    /** The first baseline of a text's laid-out lines, in px from its top. */
    private fun firstBaselineOf(node: SemanticsNode): Float? = runCatching {
        val layouts = mutableListOf<TextLayoutResult>()
        node.config.getOrNull(SemanticsActions.GetTextLayoutResult)?.action?.invoke(layouts)
        layouts.firstOrNull()?.firstBaseline
    }.getOrNull()

    private fun labelOf(node: SemanticsNode): String {
        val config = node.config
        val words = config.getOrNull(SemanticsProperties.ContentDescription)?.joinToString(" ")
            ?: config.getOrNull(SemanticsProperties.Text)?.joinToString(" ") { it.text }
            ?: config.getOrNull(SemanticsProperties.EditableText)?.text
            ?: config.getOrNull(SemanticsProperties.TestTag)
            ?: ""
        return words.replace('\n', ' ').take(60)
    }

    /**
     * The top and bottom, in px, of the tallest background or border the
     * control's own modifiers draw. A draw modifier's coordinates are those of
     * the layout inside it, which for a Material control is the shape inside
     * its minimum touch size.
     */
    private fun drawnShape(node: SemanticsNode): Pair<Float, Float>? = runCatching {
        node.layoutInfo.getModifierInfo()
            .filter { it.modifier.javaClass.simpleName in DRAWN_SHAPES && visible(it.modifier) }
            .map { it.coordinates.boundsInRoot() }
            .maxByOrNull { it.height }
            ?.let { it.top to it.bottom }
    }.getOrNull()

    private val DRAWN_SHAPES = setOf("BackgroundElement", "BorderModifierNodeElement")

    /**
     * Whether a background paints anything: an icon button's ripple bounds
     * carry a transparent one the size of its touch target, which is no
     * shape a person sees. Compose keeps the colour in a private field, read
     * here by reflection; when that fails the shape counts as drawn.
     */
    private fun visible(modifier: Any): Boolean = runCatching {
        if (modifier.javaClass.simpleName != "BackgroundElement") return@runCatching true
        fun field(name: String) = modifier.javaClass.getDeclaredField(name).apply { isAccessible = true }
        val brush = runCatching { field("brush").get(modifier) }.getOrNull()
        val color = Color(field("color").getLong(modifier).toULong())
        val alpha = runCatching { field("alpha").getFloat(modifier) }.getOrDefault(1f)
        alpha > 0f && (brush != null || color.alpha > 0f)
    }.getOrDefault(true)

    // --- Flagging -------------------------------------------------------------

    fun flags(elements: List<Element>): List<Flag> {
        val leaves = elements.filter { it.leaf }
        return rowCentres(leaves) + nearColumns(leaves) + tallControls(elements) + iconInsets(leaves)
    }

    private fun overlap(a: Element, b: Element): Float = min(a.bottom, b.bottom) - max(a.top, b.top)

    private fun sameRow(a: Element, b: Element): Boolean = overlap(a, b) >= ROW_OVERLAP * max(a.height, b.height)

    /** The nearest leaf on [of]'s row to its right, if any. */
    private fun nextOnRow(of: Element, leaves: List<Element>): Element? = leaves
        .filter { it !== of && it.left >= of.right - SAME_COLUMN_DP && sameRow(of, it) }
        .minByOrNull { it.left }

    private fun previousOnRow(of: Element, leaves: List<Element>): Element? = leaves
        .filter { it !== of && it.right <= of.left + SAME_COLUMN_DP && sameRow(of, it) }
        .maxByOrNull { it.right }

    /**
     * Texts of different sizes set on one baseline, as a name and its facts
     * are, have centres that differ and are aligned all the same, so they
     * are not flagged.
     */
    private fun rowCentres(leaves: List<Element>): List<Flag> = leaves.mapNotNull { a ->
        val b = nextOnRow(a, leaves) ?: return@mapNotNull null
        val baselines = a.firstBaseline?.let { first -> b.firstBaseline?.let { abs(first - it) } }
        if (baselines != null && baselines <= ROW_CENTRE_TOLERANCE_DP) return@mapNotNull null
        val off = abs(a.centreY - b.centreY)
        if (off <= ROW_CENTRE_TOLERANCE_DP) return@mapNotNull null
        Flag(Rule.RowCentre, listOf(a.id, b.id), off, "\"${a.label}\" and \"${b.label}\" centres ${dp(off)} apart")
    }

    /** Columns of [edge]s that at least [COLUMN_MEMBERS] leaves other than [except] share, with how many. */
    private fun columns(leaves: List<Element>, except: Element, edge: (Element) -> Float): List<Pair<Float, Int>> {
        val others = leaves.filter { it !== except }.map(edge).sorted()
        val found = mutableListOf<Pair<Float, Int>>()
        var start = 0
        while (start < others.size) {
            var end = start
            while (end + 1 < others.size && others[end + 1] - others[start] <= SAME_COLUMN_DP) end += 1
            val members = end - start + 1
            if (members >= COLUMN_MEMBERS) found += others[start] to members
            start = end + 1
        }
        return found
    }

    /**
     * A column near [value] (off by [NEAR_MISS_MIN_DP]..[NEAR_MISS_MAX_DP])
     * when none is at it; [inward] keeps only columns on that side.
     */
    private fun nearMiss(value: Float, columns: List<Pair<Float, Int>>, inward: ((Float) -> Boolean)? = null): Pair<Float, Int>? {
        if (columns.any { abs(it.first - value) <= SAME_COLUMN_DP }) return null
        return columns
            .filter { abs(it.first - value) in NEAR_MISS_MIN_DP..NEAR_MISS_MAX_DP }
            .filter { inward == null || inward(it.first) }
            .minByOrNull { abs(it.first - value) }
    }

    private fun nearColumns(leaves: List<Element>): List<Flag> = leaves.filter { it.kind != Kind.Icon }.flatMap { e ->
        // Where a piece in the middle of a row starts or ends follows its
        // neighbours' widths; only a row's first piece is aligned by its left
        // edge, and its last by its right.
        val leading = previousOnRow(e, leaves) == null
        val left = if (leading) {
            nearMiss(e.left, columns(leaves, e) { it.left })?.let { (column, members) ->
                Flag(Rule.NearColumn, listOf(e.id), abs(e.left - column), "\"${e.label}\" left edge ${dp(e.left)} is ${dp(abs(e.left - column))} off the ${dp(column)} column ($members share it)")
            }
        } else {
            null
        }
        val right = if (!leading && nextOnRow(e, leaves) == null) {
            nearMiss(e.right, columns(leaves, e) { it.right })?.let { (column, members) ->
                Flag(Rule.NearColumn, listOf(e.id), abs(e.right - column), "\"${e.label}\" right edge ${dp(e.right)} is ${dp(abs(e.right - column))} off the ${dp(column)} column ($members share it)")
            }
        } else {
            null
        }
        listOfNotNull(left, right)
    }

    private fun tallControls(elements: List<Element>): List<Flag> = elements.mapNotNull { e ->
        val top = e.drawnTop ?: return@mapNotNull null
        val bottom = e.drawnBottom ?: return@mapNotNull null
        val drawn = bottom - top
        if (e.kind != Kind.Control || e.texts > 1 || drawn <= CONTROL_HEIGHT_DP) return@mapNotNull null
        Flag(Rule.TallControl, listOf(e.id), drawn - CONTROL_HEIGHT_DP, "\"${e.label}\" drawn ${dp(drawn)} tall")
    }

    private fun iconInsets(leaves: List<Element>): List<Flag> = leaves.filter { it.kind == Kind.Icon }.mapNotNull { icon ->
        when {
            previousOnRow(icon, leaves) == null ->
                nearMiss(icon.left, columns(leaves, icon) { it.left }) { it < icon.left }?.let { (column, members) ->
                    Flag(Rule.IconInset, listOf(icon.id), icon.left - column, "\"${icon.label}\" glyph starts ${dp(icon.left - column)} inside the ${dp(column)} column ($members share it)")
                }
            nextOnRow(icon, leaves) == null ->
                nearMiss(icon.right, columns(leaves, icon) { it.right }) { it > icon.right }?.let { (column, members) ->
                    Flag(Rule.IconInset, listOf(icon.id), column - icon.right, "\"${icon.label}\" glyph ends ${dp(column - icon.right)} inside the ${dp(column)} column ($members share it)")
                }
            else -> null
        }
    }

    private fun dp(value: Float): String = "%.1fdp".format(value)

    // --- Writing --------------------------------------------------------------

    fun report(
        storyId: String,
        appearance: String,
        viewportWidthDp: Float,
        viewportHeightDp: Float,
        elements: List<Element>,
        flags: List<Flag>,
    ): JsonObject = buildJsonObject {
        put("id", storyId)
        put("appearance", appearance)
        putJsonObject("viewport") {
            put("width", viewportWidthDp)
            put("height", viewportHeightDp)
        }
        putJsonArray("flags") {
            for (flag in flags) {
                add(
                    buildJsonObject {
                        put("rule", flag.rule.wire)
                        put("offDp", flag.offDp)
                        put("detail", flag.detail)
                        putJsonArray("elements") { flag.elements.forEach { add(kotlinx.serialization.json.JsonPrimitive(it)) } }
                    },
                )
            }
        }
        putJsonArray("elements") {
            for (e in elements) {
                add(
                    buildJsonObject {
                        put("id", e.id)
                        put("kind", e.kind.name.lowercase())
                        put("label", e.label)
                        put("leaf", e.leaf)
                        put("left", e.left)
                        put("top", e.top)
                        put("right", e.right)
                        put("bottom", e.bottom)
                        e.drawnTop?.let { put("drawnTop", it) }
                        e.drawnBottom?.let { put("drawnBottom", it) }
                        e.firstBaseline?.let { put("firstBaseline", it) }
                    },
                )
            }
        }
    }
}
