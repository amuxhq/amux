package sh.amux.words

import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.RunCounts

/**
 * How a grid row joins the rail: whether the next row drawn below it is on
 * the rail too, and whether it is a subagent's step, set in under its
 * parent's text.
 */
data class RailJoin(
    val continues: Boolean = false,
    val nested: Boolean = false,
) {
    companion object {
        val NONE: RailJoin = RailJoin()

        /** The join for [row] with [next] drawn below it. */
        fun of(row: Row, next: Row?): RailJoin {
            if (!onRail(row)) return NONE
            return RailJoin(
                continues = next?.let(::onRail) ?: false,
                nested = row.parent != null,
            )
        }

        /**
         * Everything drawn on the grid hangs off the rail; what is read rather
         * than scanned — the prompt, the prose — and the rules and footers that
         * close a turn break it.
         */
        fun onRail(row: Row): Boolean = when (row.kind) {
            is RowKind.Prompt,
            is RowKind.Prose,
            is RowKind.TurnEnd,
            is RowKind.Compaction,
            is RowKind.Boundary,
            RowKind.Hidden,
            -> false
            else -> true
        }
    }
}

/**
 * Where a row stands in its run of tool steps, placed as the terminal
 * places it from the same shared fold.
 */
sealed interface RunPlacement {
    /**
     * Outside a run, or a step of an opened run with no line above it:
     * drawn as itself.
     */
    data object Own : RunPlacement

    /**
     * A step drawn as itself under its run's line: the opened run's first
     * step, or the oldest of a live run's newest steps.
     */
    data class Step(val header: Header) : RunPlacement

    /** A finished run's newest step, which draws as the run's one line. */
    data class Folded(val words: String) : RunPlacement

    /** A step a folded run keeps in view: a failure its turn never fixed. */
    data object Failed : RunPlacement

    data class Header(
        val words: String,
        val open: Boolean,
    )

    companion object {
        /**
         * The placement of a row the chat draws: [open] is whether its run is
         * open, [counts] what the run's steps did.
         */
        fun of(row: Row, open: Boolean, counts: () -> RunCounts?): RunPlacement {
            val run = row.run ?: return Own
            if (open) {
                // Opened, a run lists every step on its own line, under the
                // line that folds it again.
                if (row.id != run.first) return Own
                return Step(Header(words = ChatWords.run(run, counts = counts()), open = true))
            }
            if (run.live) {
                // Under way: the newest step and any still running, under what
                // the steps it no longer shows did.
                val hidden = run.hidden ?: return Own
                return Step(Header(words = ChatWords.run(run, counts = hidden), open = false))
            }
            if (run.recent == 0) {
                return Folded(words = ChatWords.run(run, counts = counts()))
            }
            return Failed
        }
    }
}
