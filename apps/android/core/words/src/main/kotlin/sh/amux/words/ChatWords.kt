package sh.amux.words

import sh.amux.push.PushWords
import sh.amux.values.ActivityKind
import sh.amux.values.AgentNameProblem
import sh.amux.values.AnswerView
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.AttachmentView
import sh.amux.values.Attention
import sh.amux.values.BoundaryKind
import sh.amux.values.CallPhase
import sh.amux.values.Changeability
import sh.amux.values.Changeable
import sh.amux.values.Choice
import sh.amux.values.ChoiceOutcome
import sh.amux.values.CommandView
import sh.amux.values.Comparison
import sh.amux.values.Composer
import sh.amux.values.ContextView
import sh.amux.values.ControlsSummary
import sh.amux.values.Decision
import sh.amux.values.DecisionView
import sh.amux.values.DiffLine
import sh.amux.values.ExitCause
import sh.amux.values.ExploreVerb
import sh.amux.values.FieldProblem
import sh.amux.values.FileChangeView
import sh.amux.values.FileRow
import sh.amux.values.FleetCard
import sh.amux.values.GroupState
import sh.amux.values.JobRow
import sh.amux.values.Kind
import sh.amux.values.LineKind
import sh.amux.values.ModeChoice
import sh.amux.values.Offline
import sh.amux.values.PermissionChoice
import sh.amux.values.PermissionGrant
import sh.amux.values.PhaseView
import sh.amux.values.QueuedRow
import sh.amux.values.Reach
import sh.amux.values.Resolution
import sh.amux.values.ReviewStatus
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Run
import sh.amux.values.RunCounts
import sh.amux.values.Scope
import sh.amux.values.Segment
import sh.amux.values.ServerView
import sh.amux.values.SettingsView
import sh.amux.values.SignInState
import sh.amux.values.SignInView
import sh.amux.values.TaskMark
import sh.amux.values.TasksView
import sh.amux.values.Underway
import sh.amux.values.UsageLabel
import sh.amux.values.UsageState
import sh.amux.values.UsageView
import sh.amux.values.UsageWindowView
import sh.amux.values.Waiting
import java.math.BigDecimal
import java.time.Instant
import java.time.ZoneId
import java.util.Locale
import kotlin.math.roundToInt

/**
 * How the phone words the chat's typed facts. The rows, the card and the
 * overview arrive as values; every sentence a person reads about them is
 * written here, so each can be read and tested apart from any view.
 */
object ChatWords {
    data class ChipParts(
        val name: String,
        val detail: String,
    )

    enum class AttachmentKind {
        Image,
        File,
        Text,
        Review,
        Page,
        Empty,
    }

    data class ActivityParts(
        val words: String,
        val subject: String?,
        val time: String?,
    )

    data class ControlsChip(
        val model: String,
        val detail: String,
        val unusual: List<String>,
    )

    data class ScopeRow(
        val title: String,
        val detail: String?,
    )

    // Numbers

    /** Why a typed agent name will not do. */
    fun nameProblem(problem: AgentNameProblem): String = when (problem) {
        AgentNameProblem.Empty -> "A name cannot be empty"
        AgentNameProblem.TooLong -> "A name is at most 64 characters"
        AgentNameProblem.Characters -> "Lowercase letters, digits and hyphens, starting with a letter or digit"
    }

    /** What is wrong with a form field's value, under the field. */
    fun fieldProblem(problem: FieldProblem): String = when (problem) {
        FieldProblem.Required -> "Required"
        FieldProblem.NotANumber -> "Enter a number"
        FieldProblem.NotWholeNumber -> "Enter a whole number"
        is FieldProblem.BelowMinimum -> "At least ${formatNumber(problem.minimum)}"
        is FieldProblem.AboveMaximum -> "At most ${formatNumber(problem.maximum)}"
        is FieldProblem.TooShort ->
            if (problem.minLength == 1) "At least 1 character" else "At least ${problem.minLength} characters"
        is FieldProblem.TooLong ->
            if (problem.maxLength == 1) "At most 1 character" else "At most ${problem.maxLength} characters"
        is FieldProblem.TooFew -> "Pick at least ${problem.minItems}"
        is FieldProblem.TooMany -> "Pick at most ${problem.maxItems}"
    }

    /**
     * `"4s"`, `"1m 42s"`, `"2h"`: whole seconds, rounded up. A tenth of a
     * second is precision nobody reads a chat for, and rounding up keeps a
     * turn that ran at all from claiming it took nothing.
     */
    fun duration(ms: Long): String {
        val secs = (maxOf(0L, ms) + 999L) / 1_000L
        if (secs < 60L) return "${secs}s"
        val mins = secs / 60L
        if (mins < 60L) return if (secs % 60L == 0L) "${mins}m" else "${mins}m ${secs % 60L}s"
        return if (mins % 60L == 0L) "${mins / 60L}h" else "${mins / 60L}h ${mins % 60L}m"
    }

    /** `"148k"`, `"1.2M"`, `"812"`. */
    fun tokens(count: Long): String = when {
        count < 1_000L -> "$count"
        count < 1_000_000L -> "${count / 1_000L}k"
        else -> String.format(Locale.US, "%.1fM", count.toDouble() / 1_000_000.0)
    }

    /** `"12 KB"`, `"3.4 MB"`, `"900 B"`. */
    fun bytes(size: Long): String = when {
        size < 1_024L -> "$size B"
        size < 1_048_576L -> "${size / 1_024L} KB"
        else -> String.format(Locale.US, "%.1f MB", size.toDouble() / 1_048_576.0)
    }

    fun plural(count: Int, one: String, many: String): String =
        if (count == 1) one else many

    // Rows

    /**
     * `"Read 3 files, edited 2 files, ran 4 commands"`: what a run's steps
     * did, in the terminal's words and order, with `"+"` on each count when
     * the run continues below what this phone holds.
     */
    fun run(run: Run, counts: RunCounts?): String {
        val plus = if (run.openBelow) "+" else ""
        val c = counts ?: RunCounts(
            commands = 0,
            edits = 0,
            reads = 0,
            searches = 0,
            lists = 0,
            fetches = 0,
            subagents = 0,
            other = 0,
        )
        // "1+ files": a count that is a floor may be more than one.
        fun part(count: Int, verb: String, one: String, many: String): String? {
            if (count <= 0) return null
            val noun = if (count == 1 && !run.openBelow) one else many
            return "$verb $count$plus $noun"
        }
        val searches = if (c.searches == 1 && !run.openBelow) {
            "searched once"
        } else {
            part(c.searches, "searched", "time", "times")
        }
        val parts = listOfNotNull(
            part(c.reads, "read", "file", "files"),
            part(c.edits, "edited", "file", "files"),
            part(c.commands, "ran", "command", "commands"),
            searches,
            part(c.lists, "listed", "folder", "folders"),
            part(c.fetches, "fetched", "page", "pages"),
            part(c.other, "used", "tool", "tools"),
            part(c.subagents, "started", "subagent", "subagents"),
        )
        val words = if (parts.isEmpty()) {
            part(run.steps, "took", "step", "steps") ?: ""
        } else {
            parts.joinToString(", ")
        }
        return words.replaceFirstChar { it.titlecase(Locale.ROOT) }
    }

    /**
     * What the agent said a command is for, as it wrote it; null when it
     * said nothing, as Codex never does.
     */
    fun described(description: String): String? {
        val said = firstLine(description).trim { it == ' ' || it == '\t' }
        return said.ifEmpty { null }
    }

    /** `"$ cargo test -p store"`: a command under the words that describe it. */
    fun commandLine(command: String): String = "$ " + firstLine(command)

    /**
     * Where a described command stands, which no verb says: the agent's
     * words lead its row. A failure says itself in the meta already.
     */
    fun describedState(phase: CallPhase, decided: Boolean): String? =
        if (phase == CallPhase.Failed) null else state(phase, decided = decided)

    fun explore(verb: ExploreVerb): String = when (verb) {
        ExploreVerb.Read -> "Read"
        ExploreVerb.Search -> "Searched"
        ExploreVerb.List -> "Listed"
        ExploreVerb.Fetch -> "Fetched"
        ExploreVerb.WebSearch -> "Searched the web"
    }

    /**
     * A call's phase beside it, for a row whose verb does not say it. With
     * a decision on the row, the decision says what came of asking, so the
     * phase's word for the same thing is left to it.
     */
    fun state(phase: CallPhase, decided: Boolean): String? = when (phase) {
        CallPhase.Asking, CallPhase.Pending -> if (decided) null else "waiting"
        CallPhase.Running -> "running"
        CallPhase.Succeeded -> null
        CallPhase.Failed -> "failed"
        CallPhase.Denied -> if (decided) null else "denied"
        CallPhase.Cancelled -> if (decided) null else "cancelled"
    }

    /**
     * A call's verb says what happened to it: asking the person, under way,
     * refused, cancelled or done.
     */
    fun verb(
        phase: CallPhase,
        row: Row,
        wants: String,
        doing: String,
        done: String,
    ): String = when (phase) {
        CallPhase.Denied -> "Denied"
        CallPhase.Asking -> wants
        CallPhase.Pending, CallPhase.Running -> doing
        CallPhase.Cancelled -> "Cancelled"
        CallPhase.Succeeded -> when (row.decision?.outcome) {
            DecisionView.Allowed -> "Allowed"
            DecisionView.AutoApproved -> "Auto-approved"
            else -> done
        }
        CallPhase.Failed -> done
    }

    /**
     * Whether the verb [verb] gives a call in this phase already names its
     * decision's outcome.
     */
    fun verbSays(phase: CallPhase, outcome: DecisionView): Boolean = when (outcome) {
        DecisionView.Denied -> phase == CallPhase.Denied
        DecisionView.Allowed, DecisionView.AutoApproved -> phase == CallPhase.Succeeded
        DecisionView.Dismissed -> false
    }

    /**
     * A permission decision as meta: allowed or denied, its scope, the note,
     * and where it was answered. The outcome word is left off when the verb
     * already says it.
     */
    fun decision(
        decision: Decision,
        verbSaysIt: Boolean = false,
        note: Boolean = true,
    ): String {
        val parts = mutableListOf<String>()
        if (!verbSaysIt) {
            parts.add(
                when (decision.outcome) {
                    DecisionView.Allowed -> "allowed"
                    DecisionView.Denied -> "denied"
                    DecisionView.AutoApproved -> "auto-approved"
                    DecisionView.Dismissed -> "dismissed"
                },
            )
        }
        decision.granted?.let { parts.add(grant(it)) }
        if (note) {
            decision.note?.let { parts.add("\u201C${firstLine(it)}\u201D") }
        }
        if (decision.elsewhere) {
            parts.add("in the terminal")
        }
        return parts.joinToString(" · ")
    }

    /**
     * The row's meta with its decision after it. [verbPhase] is the phase
     * of a row whose verb comes from [verb], so the decision's outcome is
     * left off where that verb says it.
     */
    fun meta(
        meta: List<String>,
        row: Row,
        verbPhase: CallPhase? = null,
        note: Boolean = true,
    ): String {
        val kept = meta.filter { it.isNotEmpty() }.toMutableList()
        row.decision?.let { dec ->
            val decided = decision(
                decision = dec,
                verbSaysIt = verbPhase?.let { verbSays(it, dec.outcome) } ?: false,
                note = note,
            )
            if (decided.isNotEmpty()) kept.add(decided)
        }
        return kept.joinToString(" · ")
    }

    fun answer(answer: AnswerView): String {
        if (answer.hidden) return "answered (hidden)"
        if (answer.skipped) return "Skipped"
        val picks = answer.picked.toMutableList()
        answer.other?.let { picks.add("\u201C$it\u201D") }
        return picks.joinToString(", ")
    }

    fun resolution(resolution: Resolution, answered: String): String = when (resolution) {
        Resolution.Open -> "Asking"
        Resolution.Answered -> answered
        Resolution.Declined -> "Declined"
        Resolution.Cancelled -> "Cancelled"
        Resolution.Replied -> "Replied instead"
        Resolution.Dismissed -> "Dismissed"
    }

    /** What an allowance granted beyond the one call. */
    fun grant(grant: PermissionGrant): String = when (grant) {
        is PermissionGrant.Claude -> when {
            grant.subjects.isNotEmpty() ->
                "always ${grant.subjects.joinToString(", ")} ${scope(grant.destination)}"
            grant.directories.isNotEmpty() ->
                "access to ${grant.directories.joinToString(", ")} ${scope(grant.destination)}"
            grant.modeName.isNotEmpty() ->
                "switched to ${grant.modeName}"
            else ->
                "always ${scope(grant.destination)}"
        }
        PermissionGrant.Session -> "this session"
        PermissionGrant.Always -> "always, in every project"
        is PermissionGrant.CommandPrefix ->
            "commands starting with ${grant.words.joinToString(" ")}"
        is PermissionGrant.NetworkHosts ->
            if (grant.hosts.isEmpty()) {
                "network access"
            } else {
                "network access to ${grant.hosts.joinToString(", ")}"
            }
    }

    fun boundary(kind: BoundaryKind, cause: String): String {
        val word = when (kind) {
            BoundaryKind.Started -> "Started"
            BoundaryKind.Cleared -> "Cleared"
            BoundaryKind.Compacted -> "Compacted"
            BoundaryKind.Resumed -> "Resumed"
            BoundaryKind.Forked -> "Forked"
            BoundaryKind.Restarted -> "Restarted"
            BoundaryKind.Exited -> "Ended"
            BoundaryKind.DaemonLost -> "amux stopped"
            BoundaryKind.Unspecified -> "Session"
        }
        return if (cause.isEmpty()) word else "$word · $cause"
    }

    /**
     * `"Compacted · 148k → 22k"`; `"Compacted while idle · 248k → 31k"` for
     * one amux ran while the agent sat idle.
     */
    fun compaction(
        before: Long?,
        after: Long?,
        automatic: Boolean,
        idle: Boolean = false,
    ): String {
        var words = if (idle) "Compacted while idle" else "Compacted"
        if (before != null && after != null) {
            words += " · ${tokens(before)} \u2192 ${tokens(after)}"
        }
        if (automatic && !idle) {
            words += " · automatic"
        }
        return words
    }

    /** `"Worked for 1m 42s"`. */
    fun turnEnd(durationMs: Long?, failed: Boolean): String {
        val words = mutableListOf(
            durationMs?.let { "Worked for ${duration(it)}" } ?: "Turn ended",
        )
        if (failed) words.add("failed")
        return words.joinToString(" · ")
    }

    fun thinking(open: Boolean, durationMs: Long?): String {
        if (open) return "Thinking"
        if (durationMs == null) return "Thought"
        return "Thought for ${duration(durationMs)}"
    }

    /** An attachment as its chip names it, from the reference alone. */
    fun chip(view: AttachmentView): String {
        val (name, detail) = chipParts(view)
        return if (detail.isEmpty()) name else "$name · $detail"
    }

    /**
     * A chip's name and what it measures, apart: at a large text size the
     * name wraps and the size goes under it, and neither cuts the other.
     */
    fun chipParts(view: AttachmentView): ChipParts = when (view) {
        is AttachmentView.Image -> ChipParts(view.value.name, bytes(view.value.size))
        is AttachmentView.File -> ChipParts(view.value.name, bytes(view.value.size))
        is AttachmentView.Text ->
            ChipParts(view.name, if (view.lines == 1) "1 line" else "${view.lines} lines")
        is AttachmentView.Review ->
            ChipParts("Review", if (view.comments == 1) "1 comment" else "${view.comments} comments")
        is AttachmentView.Page ->
            ChipParts(view.title, if (view.comments == 1) "1 comment" else "${view.comments} comments")
        AttachmentView.Empty -> ChipParts("Attachment", "")
    }

    fun chipKind(view: AttachmentView): AttachmentKind = when (view) {
        is AttachmentView.Image -> AttachmentKind.Image
        is AttachmentView.File -> AttachmentKind.File
        is AttachmentView.Text -> AttachmentKind.Text
        is AttachmentView.Review -> AttachmentKind.Review
        is AttachmentView.Page -> AttachmentKind.Page
        AttachmentView.Empty -> AttachmentKind.Empty
    }

    /**
     * What Codex's automatic reviewer did, as the row's verb; a status this
     * build does not know in Codex's own word.
     */
    fun autoReview(status: ReviewStatus, word: String): String = when (status) {
        ReviewStatus.Approved -> "Auto-approved"
        ReviewStatus.Denied -> "Auto-denied"
        ReviewStatus.InProgress -> "Auto-reviewing"
        ReviewStatus.Timeout -> "Auto-review timed out"
        ReviewStatus.Abort -> "Auto-review stopped"
        ReviewStatus.Unspecified -> "Auto-reviewed $word"
    }

    fun firstLine(text: String): String {
        val newline = text.indexOf('\n')
        return if (newline < 0) text else text.substring(0, newline)
    }

    fun text(segments: List<Segment>): String =
        segments.joinToString("") { segment ->
            when (segment) {
                is Segment.Text -> segment.value
                is Segment.Attachment -> "[${chip(segment.value)}]"
            }
        }

    // The activity line

    /**
     * `"Running cargo test · 12s"`, as one sentence to hear. [subject] names
     * the call a Running line points at, when the chat holds it.
     */
    fun activity(
        kind: ActivityKind,
        elapsedMs: Long,
        subject: String?,
        sinceMs: Long = 0L,
        subagents: GroupState? = null,
    ): String {
        val parts = activityParts(
            kind = kind,
            elapsedMs = elapsedMs,
            subject = subject,
            sinceMs = sinceMs,
            subagents = subagents,
        )
        val head = listOfNotNull(parts.words, parts.subject).joinToString(" ")
        return listOfNotNull(head, parts.time).joinToString(" · ")
    }

    /**
     * The activity line in its three drawn parts: the words, the subject a
     * Running line names (set in mono), and the elapsed time at the end.
     */
    fun activityParts(
        kind: ActivityKind,
        elapsedMs: Long,
        subject: String?,
        sinceMs: Long = 0L,
        subagents: GroupState? = null,
    ): ActivityParts {
        val elapsed = duration((elapsedMs / 1_000L) * 1_000L)
        return when (kind) {
            ActivityKind.Working -> ActivityParts("Working", null, elapsed)
            ActivityKind.Thinking -> ActivityParts("Thinking", null, null)
            is ActivityKind.Running -> {
                val named = subject?.takeIf { it.isNotEmpty() }
                ActivityParts("Running", named, elapsed)
            }
            is ActivityKind.Subagents -> {
                if (subagents != null && subagents.needsYou > 0) {
                    ActivityParts(subagentActivity(subagents), null, null)
                } else {
                    val label = if (kind.count == 1) {
                        "1 subagent working"
                    } else {
                        "${kind.count} subagents working"
                    }
                    ActivityParts(label, null, null)
                }
            }
            ActivityKind.Compacting -> ActivityParts("Compacting", null, null)
            ActivityKind.SafetyCheck -> ActivityParts("Running safety checks", null, null)
            is ActivityKind.Retrying -> {
                val left = if (sinceMs > 0L) (kind.retryAtMs ?: 0L) - sinceMs - elapsedMs else 0L
                val words = if (kind.maxAttempts > 0) {
                    "Retrying · attempt ${kind.attempt} of ${kind.maxAttempts}"
                } else {
                    "Retrying · attempt ${kind.attempt}"
                }
                ActivityParts(words, null, if (left > 0L) "in ${duration(left)}" else null)
            }
        }
    }

    /** What a row names when the activity line points at it. */
    fun subject(row: Row): String? = when (val kind = row.kind) {
        is RowKind.Command -> described(kind.description) ?: firstLine(kind.command)
        is RowKind.Explore -> kind.subject
        is RowKind.ToolCall ->
            if (kind.server.isEmpty()) kind.tool else "${kind.server} · ${kind.tool}"
        is RowKind.Child -> kind.name.ifEmpty { null }
        is RowKind.FileChange -> kind.files.firstOrNull()?.path
        is RowKind.Subagent -> firstLine(kind.description)
        else -> null
    }

    // The composer

    /**
     * What the empty field says: `"Reply"`, or why sending waits while the
     * draft is kept.
     */
    fun placeholder(
        composer: Composer?,
        host: String,
        reach: Reach,
        connecting: Boolean = false,
        working: Boolean,
        limit: String? = null,
    ): String {
        if (connecting && composer is Composer.Waiting && composer.value == Waiting.Detached) {
            return "Reply"
        }
        return when (composer) {
            Composer.Send, null -> {
                if (limit != null) {
                    limit
                } else if (working) {
                    "Queue a message"
                } else {
                    "Reply"
                }
            }
            Composer.Resume -> "Reply"
            is Composer.Waiting -> when (composer.value) {
                Waiting.CatchingUp -> "Catching up · your draft is kept"
                Waiting.Reconnecting -> "Reconnecting · your draft is kept"
                Waiting.Detached -> when (reach) {
                    is Reach.Offline -> when (reach.value) {
                        Offline.Revoked -> "$host unpaired this phone · your draft is kept"
                        Offline.SignedOut -> "This phone is signed out · your draft is kept"
                        Offline.Plain -> "$host is out of reach · your draft is kept"
                    }
                    else -> "$host is out of reach · your draft is kept"
                }
            }
            is Composer.Native -> ""
        }
    }

    fun queued(row: QueuedRow): String = when {
        row.steered -> "steered"
        row.fromAgent != null -> "queued from ${row.fromAgent}"
        else -> "queued"
    }

    /**
     * How a prompt of this client's is on its way: sending, waiting for its
     * host while the link is down, or perhaps never arrived.
     */
    fun underway(underway: Underway, host: String): String = when (underway) {
        is Underway.Sending -> if (underway.waiting) "waiting for $host\u2026" else "sending"
        Underway.MayNotHaveArrived -> "may not have arrived"
    }

    /**
     * Where one file starts in a patch of several: its path, and what happens
     * to it when that is more than an edit.
     */
    fun fileHeader(file: FileRow): String = when (val change = file.change) {
        is FileChangeView.Created -> "${file.path} (new file)"
        FileChangeView.Deleted -> "${file.path} (deleted)"
        is FileChangeView.Moved -> "${file.path} \u2192 ${change.to}"
        FileChangeView.Edited, is FileChangeView.Writing -> file.path
    }

    // Review

    fun files(count: Int): String = if (count == 1) "1 file" else "$count files"

    fun comments(count: Int): String = if (count == 1) "1 comment" else "$count comments"

    /** `"4 files, 18 added, 28 removed"`: the counts as a sentence to hear. */
    fun changes(files: Int, added: Int, removed: Int): String =
        "${files(files)}, $added added, $removed removed"

    fun attachReview(count: Int): String =
        if (count == 1) "Attach Review · 1 comment" else "Attach Review · $count comments"

    /** `"Added line 12, Refused(String),"`: a diff line as TalkBack reads it. */
    fun spoken(line: DiffLine): String {
        val text = line.text.trim { it == ' ' || it == '\t' }
        return when (line.kind) {
            LineKind.Added -> "Added line ${line.newLine ?: 0}, $text"
            LineKind.Removed -> "Removed line ${line.oldLine ?: 0}, $text"
            LineKind.Context -> "Line ${line.newLine ?: line.oldLine ?: 0}, $text"
        }
    }

    /** `"1 line"`, `"12 lines"`. */
    fun lines(count: Int): String = if (count == 1) "1 line" else "$count lines"

    fun commentOn(lines: Int): String =
        if (lines == 1) "Comment on 1 line" else "Comment on $lines lines"

    // Around the composer

    /**
     * `"92% context"`, while the context is near its end: said on the model
     * pill, because it is the session's standing rather than news.
     */
    fun contextLeft(context: ContextView?): String? {
        if (context == null || !context.nearFull) return null
        val percent = context.percent ?: return null
        return "$percent% context"
    }

    /**
     * `"184k of 200k tokens"`, or the tokens alone where the window is not
     * known.
     */
    fun contextUsed(context: ContextView): String {
        val window = context.windowTokens
            ?: return "${tokens(context.usedTokens)} tokens"
        return "${tokens(context.usedTokens)} of ${tokens(window)} tokens"
    }

    /** `"2 in background"`: the dock's row for what still runs after a turn. */
    fun inBackground(count: Int): String = "$count in background"

    /**
     * `"5-hour limit reached · resets 11:24 PM"`: the window of a usage limit
     * that has been reached, the fullest one whose use is known when the
     * provider does not say which.
     */
    fun limitReached(
        usage: UsageView,
        now: Instant = Instant.now(),
        zone: ZoneId = ZoneId.systemDefault(),
        locale: Locale = Locale.getDefault(),
    ): String {
        val window = usage.windows.firstOrNull { it.state == UsageState.Blocked }
            ?: usage.windows
                .mapNotNull { w -> w.usedPercent?.let { w to it } }
                .maxByOrNull { it.second }
                ?.first
            ?: return "Usage limit reached"
        val reached = "${usageLabel(window.label)} reached"
        val at = window.resetsAtMs ?: return reached
        if (!Instant.ofEpochMilli(at).isAfter(now)) return reached
        return "$reached · resets ${resets(at, now = now, zone = zone, locale = locale)}"
    }

    // The dock

    /**
     * The task the dock's head names: the one in progress, else the next to
     * do, else the last one done.
     */
    fun headTask(tasks: TasksView): String {
        val pick = tasks.entries.firstOrNull { it.mark == TaskMark.Current }
            ?: tasks.entries.firstOrNull { it.mark == TaskMark.Todo }
            ?: tasks.entries.lastOrNull()
        return pick?.subject ?: tasks.current
    }

    /** The dock's head when the agent started others but keeps no list. */
    fun started(count: Int): String =
        if (count == 1) "Started 1 agent" else "Started $count agents"

    /**
     * A started agent's state under its name: how it ended once it has,
     * else its host when that is out of reach, else what it is doing.
     */
    fun childState(card: FleetCard): String {
        if (card.attention != Attention.Exited) {
            when (card.hostReach) {
                is Reach.Online -> Unit
                Reach.Away -> return "${card.host} away"
                is Reach.Offline -> return "${card.host} offline"
            }
        }
        return when (card.attention) {
            Attention.NeedsYou -> "needs you"
            Attention.Working -> "working"
            Attention.Starting -> "starting"
            Attention.Idle -> "idle"
            Attention.Exited -> when (val cause = card.exitCause ?: ExitCause.Ended) {
                ExitCause.Finished -> "finished"
                ExitCause.Ended -> "stopped"
                is ExitCause.Failed -> "stopped · ${cause.value}"
            }
        }
    }

    /**
     * What settling an agent cuts short, as the settle confirm says it:
     * `"desk-1 is working and has 2 background jobs and 1 subagent."` Null when
     * settling cuts nothing under way, so it settles without asking.
     */
    fun closeCuts(name: String, phase: PhaseView?, jobs: Int, subAgents: Int): String? {
        val turn = when (phase) {
            PhaseView.Working -> Turn.Working
            PhaseView.NeedsYou -> Turn.NeedsYou
            else -> Turn.None
        }
        return closeCuts(name = name, turn = turn, jobs = jobs, subAgents = subAgents)
    }

    /** The same, from what a fleet row knows of the agent. */
    fun closeCuts(name: String, attention: Attention, jobs: Int, subAgents: Int): String? {
        val turn = when (attention) {
            Attention.Working -> Turn.Working
            Attention.NeedsYou -> Turn.NeedsYou
            else -> Turn.None
        }
        return closeCuts(name = name, turn = turn, jobs = jobs, subAgents = subAgents)
    }

    private enum class Turn { Working, NeedsYou, None }

    private fun closeCuts(name: String, turn: Turn, jobs: Int, subAgents: Int): String? {
        val held = mutableListOf<String>()
        if (jobs > 0) {
            held.add(if (jobs == 1) "1 background job" else "$jobs background jobs")
        }
        if (subAgents > 0) {
            held.add(if (subAgents == 1) "1 subagent" else "$subAgents subagents")
        }
        val has = held.joinToString(" and ")
        return when (turn) {
            Turn.Working ->
                if (has.isEmpty()) "$name is working." else "$name is working and has $has."
            Turn.NeedsYou ->
                if (has.isEmpty()) "$name needs you mid-turn." else "$name needs you mid-turn and has $has."
            Turn.None ->
                if (has.isEmpty()) null else "$name has $has."
        }
    }

    /**
     * The exited agent's head in the composer. An agent the person settled is
     * said to be settled; why it failed outranks both.
     */
    fun exited(cause: ExitCause, settled: Boolean = false): String = when (cause) {
        is ExitCause.Failed -> "Stopped · ${cause.value}"
        else -> when {
            settled -> "Settled"
            cause == ExitCause.Finished -> "Finished"
            else -> "Stopped"
        }
    }

    // Settings

    /** A usage window by which limit it is. */
    fun usageLabel(label: UsageLabel): String = when (label) {
        UsageLabel.FiveHour -> "5-hour limit"
        is UsageLabel.Weekly ->
            label.model?.let { "$it weekly limit" } ?: "Weekly limit"
        is UsageLabel.Minutes -> "${label.value}-minute limit"
        is UsageLabel.Named -> "${label.value} limit"
    }

    /**
     * How full a usage window is: `"91% used"`, or `"—"` when the provider
     * has not said.
     */
    fun used(window: UsageWindowView): String {
        val used = window.usedPercent ?: return "\u2014"
        return "${used.roundToInt()}% used"
    }

    /**
     * A usage window's state and when it resets: `"near · resets 16:07"`.
     * Null when there is neither.
     */
    fun usageDetail(
        window: UsageWindowView,
        now: Instant = Instant.now(),
        zone: ZoneId = ZoneId.systemDefault(),
        locale: Locale = Locale.getDefault(),
    ): String? {
        val state = when (window.state) {
            UsageState.Blocked -> "reached"
            UsageState.NearLimit -> "near"
            UsageState.Ok -> "fine"
            UsageState.Unspecified -> null
        }
        val resets = window.resetsAtMs
            ?.takeIf { Instant.ofEpochMilli(it).isAfter(now) }
            ?.let { "resets ${resets(it, now = now, zone = zone, locale = locale)}" }
        val words = listOfNotNull(state, resets)
        return if (words.isEmpty()) null else words.joinToString(" · ")
    }

    /**
     * When a limit resets: the time of day when it is within the day, else
     * the abbreviated weekday.
     */
    fun resets(
        atMs: Long,
        now: Instant = Instant.now(),
        zone: ZoneId = ZoneId.systemDefault(),
        locale: Locale = Locale.getDefault(),
    ): String = PushWords.resets(Instant.ofEpochMilli(atMs), now = now, zone = zone, locale = locale)

    // The overview

    /** What the changes are counted against: `"Uncommitted"`, `"vs main"`. */
    fun comparison(comparison: Comparison, base: String?): String = when (comparison) {
        Comparison.Uncommitted -> "Uncommitted"
        Comparison.OnBranch -> base?.let { "vs $it" } ?: "On branch"
    }

    /** A file's counts with a zero side left out: `"+20"`, `"−12"`, `"+25 −8"`. */
    fun counts(added: Int, removed: Int): String =
        listOfNotNull(
            if (added > 0) "+$added" else null,
            if (removed > 0) "\u2212$removed" else null,
        ).joinToString(" ")

    /** The question before one background job is stopped: `"Stop cargo test?"`. */
    fun stopJob(job: JobRow): String = "Stop ${firstLine(job.command)}?"

    /** What stopping one background job leaves alone. */
    const val STOP_JOB_LEAVES: String = "The agent keeps running."

    /** A background job asked to stop that has not yet ended. */
    const val STOPPING_JOB: String = "Stopping\u2026"

    /** How long a background job has run: `"40s"`, `"14m"`. */
    fun ran(sinceMs: Long, now: Instant = Instant.now()): String =
        Elapsed.spelled(maxOf(0L, now.toEpochMilli() - sinceMs) / 1_000.0)

    /** A tool server that failed: `"github needs sign-in"`. */
    fun failed(server: ServerView): String =
        if (server.needsAuth) "${server.name} needs sign-in" else "${server.name} failed to start"

    /**
     * The model chip's words, from the controls the view sums up. Null when
     * nothing is reported.
     */
    fun chip(controls: ControlsSummary, context: ContextView? = null): ControlsChip? {
        val unusual = listOfNotNull(
            controls.permission?.let(::permission),
            controls.mode?.let(::mode),
            contextLeft(context),
        )
        val model = controls.model ?: ""
        val detail = controls.effort ?: ""
        if (model.isEmpty() && detail.isEmpty() && unusual.isEmpty()) return null
        return ControlsChip(model = model, detail = detail, unusual = unusual)
    }

    /** A permission by the name the view gives it; unmatched Codex settings read Custom. */
    fun permission(choice: PermissionChoice): String =
        if (choice.custom) "Custom" else choice.displayName

    /** What the catalogue says of a permission beyond its name. */
    fun permissionDetail(choice: PermissionChoice): String = when {
        choice.unlisted -> "Reported by the agent"
        choice.neverAsks -> "Acts without asking"
        !choice.settable -> "Can\u2019t be picked from here"
        else -> ""
    }

    /** A mode by the name the view gives it. */
    fun mode(choice: ModeChoice): String = choice.displayName

    /**
     * How the model and effort change where only the agent's own commands
     * change them, typed in the composer; null where a pick does.
     */
    fun byTyping(changeable: Changeability): String? {
        val model = changeable.model
        val effort = changeable.effort
        return when {
            model is Changeable.ByTyping && effort is Changeable.ByTyping ->
                "To change the model or effort, type ${model.value} <name> or ${effort.value} <level> in the composer."
            model is Changeable.ByTyping ->
                "To change the model, type ${model.value} <name> in the composer."
            effort is Changeable.ByTyping ->
                "To change the effort, type ${effort.value} <level> in the composer."
            else -> null
        }
    }

    /** Where the permission changes only by stepping through the agent's own cycle. */
    const val CYCLES_ONLY: String =
        "Terminal Claude changes permission only by cycling through its permissions."

    /** The permissions card's heading, by the agent's kind. */
    fun permissionsHeading(kind: Kind?): String =
        if (kind == Kind.Codex) "CODEX PERMISSIONS" else "CLAUDE PERMISSIONS"

    /** A command as TalkBack speaks it: its name, then where it comes from. */
    fun command(command: CommandView): String =
        if (command.source.isEmpty()) command.name else "${command.name}, from ${command.source}"

    /** The plus menu's permissions row: the current permission after the name. */
    fun permissionsItem(settings: SettingsView): String {
        val current = settings.permissions.firstOrNull { it.current } ?: return "Permissions"
        return "Permissions · ${permission(current)}"
    }

    /** Who must be signed in again, for the card that stands in the composer's place. */
    fun needsSignIn(kind: Kind?): String =
        if (kind == Kind.Codex) "Codex needs you to sign in" else "Claude needs you to sign in"

    /** Where and how to sign in again: on the agent's own host. */
    fun signInSteps(kind: Kind?, host: String): String =
        if (kind == Kind.Codex) {
            "Run codex login on $host."
        } else {
            "Run claude and sign in with /login on $host."
        }

    fun signIn(view: SignInView): String {
        val head = if (view.state == SignInState.Failed) "Sign-in failed" else "Signed out"
        return if (view.account.isEmpty()) head else "$head · ${view.account}"
    }

    // The card

    /** What the card says it wants. */
    fun headline(card: AskCard): String = when (val body = card.body) {
        is AskBody.Command -> "Wants to run a command"
        is AskBody.Edit -> when {
            body.files > 1 -> "Wants to edit ${body.files} files"
            body.created == true -> "Wants to create a file"
            else -> "Wants to edit a file"
        }
        is AskBody.Tool ->
            if (body.server.isEmpty()) {
                "Wants to use ${body.tool}"
            } else {
                "Wants to use ${body.server} · ${body.tool}"
            }
        is AskBody.Question ->
            if (body.value.size > 1) "${body.value.size} questions" else "Question"
        is AskBody.Plan -> "Plan ready"
        is AskBody.Form -> "${body.server} needs details"
        is AskBody.Link -> "${body.server} wants you to open a link"
        is AskBody.Access -> "Wants more access"
        is AskBody.Unanswerable -> "Can\u2019t answer this here"
    }

    fun scope(scope: Scope): String = when (scope) {
        Scope.Session -> "this session"
        Scope.Project -> "in this project"
        Scope.ProjectShared -> "in this project, for everyone"
        Scope.User -> "in every project"
        is Scope.Other -> scope.value
    }

    /** A choice in words: what happens, never rule syntax. */
    fun choice(choice: Choice): String {
        val label = when (val outcome = choice.outcome) {
            ChoiceOutcome.AllowOnce -> "Allow once"
            is ChoiceOutcome.AllowAlways -> when {
                outcome.subjects.isNotEmpty() ->
                    "Always allow ${outcome.subjects.joinToString(", ")} ${scope(outcome.scope)}"
                outcome.directories.isNotEmpty() ->
                    "Allow access to ${outcome.directories.joinToString(", ")} ${scope(outcome.scope)}"
                outcome.mode.isNotEmpty() ->
                    "Switch to ${outcome.modeName.ifEmpty { outcome.mode }}"
                else ->
                    "Always allow ${scope(outcome.scope)}"
            }
            ChoiceOutcome.AllowForSession -> "Allow for this session"
            is ChoiceOutcome.AllowSimilar ->
                "Allow commands starting with ${outcome.prefix.joinToString(" ")}"
            is ChoiceOutcome.AllowNetwork ->
                if (outcome.hosts.isEmpty()) {
                    "Allow network access"
                } else {
                    "Allow network access to ${outcome.hosts.joinToString(", ")}"
                }
            ChoiceOutcome.Deny -> "Deny"
            ChoiceOutcome.DenyAndStop -> "Deny and stop"
            is ChoiceOutcome.ApprovePlan ->
                if (outcome.mode.isEmpty()) {
                    "Approve"
                } else {
                    "Approve and switch to ${outcome.modeName.ifEmpty { outcome.mode }}"
                }
            ChoiceOutcome.SendBack -> "Send back"
            ChoiceOutcome.Submit -> "Submit"
            ChoiceOutcome.Decline -> "Decline"
            ChoiceOutcome.OpenLink -> "I\u2019m done"
            ChoiceOutcome.GrantForTurn -> "Grant"
            ChoiceOutcome.GrantForSession -> "Grant"
        }
        return if (choice.takesNote) "$label\u2026" else label
    }

    /** A plan approval's switch as the plan card's toggle names it. */
    fun planSwitch(choice: Choice): String {
        val outcome = choice.outcome as? ChoiceOutcome.ApprovePlan ?: return ""
        return "Switch to ${outcome.modeName.ifEmpty { outcome.mode }}"
    }

    /** A choice on a button: the likely allow reads `"Allow"` beside `"Deny"`. */
    fun button(choice: Choice): String = when (choice.outcome) {
        ChoiceOutcome.AllowOnce -> "Allow"
        ChoiceOutcome.Deny -> "Deny"
        else -> choice(choice)
    }

    /** An offered scope as a row: what it allows, and under it how far it reaches. */
    fun scopeRow(choice: Choice): ScopeRow = when (val outcome = choice.outcome) {
        is ChoiceOutcome.AllowAlways -> when {
            outcome.subjects.isNotEmpty() ->
                ScopeRow("Always allow ${outcome.subjects.joinToString(", ")}", reach(outcome.scope))
            outcome.directories.isNotEmpty() ->
                ScopeRow(
                    if (outcome.scope == Scope.Session) {
                        "Allow access for this session"
                    } else {
                        "Always allow access"
                    },
                    "in ${outcome.directories.joinToString(", ")}",
                )
            outcome.mode.isNotEmpty() ->
                ScopeRow("Switch to ${outcome.modeName.ifEmpty { outcome.mode }}", reach(outcome.scope))
            else ->
                ScopeRow("Always allow", reach(outcome.scope))
        }
        is ChoiceOutcome.AllowSimilar ->
            ScopeRow("Allow similar commands", "Starting with ${outcome.prefix.joinToString(" ")}")
        is ChoiceOutcome.AllowNetwork ->
            ScopeRow(
                "Allow network access",
                if (outcome.hosts.isEmpty()) null else "To ${outcome.hosts.joinToString(", ")}",
            )
        else -> ScopeRow(choice(choice), null)
    }

    /** How far a scope reaches, under its row. */
    fun reach(scope: Scope): String = when (scope) {
        Scope.Session -> "For this session"
        Scope.Project -> "In this project, from now on"
        Scope.ProjectShared -> "In this project, for everyone"
        Scope.User -> "In every project, from now on"
        is Scope.Other -> scope.value
    }

    /** `"1 of 3"` when asks are queued. */
    fun position(card: AskCard): String? =
        if (card.count > 1L) "${card.position} of ${card.count}" else null

    private fun formatNumber(value: Double): String {
        if (!value.isFinite()) return value.toString()
        val asLong = value.toLong()
        if (asLong.toDouble() == value) return asLong.toString()
        return BigDecimal.valueOf(value).stripTrailingZeros().toPlainString()
    }
}
