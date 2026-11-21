package sh.amux.features

import sh.amux.design.IconSize
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.DriveFileMove
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.AccountTree
import androidx.compose.material.icons.outlined.Code
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.FrontHand
import androidx.compose.material.icons.outlined.PersonAdd
import androidx.compose.material.icons.outlined.PersonRemove
import androidx.compose.material.icons.outlined.Stop
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.AgentKey
import sh.amux.values.CallPhase
import sh.amux.values.ChildAction
import sh.amux.values.FileChangeView
import sh.amux.values.FileRow
import sh.amux.values.GroupState
import sh.amux.values.OpenedAgent
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.SubagentGroup
import sh.amux.words.AgentStanding
import sh.amux.words.ChatWords
import sh.amux.words.RailJoin
import sh.amux.words.subagentGroup
import sh.amux.words.subagentGroupStanding

@Composable
internal fun RunHeaderLine(
    words: String,
    open: Boolean,
    rail: RailJoin,
    onToggleRun: () -> Unit,
) {
    GridRow(
        kindTag = "run",
        glyph = if (open) Icons.Outlined.ExpandMore else Icons.AutoMirrored.Outlined.KeyboardArrowRight,
        rail = rail,
        verb = words,
        quiet = true,
        opens = true,
        open = open,
        trailingChevron = false,
        onClick = onToggleRun,
        modifier = Modifier.testTag(ChatRowTags.RUN_HEADER),
    )
}

@Composable
internal fun SubagentGroupLine(
    row: Row,
    group: SubagentGroup,
    groupState: GroupState?,
    rail: RailJoin,
    onToggleGroup: () -> Unit,
) {
    val standing = groupState?.let(ChatWords::subagentGroupStanding)
    val count = groupState?.agents ?: group.agents.size
    GridRow(
        kindTag = "subagent-group",
        glyph = if (standing == AgentStanding.NeedsYou) Icons.Outlined.FrontHand else Icons.Outlined.AccountTree,
        accented = standing == AgentStanding.NeedsYou || row.attention,
        failed = standing == AgentStanding.Failed,
        rail = rail,
        verb = ChatWords.subagentGroup(count),
        meta = groupState?.let(RowMeta::group) ?: RowMeta(),
        opens = true,
        open = group.open,
        onClick = onToggleGroup,
        modifier = Modifier.testTag(ChatRowTags.SUBAGENT_GROUP),
    )
}

@Composable
internal fun CommandRowView(
    row: Row,
    kind: RowKind.Command,
    expanded: Boolean,
    rail: RailJoin,
    unresolvedFailure: Boolean,
    denialNote: String?,
    onToggleExpand: () -> Unit,
) {
    val metaParts = buildList {
        val code = kind.exitCode
        if (code != null && code != 0) {
            add(stringResource(R.string.chat_row_command_exit, code))
        } else if (code == null && kind.state == CallPhase.Failed) {
            add(stringResource(R.string.chat_row_failed))
        }
        kind.durationMs?.let { add(ChatWords.duration(it)) }
    }
    val said = ChatWords.described(kind.description)
    val hasOutput = kind.outputHead.isNotEmpty() || kind.outputTail.isNotEmpty() || kind.moreLines > 0L
    val shownOutput = remember(kind.outputHead, kind.outputTail, expanded) {
        if (expanded) {
            if (kind.outputTail.isNotEmpty()) kind.outputHead + kind.outputTail else kind.outputHead
        } else {
            kind.outputHead.take(RowGridMetrics.COMMAND_OUTPUT_HEAD_LINES)
        }
    }
    val remainingLines = if (expanded) 0L else kind.moreLines

    if (said != null) {
        val extraState = ChatWords.describedState(kind.state, decided = row.decision != null).orEmpty()
        GridRow(
            kindTag = "command",
            glyph = callGlyph(kind.state, Icons.Outlined.Code),
            accented = isAccented(row, kind.state),
            failed = kind.state == CallPhase.Failed,
            unresolvedFailure = unresolvedFailure,
            rail = rail,
            verb = said,
            meta = RowMeta(ChatWords.meta(metaParts + extraState, row, note = false)),
            note = ChatWords.commandLine(kind.command),
            quote = denialNote,
            output = shownOutput,
            outputTrimmed = kind.outputTrimmed,
            moreLines = remainingLines,
            opens = hasOutput,
            open = expanded,
            onClick = onToggleExpand,
        )
    } else {
        val verb = ChatWords.verb(
            phase = kind.state,
            row = row,
            wants = stringResource(R.string.chat_row_command_wants),
            doing = stringResource(R.string.chat_row_command_running),
            done = stringResource(R.string.chat_row_command_ran),
        )
        GridRow(
            kindTag = "command",
            glyph = callGlyph(kind.state, Icons.Outlined.Code),
            accented = isAccented(row, kind.state),
            failed = kind.state == CallPhase.Failed,
            unresolvedFailure = unresolvedFailure,
            rail = rail,
            verb = verb,
            subject = ChatWords.firstLine(kind.command),
            meta = RowMeta(ChatWords.meta(metaParts, row, verbPhase = kind.state, note = false)),
            quote = denialNote,
            output = shownOutput,
            outputTrimmed = kind.outputTrimmed,
            moreLines = remainingLines,
            opens = hasOutput,
            open = expanded,
            onClick = onToggleExpand,
        )
    }
}

@Composable
internal fun FileChangeRowView(
    row: Row,
    files: List<FileRow>,
    state: CallPhase,
    rail: RailJoin,
    unresolvedFailure: Boolean,
    denialNote: String?,
    onOpenFileChange: (FileRow) -> Unit,
) {
    val tail = listOf(ChatWords.state(state, decided = row.decision != null).orEmpty())
    if (files.isEmpty()) {
        GridRow(
            kindTag = "file-change",
            glyph = callGlyph(state, Icons.Outlined.Edit),
            accented = isAccented(row, state),
            failed = state == CallPhase.Failed,
            unresolvedFailure = unresolvedFailure,
            rail = rail,
            verb = stringResource(R.string.chat_row_file_editing),
            meta = RowMeta(ChatWords.meta(tail, row, note = false)),
            quote = denialNote,
        )
        return
    }

    Column(modifier = Modifier.fillMaxWidth()) {
        files.forEachIndexed { index, file ->
            val isLast = index == files.lastIndex
            val extra = if (index == 0) ChatWords.meta(tail, row, note = false) else ""
            val (glyph, verb, subject, baseMeta) = fileChangePresentation(file)
            val openLabel = stringResource(R.string.chat_row_file_open_diff, file.path)
            GridRow(
                kindTag = "file-change",
                glyph = callGlyph(state, glyph),
                accented = isAccented(row, state),
                failed = state == CallPhase.Failed,
                unresolvedFailure = unresolvedFailure,
                rail = if (isLast) rail else RailJoin(continues = true, nested = rail.nested),
                verb = verb,
                subject = subject,
                subjectEmphasis = verb.isEmpty(),
                meta = baseMeta.then(extra),
                quote = if (index == 0) denialNote else null,
                opens = true,
                pushes = true,
                actionDescription = openLabel,
                onClick = { onOpenFileChange(file) },
                modifier = Modifier.testTag(ChatRowTags.FILE_CHANGE_ITEM),
            )
        }
    }
}

private data class FilePresentation(
    val glyph: ImageVector,
    val verb: String,
    val subject: String,
    val meta: RowMeta,
)

@Composable
private fun fileChangePresentation(file: FileRow): FilePresentation = when (val change = file.change) {
    FileChangeView.Edited -> FilePresentation(
        glyph = Icons.Outlined.Edit,
        verb = "",
        subject = file.path,
        meta = RowMeta.change(added = file.added, removed = file.removed),
    )
    is FileChangeView.Created -> FilePresentation(
        glyph = Icons.Outlined.Edit,
        verb = stringResource(R.string.chat_row_file_created),
        subject = file.path,
        meta = RowMeta(ChatWords.lines(change.lines)),
    )
    FileChangeView.Deleted -> FilePresentation(
        glyph = Icons.Outlined.DeleteOutline,
        verb = stringResource(R.string.chat_row_file_deleted),
        subject = file.path,
        meta = RowMeta(),
    )
    is FileChangeView.Moved -> FilePresentation(
        glyph = Icons.AutoMirrored.Outlined.DriveFileMove,
        verb = stringResource(R.string.chat_row_file_moved),
        subject = stringResource(R.string.chat_row_file_moved_path, file.path, change.to),
        meta = RowMeta(),
    )
    is FileChangeView.Writing -> FilePresentation(
        glyph = Icons.Outlined.Edit,
        verb = stringResource(R.string.chat_row_file_writing),
        subject = file.path,
        meta = RowMeta(ChatWords.lines(change.lines)),
    )
}

@Composable
internal fun ChildRowView(
    row: Row,
    kind: RowKind.Child,
    expanded: Boolean,
    rail: RailJoin,
    opened: OpenedAgent?,
    unresolvedFailure: Boolean,
    denialNote: String?,
    onToggleExpand: () -> Unit,
    onOpenAgent: (AgentKey) -> Unit,
) {
    val verb = when {
        kind.action == ChildAction.Start && kind.state == CallPhase.Failed ->
            stringResource(R.string.chat_row_child_could_not_start)
        kind.action == ChildAction.Stop && kind.state == CallPhase.Failed ->
            stringResource(R.string.chat_row_child_could_not_stop)
        kind.action == ChildAction.Start -> ChatWords.verb(
            phase = kind.state,
            row = row,
            wants = stringResource(R.string.chat_row_child_wants_start),
            doing = stringResource(R.string.chat_row_child_starting),
            done = stringResource(R.string.chat_row_child_started),
        )
        else -> ChatWords.verb(
            phase = kind.state,
            row = row,
            wants = stringResource(R.string.chat_row_child_wants_stop),
            doing = stringResource(R.string.chat_row_child_stopping),
            done = stringResource(R.string.chat_row_child_stopped),
        )
    }
    val who = kind.name.ifEmpty { stringResource(R.string.chat_row_child_fallback_name) }
    val subject = if (kind.host.isEmpty()) who else stringResource(R.string.chat_row_child_on_host, who, kind.host)

    if (opened != null) {
        val standing = AgentStanding.of(opened.phase)
        GridRow(
            kindTag = "child",
            glyph = if (standing == AgentStanding.NeedsYou) Icons.Outlined.FrontHand else callGlyph(kind.state, Icons.Outlined.PersonAdd),
            accented = standing == AgentStanding.NeedsYou || isAccented(row, kind.state),
            failed = kind.state == CallPhase.Failed || standing == AgentStanding.Failed,
            unresolvedFailure = unresolvedFailure,
            rail = rail,
            verb = verb,
            subject = subject,
            meta = RowMeta(ChatWords.meta(listOf(kind.kind), row, verbPhase = kind.state, note = false))
                .then(standing.words),
            quote = denialNote,
            opens = true,
            pushes = true,
            actionDescription = stringResource(R.string.chat_row_open_agent),
            onClick = { onOpenAgent(opened.agent) },
            modifier = Modifier.testTag(ChatRowTags.CHILD_OPEN_AGENT),
        )
    } else {
        val defaultGlyph = if (kind.action == ChildAction.Start) Icons.Outlined.PersonAdd else Icons.Outlined.PersonRemove
        GridRow(
            kindTag = "child",
            glyph = callGlyph(kind.state, defaultGlyph),
            accented = isAccented(row, kind.state),
            failed = kind.state == CallPhase.Failed,
            unresolvedFailure = unresolvedFailure,
            rail = rail,
            verb = verb,
            subject = subject,
            meta = RowMeta(ChatWords.meta(listOf(kind.kind), row, verbPhase = kind.state, note = false)),
            quote = denialNote,
            detail = if (expanded && kind.result.isNotEmpty()) kind.result else null,
            opens = kind.result.isNotEmpty(),
            open = expanded,
            onClick = onToggleExpand,
        )
    }
}

@Composable
internal fun SubagentRowView(
    row: Row,
    kind: RowKind.Subagent,
    expanded: Boolean,
    rail: RailJoin,
    opened: OpenedAgent?,
    onToggleExpand: () -> Unit,
    onOpenAgent: (AgentKey) -> Unit,
) {
    val standing = AgentStanding.of(opened = opened, call = kind.state)
    val verb = stringResource(
        if (kind.continued) R.string.chat_row_subagent_continued else R.string.chat_row_subagent_verb,
    )
    val hasAnswer = !standing.live && kind.answer.isNotEmpty()

    if (row.opens != null) {
        val subject = if (kind.description.isEmpty()) {
            opened?.name.orEmpty()
        } else {
            ChatWords.firstLine(kind.description)
        }
        val openAction: (() -> Unit)? = opened?.let { agent -> { onOpenAgent(agent.agent) } }

        // Opening the subagent's own chat and expanding its answer are kept on
        // distinct touch targets so the two gestures never conflict.
        Column(modifier = Modifier.fillMaxWidth()) {
            GridRow(
                kindTag = "subagent",
                glyph = if (standing == AgentStanding.NeedsYou) Icons.Outlined.FrontHand else Icons.Outlined.AccountTree,
                accented = standing == AgentStanding.NeedsYou || row.attention,
                failed = standing == AgentStanding.Failed,
                rail = if (hasAnswer) RailJoin(continues = true, nested = rail.nested) else rail,
                verb = verb,
                subject = subject,
                subjectMono = false,
                meta = RowMeta.standing(standing),
                opens = openAction != null,
                pushes = true,
                actionDescription = stringResource(R.string.chat_row_open_agent),
                onClick = { openAction?.invoke() },
                modifier = if (openAction != null) Modifier.testTag(ChatRowTags.SUBAGENT_OPEN_AGENT) else Modifier,
            )
            if (hasAnswer) {
                SubagentAnswerToggleBlock(
                    answer = kind.answer,
                    expanded = expanded,
                    rail = rail,
                    onToggleExpand = onToggleExpand,
                )
            }
        }
    } else {
        val durationPart = if (standing.live) "" else kind.durationMs?.let(ChatWords::duration).orEmpty()
        GridRow(
            kindTag = "subagent",
            glyph = Icons.Outlined.AccountTree,
            accented = standing == AgentStanding.NeedsYou || row.attention,
            failed = standing == AgentStanding.Failed,
            rail = rail,
            verb = verb,
            subject = ChatWords.firstLine(kind.description),
            subjectMono = false,
            meta = RowMeta.standing(standing).then(durationPart),
            detail = if (hasAnswer) kind.answer else null,
            detailMono = false,
            detailMaxLines = if (expanded) Int.MAX_VALUE else 2,
            opens = hasAnswer,
            open = expanded,
            onClick = onToggleExpand,
        )
    }
}

@Composable
private fun SubagentAnswerToggleBlock(
    answer: String,
    expanded: Boolean,
    rail: RailJoin,
    onToggleExpand: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val toggleLabel = stringResource(
        if (expanded) R.string.chat_row_hide_answer else R.string.chat_row_show_answer,
    )
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(
                start = RowGridMetrics.NestedIndent + if (rail.nested) RowGridMetrics.NestedIndent else 0.dp,
                bottom = if (rail.continues) RowGridMetrics.InRunBottom else RowGridMetrics.AfterRunBottom,
            )
            .clip(RoundedCornerShape(Space.s))
            .thumbTarget()
            .clickable(role = Role.Button, onClick = onToggleExpand)
            .semantics { contentDescription = toggleLabel }
            .padding(vertical = Space.xs)
            .testTag(ChatRowTags.SUBAGENT_TOGGLE_ANSWER),
        horizontalArrangement = Arrangement.spacedBy(Space.s),
        verticalAlignment = Alignment.Top,
    ) {
        Text(
            text = answer,
            style = typography.detail,
            color = colors.inkMuted,
            maxLines = if (expanded) Int.MAX_VALUE else 2,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        Icon(
            imageVector = if (expanded) Icons.Outlined.ExpandLess else Icons.Outlined.ExpandMore,
            contentDescription = null,
            tint = colors.inkFaint,
            modifier = Modifier.size(IconSize.Inline),
        )
    }
}
