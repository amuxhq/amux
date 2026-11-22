package sh.amux.testsupport

import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import sh.amux.features.ReportedLabel
import sh.amux.features.ReportedValue

/**
 * How the debug app's driving door reads a tagged element's label and value
 * from the semantics tree, in one place so the Robolectric tests that guard
 * the journeys' ids read them exactly as the door does.
 */
object DoorReading {
    /**
     * What the screen declared for the door, else the element's own
     * description and text, else the words of its untagged descendants.
     */
    fun label(node: SemanticsNode): String? {
        node.config.getOrNull(ReportedLabel)?.let { return it }
        val direct = parts(node)
        if (direct.isNotEmpty()) {
            return direct.joinToString(", ")
        }
        val nested = mutableListOf<String>()
        collectDescendantText(node, nested, maxDepth = 6)
        return nested.distinct().takeIf { it.isNotEmpty() }?.joinToString(", ")
    }

    /**
     * What the screen declared for the door, else the text in a field, else
     * the element's spoken state, else "selected" for a chosen tab or option
     * that says nothing more, as the iPhone door reports one.
     */
    fun value(node: SemanticsNode): String? {
        node.config.getOrNull(ReportedValue)?.let { return it }
        val editable = node.config.getOrNull(SemanticsProperties.EditableText)?.text
        if (editable != null) return editable
        node.config.getOrNull(SemanticsProperties.StateDescription)?.let { return it }
        return if (node.config.getOrNull(SemanticsProperties.Selected) == true) "selected" else null
    }

    /** The element's own description and text, each once. */
    fun parts(node: SemanticsNode): List<String> {
        val parts = mutableListOf<String>()
        node.config.getOrNull(SemanticsProperties.ContentDescription)
            ?.filter { it.isNotBlank() }
            ?.let { parts.addAll(it) }
        node.config.getOrNull(SemanticsProperties.Text)
            ?.map { it.text }
            ?.filter { it.isNotBlank() }
            ?.let { parts.addAll(it) }
        return parts.distinct()
    }

    private fun collectDescendantText(
        node: SemanticsNode,
        out: MutableList<String>,
        maxDepth: Int,
    ) {
        if (maxDepth <= 0) return
        for (child in node.children) {
            out.addAll(parts(child))
            // A tagged descendant is an element of its own and speaks for
            // its own subtree.
            if (child.config.getOrNull(SemanticsProperties.TestTag).isNullOrEmpty()) {
                collectDescendantText(child, out, maxDepth - 1)
            }
        }
    }
}
