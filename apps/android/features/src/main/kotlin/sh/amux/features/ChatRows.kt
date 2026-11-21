package sh.amux.features

import android.graphics.BitmapFactory
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.CallMade
import androidx.compose.material.icons.automirrored.outlined.CallReceived
import androidx.compose.material.icons.automirrored.outlined.DriveFileMove
import androidx.compose.material.icons.automirrored.outlined.HelpOutline
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.automirrored.outlined.Notes
import androidx.compose.material.icons.outlined.AccountTree
import androidx.compose.material.icons.outlined.AttachFile
import androidx.compose.material.icons.outlined.Block
import androidx.compose.material.icons.outlined.Build
import androidx.compose.material.icons.outlined.Checklist
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Code
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.FrontHand
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material.icons.outlined.Language
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.MoreHoriz
import androidx.compose.material.icons.outlined.PersonAdd
import androidx.compose.material.icons.outlined.PersonRemove
import androidx.compose.material.icons.outlined.PlayArrow
import androidx.compose.material.icons.outlined.RateReview
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Stop
import androidx.compose.material.icons.outlined.SwapHoriz
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material.icons.outlined.VerifiedUser
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxColors
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.AgentKey
import sh.amux.values.AnswerView
import sh.amux.values.AskRow
import sh.amux.values.AttachmentView
import sh.amux.values.BlobRef
import sh.amux.values.CallPhase
import sh.amux.values.ChildAction
import sh.amux.values.DecisionView
import sh.amux.values.EnvelopeKind
import sh.amux.values.FileChangeView
import sh.amux.values.FileRow
import sh.amux.values.GroupState
import sh.amux.values.OpenedAgent
import sh.amux.values.PlanVerdict
import sh.amux.values.QuestionView
import sh.amux.values.Resolution
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Segment
import sh.amux.values.SendState
import sh.amux.values.SubagentGroup
import sh.amux.words.AgentStanding
import sh.amux.words.ChatWords
import sh.amux.words.OpenableLink
import sh.amux.words.RailJoin
import sh.amux.words.RunPlacement
import sh.amux.words.subagentGroup
import sh.amux.words.subagentGroupParts
import sh.amux.words.subagentGroupStanding

/**
 * Local test tags for [ChatRowView] and its sub-components.
 */
object ChatRowTags {
    fun row(id: String): String = "chat.row.$id"
    fun kind(kind: String): String = "chat.row.kind.$kind"

    const val RUN_HEADER: String = "chat.row.run"
    const val SUBAGENT_GROUP: String = "chat.row.subagent-group"
    const val SUBAGENT_OPEN_AGENT: String = "chat.row.subagent.open"
    const val SUBAGENT_TOGGLE_ANSWER: String = "chat.row.subagent.expand"
    const val CHILD_OPEN_AGENT: String = "chat.row.child.open"
    const val FILE_CHANGE_ITEM: String = "chat.row.file-change.item"
    const val IMAGE_THUMBNAIL: String = "chat.row.image.thumbnail"
    const val ATTACHMENT_CHIP: String = "chat.attachment"
    const val ATTACHMENT_REMOVE: String = "chat.attachment.remove"
    const val COMMAND_OUTPUT_TRIMMED: String = "chat.row.command.trimmed"
    const val COMMAND_MORE_LINES: String = "chat.row.command.more"
    const val TURN_END: String = "chat.row.turn-end"
    const val COMPACTION: String = "chat.row.compaction"
    const val BOUNDARY: String = "chat.row.boundary"
    const val ERROR: String = "chat.row.error"
    const val UNRECOGNIZED: String = "chat.row.unrecognized"
}

/**
 * Callbacks emitted by [ChatRowView] without coupling rows to runtime or navigation.
 */
@Immutable
data class ChatRowCallbacks(
    val onToggleExpand: (rowId: String) -> Unit = {},
    val onToggleRun: (rowId: String) -> Unit = {},
    val onToggleGroup: (rowId: String) -> Unit = {},
    val onOpenAgent: (AgentKey) -> Unit = {},
    val onOpenFileChange: (FileRow) -> Unit = {},
    val onOpenImage: (hash: List<Int>, label: String) -> Unit = { _, _ -> },
    val onOpenAttachment: (AttachmentView) -> Unit = {},
    val onOpenLink: (OpenableLink) -> Unit = {},
    val onCopyCode: (String) -> Unit = {},
)

private object RowGridMetrics {
    val GlyphCell: Dp = 20.dp
    val GlyphGap: Dp = 10.dp
    val NestedIndent: Dp = GlyphCell + GlyphGap
    val InRunBottom: Dp = 6.dp
    val AfterRunBottom: Dp = 12.dp
    val ProseBottom: Dp = 12.dp
    const val THUMBNAIL_SIZE_LIMIT: Long = 16L * 1024L * 1024L
    const val COMMAND_OUTPUT_HEAD_LINES: Int = 3
}

/**
 * Renders one chat [Row] according to the shared chat vocabulary.
 *
 * Returns early when [Row.collapsed] is true or [Row.kind] is [RowKind.Hidden].
 */
@Composable
fun ChatRowView(
    row: Row,
    modifier: Modifier = Modifier,
    expanded: Boolean = false,
    rail: RailJoin = RailJoin.NONE,
    placement: RunPlacement = RunPlacement.Own,
    openedAgents: Map<List<Int>, OpenedAgent> = emptyMap(),
    opened: OpenedAgent? = row.opens?.let { openedAgents[it] },
    groupState: GroupState? = null,
    imageBytes: (BlobRef) -> ByteArray? = { null },
    callbacks: ChatRowCallbacks = ChatRowCallbacks(),
) {
    if (row.collapsed || row.kind is RowKind.Hidden) return

    Box(
        modifier = modifier
            .fillMaxWidth()
            .testTag(ChatRowTags.row(row.id)),
    ) {
        val group = row.group
        if (group != null && row.id == group.first) {
            if (group.open) {
                Column(modifier = Modifier.fillMaxWidth()) {
                    SubagentGroupLine(
                        row = row,
                        group = group,
                        groupState = groupState,
                        rail = RailJoin(continues = true, nested = rail.nested),
                        onToggleGroup = { callbacks.onToggleGroup(row.id) },
                    )
                    ChatRowKindView(
                        row = row,
                        expanded = expanded,
                        rail = rail,
                        opened = opened,
                        unresolvedFailure = false,
                        imageBytes = imageBytes,
                        callbacks = callbacks,
                    )
                }
            } else {
                SubagentGroupLine(
                    row = row,
                    group = group,
                    groupState = groupState,
                    rail = rail,
                    onToggleGroup = { callbacks.onToggleGroup(row.id) },
                )
            }
        } else {
            when (placement) {
                RunPlacement.Own -> {
                    ChatRowKindView(
                        row = row,
                        expanded = expanded,
                        rail = rail,
                        opened = opened,
                        unresolvedFailure = false,
                        imageBytes = imageBytes,
                        callbacks = callbacks,
                    )
                }
                is RunPlacement.Step -> {
                    Column(modifier = Modifier.fillMaxWidth()) {
                        RunHeaderLine(
                            words = placement.header.words,
                            open = placement.header.open,
                            rail = RailJoin(continues = true, nested = rail.nested),
                            onToggleRun = { callbacks.onToggleRun(row.id) },
                        )
                        ChatRowKindView(
                            row = row,
                            expanded = expanded,
                            rail = rail,
                            opened = opened,
                            unresolvedFailure = false,
                            imageBytes = imageBytes,
                            callbacks = callbacks,
                        )
                    }
                }
                is RunPlacement.Folded -> {
                    val unresolved = row.run?.unresolvedFailure == true
                    Column(modifier = Modifier.fillMaxWidth()) {
                        RunHeaderLine(
                            words = placement.words,
                            open = false,
                            rail = if (unresolved) RailJoin(continues = true, nested = rail.nested) else rail,
                            onToggleRun = { callbacks.onToggleRun(row.id) },
                        )
                        if (unresolved) {
                            ChatRowKindView(
                                row = row,
                                expanded = expanded,
                                rail = rail,
                                opened = opened,
                                unresolvedFailure = true,
                                imageBytes = imageBytes,
                                callbacks = callbacks,
                            )
                        }
                    }
                }
                RunPlacement.Failed -> {
                    ChatRowKindView(
                        row = row,
                        expanded = expanded,
                        rail = rail,
                        opened = opened,
                        unresolvedFailure = true,
                        imageBytes = imageBytes,
                        callbacks = callbacks,
                    )
                }
            }
        }
    }
}

@Composable
private fun RunHeaderLine(
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
private fun SubagentGroupLine(
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
private fun ChatRowKindView(
    row: Row,
    expanded: Boolean,
    rail: RailJoin,
    opened: OpenedAgent?,
    unresolvedFailure: Boolean,
    imageBytes: (BlobRef) -> ByteArray?,
    callbacks: ChatRowCallbacks,
) {
    val denialNote = remember(row.decision) {
        val decision = row.decision
        if (decision != null && decision.outcome == DecisionView.Denied && !decision.note.isNullOrEmpty()) {
            ChatWords.firstLine(decision.note!!)
        } else {
            null
        }
    }

    when (val kind = row.kind) {
        is RowKind.Prompt -> {
            PromptBubbleRow(
                segments = kind.text,
                steered = kind.steered,
                imageBytes = imageBytes,
                onOpenImage = callbacks.onOpenImage,
                onOpenAttachment = callbacks.onOpenAttachment,
                onOpenLink = callbacks.onOpenLink,
                onCopyCode = callbacks.onCopyCode,
            )
        }
        is RowKind.Prose -> {
            ProseRowView(
                segments = kind.text,
                streaming = kind.streaming,
                workingNote = kind.workingNote,
                imageBytes = imageBytes,
                onOpenImage = callbacks.onOpenImage,
                onOpenAttachment = callbacks.onOpenAttachment,
                onOpenLink = callbacks.onOpenLink,
                onCopyCode = callbacks.onCopyCode,
            )
        }
        is RowKind.Thinking -> {
            GridRow(
                kindTag = "thinking",
                glyph = Icons.Outlined.MoreHoriz,
                rail = rail,
                verb = ChatWords.thinking(open = kind.open, durationMs = kind.durationMs),
                quiet = true,
                detail = if (expanded && kind.text.isNotEmpty()) kind.text else null,
                detailMono = false,
                opens = kind.text.isNotEmpty(),
                open = expanded,
                onClick = { callbacks.onToggleExpand(row.id) },
            )
        }
        is RowKind.Explore -> {
            val stateWord = ChatWords.state(kind.state, decided = row.decision != null).orEmpty()
            GridRow(
                kindTag = "explore",
                glyph = callGlyph(kind.state, Icons.Outlined.Search),
                accented = isAccented(row, kind.state),
                failed = kind.state == CallPhase.Failed,
                unresolvedFailure = unresolvedFailure,
                rail = rail,
                verb = ChatWords.explore(kind.verb),
                subject = kind.subject,
                meta = RowMeta(ChatWords.meta(listOf(stateWord), row, note = false)),
                quote = denialNote,
                detail = if (expanded && kind.result.isNotEmpty()) kind.result else null,
                opens = kind.result.isNotEmpty(),
                open = expanded,
                onClick = { callbacks.onToggleExpand(row.id) },
            )
        }
        is RowKind.Command -> {
            CommandRowView(
                row = row,
                kind = kind,
                expanded = expanded,
                rail = rail,
                unresolvedFailure = unresolvedFailure,
                denialNote = denialNote,
                onToggleExpand = { callbacks.onToggleExpand(row.id) },
            )
        }
        is RowKind.FileChange -> {
            FileChangeRowView(
                row = row,
                files = kind.files,
                state = kind.state,
                rail = rail,
                unresolvedFailure = unresolvedFailure,
                denialNote = denialNote,
                onOpenFileChange = callbacks.onOpenFileChange,
            )
        }
        is RowKind.ToolCall -> {
            val verb = ChatWords.verb(
                phase = kind.state,
                row = row,
                wants = stringResource(R.string.chat_row_tool_wants),
                doing = stringResource(R.string.chat_row_tool_using),
                done = stringResource(R.string.chat_row_tool_used),
            )
            val failedWord = if (kind.state == CallPhase.Failed) stringResource(R.string.chat_row_failed) else ""
            val subject = if (kind.server.isEmpty()) kind.tool else "${kind.server} · ${kind.tool}"
            GridRow(
                kindTag = "tool",
                glyph = callGlyph(kind.state, Icons.Outlined.Build),
                accented = isAccented(row, kind.state),
                failed = kind.state == CallPhase.Failed,
                unresolvedFailure = unresolvedFailure,
                rail = rail,
                verb = verb,
                subject = subject,
                meta = RowMeta(
                    ChatWords.meta(
                        meta = listOf(kind.fact, failedWord),
                        row = row,
                        verbPhase = kind.state,
                        note = false,
                    ),
                ),
                quote = denialNote,
                detail = if (expanded && kind.result.isNotEmpty()) kind.result else null,
                opens = kind.result.isNotEmpty(),
                open = expanded,
                onClick = { callbacks.onToggleExpand(row.id) },
            )
        }
        is RowKind.Child -> {
            ChildRowView(
                row = row,
                kind = kind,
                expanded = expanded,
                rail = rail,
                opened = opened,
                unresolvedFailure = unresolvedFailure,
                denialNote = denialNote,
                onToggleExpand = { callbacks.onToggleExpand(row.id) },
                onOpenAgent = callbacks.onOpenAgent,
            )
        }
        is RowKind.Subagent -> {
            SubagentRowView(
                row = row,
                kind = kind,
                expanded = expanded,
                rail = rail,
                opened = opened,
                onToggleExpand = { callbacks.onToggleExpand(row.id) },
                onOpenAgent = callbacks.onOpenAgent,
            )
        }
        is RowKind.Background -> {
            val said = ChatWords.described(kind.description)
            val metaText = when {
                said == null && kind.running -> stringResource(R.string.chat_row_running)
                said == null -> stringResource(R.string.chat_row_finished)
                kind.running -> stringResource(R.string.chat_row_in_background_running)
                else -> stringResource(R.string.chat_row_in_background_finished)
            }
            GridRow(
                kindTag = "background",
                glyph = Icons.Outlined.PlayArrow,
                rail = rail,
                verb = said ?: stringResource(R.string.chat_row_in_background),
                subject = if (said == null) ChatWords.firstLine(kind.command) else "",
                meta = RowMeta(metaText),
                note = if (said != null) ChatWords.commandLine(kind.command) else null,
            )
        }
        is RowKind.Image -> {
            val verb = stringResource(
                if (kind.generated) R.string.chat_row_image_generated else R.string.chat_row_image_label,
            )
            val label = kind.path.ifEmpty { kind.image?.name.orEmpty() }
            val metaText = kind.image?.let { ChatWords.bytes(it.size) }.orEmpty()
            GridRow(
                kindTag = "image",
                glyph = Icons.Outlined.Image,
                rail = rail,
                verb = verb,
                subject = label,
                meta = RowMeta(metaText),
                below = {
                    val blob = kind.image
                    if (blob != null) {
                        if (blob.size <= RowGridMetrics.THUMBNAIL_SIZE_LIMIT) {
                            ImageThumbnailCard(
                                blob = blob,
                                label = label.ifEmpty { blob.name },
                                bytes = imageBytes(blob),
                                named = false,
                                onOpen = { callbacks.onOpenImage(blob.hash, label.ifEmpty { blob.name }) },
                                modifier = Modifier.padding(top = Space.xs),
                            )
                        } else {
                            AttachmentChip(
                                view = AttachmentView.Image(blob),
                                onClick = { callbacks.onOpenImage(blob.hash, blob.name) },
                                modifier = Modifier.padding(top = Space.xs),
                            )
                        }
                    } else if (label.isNotEmpty()) {
                        ImagePlaceholderCard(
                            label = label,
                            onOpen = { callbacks.onOpenImage(emptyList(), label) },
                            modifier = Modifier.padding(top = Space.xs),
                        )
                    }
                },
            )
        }
        is RowKind.SlashOutput -> {
            GridRow(
                kindTag = "slash",
                glyph = Icons.Outlined.Terminal,
                rail = rail,
                verb = kind.command,
                subject = kind.args,
                detail = kind.output.ifEmpty { null },
                detailMaxLines = if (expanded) Int.MAX_VALUE else 6,
                opens = kind.output.isNotEmpty(),
                open = expanded,
                onClick = { callbacks.onToggleExpand(row.id) },
            )
        }
        is RowKind.AgentMessage -> {
            val received = kind.to.isEmpty()
            val body = ChatWords.text(kind.text).trim()
            val meta = when {
                kind.kind == EnvelopeKind.Finished -> stringResource(R.string.chat_row_finished)
                kind.kind == EnvelopeKind.Failed -> stringResource(R.string.chat_row_failed)
                kind.sent == SendState.Sending -> stringResource(R.string.chat_row_sending)
                kind.sent == SendState.Rejected -> {
                    if (kind.rejection.isNotEmpty()) {
                        stringResource(R.string.chat_row_agent_not_delivered, kind.rejection)
                    } else {
                        stringResource(R.string.chat_row_agent_not_delivered_plain)
                    }
                }
                else -> ""
            }
            GridRow(
                kindTag = "agent-message",
                glyph = if (received) Icons.AutoMirrored.Outlined.CallReceived else Icons.AutoMirrored.Outlined.CallMade,
                failed = kind.sent == SendState.Rejected,
                rail = rail,
                verb = stringResource(if (received) R.string.chat_row_agent_from else R.string.chat_row_agent_to),
                subject = if (received) kind.from else kind.to,
                meta = RowMeta(meta),
                detail = body.ifEmpty { null },
                detailMono = false,
                detailMaxLines = if (expanded) Int.MAX_VALUE else 1,
                opens = body.isNotEmpty(),
                open = expanded,
                onClick = { callbacks.onToggleExpand(row.id) },
            )
        }
        is RowKind.Ask -> {
            AskRowView(
                ask = kind.value,
                rail = rail,
                expanded = expanded,
                onToggleExpand = { callbacks.onToggleExpand(row.id) },
                onOpenLink = callbacks.onOpenLink,
                onCopyCode = callbacks.onCopyCode,
            )
        }
        is RowKind.TurnEnd -> {
            val colors = AmuxTheme.colors
            val text = ChatWords.turnEnd(durationMs = kind.durationMs, failed = kind.failed)
            Text(
                text = text,
                style = AmuxTheme.typography.caption,
                color = if (kind.failed) colors.removed else colors.inkFaint,
                modifier = Modifier
                    .padding(bottom = RowGridMetrics.AfterRunBottom)
                    .testTag(ChatRowTags.TURN_END),
            )
        }
        RowKind.Stopped -> {
            GridRow(
                kindTag = "stopped",
                glyph = Icons.Outlined.Stop,
                rail = rail,
                verb = stringResource(R.string.chat_row_stopped),
                quiet = true,
            )
        }
        is RowKind.Compaction -> {
            FeedRuleRow(
                label = ChatWords.compaction(
                    before = kind.tokensBefore,
                    after = kind.tokensAfter,
                    automatic = kind.automatic,
                    idle = kind.idle,
                ),
                tag = ChatRowTags.COMPACTION,
            )
        }
        is RowKind.Error -> {
            ErrorRowCard(
                row = row,
                kind = kind,
                expanded = expanded,
                onToggleExpand = { callbacks.onToggleExpand(row.id) },
            )
        }
        is RowKind.ModelSwitch -> {
            GridRow(
                kindTag = "model-switch",
                glyph = Icons.Outlined.SwapHoriz,
                rail = rail,
                verb = stringResource(R.string.chat_row_model_switched),
                subject = kind.to,
                note = kind.reason.ifEmpty { null },
            )
        }
        is RowKind.Boundary -> {
            FeedRuleRow(
                label = ChatWords.boundary(kind.kind, cause = kind.cause),
                tag = ChatRowTags.BOUNDARY,
            )
        }
        is RowKind.AutoReview -> {
            val riskText = if (kind.risk.isEmpty()) "" else stringResource(R.string.chat_row_auto_review_risk, kind.risk)
            GridRow(
                kindTag = "auto-review",
                glyph = Icons.Outlined.VerifiedUser,
                rail = rail,
                verb = ChatWords.autoReview(kind.status, kind.statusWord),
                meta = RowMeta(riskText),
                detail = if (expanded && kind.rationale.isNotEmpty()) kind.rationale else null,
                detailMono = false,
                opens = kind.rationale.isNotEmpty(),
                open = expanded,
                onClick = { callbacks.onToggleExpand(row.id) },
            )
        }
        is RowKind.Unrecognized -> {
            GridRow(
                kindTag = "unrecognized",
                glyph = Icons.AutoMirrored.Outlined.HelpOutline,
                rail = rail,
                verb = stringResource(R.string.chat_row_unrecognized),
                subject = kind.what,
                note = if (kind.summary.isEmpty()) null else ChatWords.firstLine(kind.summary),
                modifier = Modifier.testTag(ChatRowTags.UNRECOGNIZED),
            )
        }
        RowKind.Hidden -> Unit
    }
}

@Composable
private fun CommandRowView(
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
private fun FileChangeRowView(
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
private fun ChildRowView(
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
private fun SubagentRowView(
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
            modifier = Modifier.size(16.dp),
        )
    }
}

@Composable
internal fun PromptBubbleRow(
    segments: List<Segment>,
    steered: Boolean,
    imageBytes: (BlobRef) -> ByteArray?,
    onOpenImage: (List<Int>, String) -> Unit,
    onOpenAttachment: (AttachmentView) -> Unit,
    onOpenLink: (OpenableLink) -> Unit,
    onCopyCode: (String) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val metrics = AmuxTheme.metrics

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(bottom = RowGridMetrics.ProseBottom)
            .testTag(ChatRowTags.kind("prompt")),
        horizontalArrangement = Arrangement.End,
    ) {
        Spacer(modifier = Modifier.width(44.dp))
        Column(
            horizontalAlignment = Alignment.End,
            verticalArrangement = Arrangement.spacedBy(Space.xs),
            modifier = Modifier.weight(1f, fill = false),
        ) {
            Surface(
                shape = RoundedCornerShape(metrics.controlRadius + 3.dp),
                color = colors.sunken,
                contentColor = colors.ink,
            ) {
                SegmentsColumn(
                    segments = segments,
                    inBubble = true,
                    workingNote = false,
                    streaming = false,
                    imageBytes = imageBytes,
                    onOpenImage = onOpenImage,
                    onOpenAttachment = onOpenAttachment,
                    onOpenLink = onOpenLink,
                    onCopyCode = onCopyCode,
                    modifier = Modifier.padding(horizontal = 14.dp, vertical = 10.dp),
                )
            }
            if (steered) {
                Text(
                    text = stringResource(R.string.chat_row_steered),
                    style = typography.caption,
                    color = colors.inkFaint,
                )
            }
        }
    }
}

@Composable
private fun ProseRowView(
    segments: List<Segment>,
    streaming: Boolean,
    workingNote: Boolean,
    imageBytes: (BlobRef) -> ByteArray?,
    onOpenImage: (List<Int>, String) -> Unit,
    onOpenAttachment: (AttachmentView) -> Unit,
    onOpenLink: (OpenableLink) -> Unit,
    onCopyCode: (String) -> Unit,
) {
    SegmentsColumn(
        segments = segments,
        inBubble = false,
        workingNote = workingNote,
        streaming = streaming,
        imageBytes = imageBytes,
        onOpenImage = onOpenImage,
        onOpenAttachment = onOpenAttachment,
        onOpenLink = onOpenLink,
        onCopyCode = onCopyCode,
        modifier = Modifier
            .fillMaxWidth()
            .padding(bottom = RowGridMetrics.ProseBottom)
            .testTag(ChatRowTags.kind("prose")),
    )
}

@Composable
private fun SegmentsColumn(
    segments: List<Segment>,
    inBubble: Boolean,
    workingNote: Boolean,
    streaming: Boolean,
    imageBytes: (BlobRef) -> ByteArray?,
    onOpenImage: (List<Int>, String) -> Unit,
    onOpenAttachment: (AttachmentView) -> Unit,
    onOpenLink: (OpenableLink) -> Unit,
    onCopyCode: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val lastTextIndex = segments.indexOfLast { it is Segment.Text }
    Column(
        modifier = modifier,
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        segments.forEachIndexed { index, segment ->
            when (segment) {
                is Segment.Text -> {
                    val isStreamingThis = streaming && index == lastTextIndex
                    if (segment.value.isNotBlank() || isStreamingThis) {
                        MarkdownView(
                            markdown = segment.value,
                            workingNote = workingNote,
                            streaming = isStreamingThis,
                            inBubble = inBubble,
                            onOpenLink = onOpenLink,
                            onCopyCode = onCopyCode,
                        )
                    }
                }
                is Segment.Attachment -> {
                    MessageAttachmentView(
                        view = segment.value,
                        imageBytes = imageBytes,
                        onOpenImage = onOpenImage,
                        onOpenAttachment = onOpenAttachment,
                    )
                }
            }
        }
        if (streaming && lastTextIndex < 0) {
            MarkdownView(
                markdown = "",
                workingNote = workingNote,
                streaming = true,
                inBubble = inBubble,
            )
        }
    }
}

@Composable
private fun MessageAttachmentView(
    view: AttachmentView,
    imageBytes: (BlobRef) -> ByteArray?,
    onOpenImage: (List<Int>, String) -> Unit,
    onOpenAttachment: (AttachmentView) -> Unit,
) {
    when (view) {
        is AttachmentView.Image -> {
            if (view.value.size <= RowGridMetrics.THUMBNAIL_SIZE_LIMIT) {
                ImageThumbnailCard(
                    blob = view.value,
                    label = view.value.name,
                    bytes = imageBytes(view.value),
                    named = true,
                    onOpen = { onOpenImage(view.value.hash, view.value.name) },
                )
            } else {
                AttachmentChip(
                    view = view,
                    onClick = { onOpenAttachment(view) },
                )
            }
        }
        is AttachmentView.File -> {
            AttachmentChip(
                view = view,
                onClick = { onOpenAttachment(view) },
            )
        }
        else -> {
            AttachmentChip(
                view = view,
                onClick = null,
            )
        }
    }
}

/**
 * Shared attachment chip used in both prompt bubbles and [ChatComposer].
 */
@Composable
fun AttachmentChip(
    view: AttachmentView,
    modifier: Modifier = Modifier,
    thumbnailBytes: ByteArray? = null,
    onClick: (() -> Unit)? = null,
    onRemove: (() -> Unit)? = null,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val metrics = AmuxTheme.metrics
    val parts = remember(view) { ChatWords.chipParts(view) }
    val chipLabel = remember(view) { ChatWords.chip(view) }
    val bitmap = remember(thumbnailBytes) {
        thumbnailBytes?.let { bytes ->
            runCatching { BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap() }.getOrNull()
        }
    }
    val openDescription = stringResource(R.string.chat_row_attachment_open_named, chipLabel)
    val removeDescription = stringResource(R.string.chat_row_attachment_remove)

    val containerModifier = if (onClick != null) {
        modifier
            .thumbTarget()
            .clip(RoundedCornerShape(50))
            .clickable(role = Role.Button, onClick = onClick)
            .semantics { contentDescription = openDescription }
    } else {
        modifier.semantics { contentDescription = chipLabel }
    }

    Surface(
        modifier = containerModifier.testTag(ChatRowTags.ATTACHMENT_CHIP),
        shape = RoundedCornerShape(50),
        color = colors.raised,
        contentColor = colors.inkMuted,
        border = BorderStroke(metrics.hairline, colors.hairline),
    ) {
        Row(
            modifier = Modifier.padding(
                start = Space.s + Space.xxs,
                end = if (onRemove != null) Space.xxs else Space.s + Space.xxs,
                top = Space.xs,
                bottom = Space.xs,
            ),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.xs + Space.xxs),
        ) {
            if (bitmap != null) {
                Image(
                    bitmap = bitmap,
                    contentDescription = null,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier
                        .size(24.dp)
                        .clip(RoundedCornerShape(6.dp)),
                )
            } else {
                Icon(
                    imageVector = attachmentKindIcon(ChatWords.chipKind(view)),
                    contentDescription = null,
                    tint = colors.inkMuted,
                    modifier = Modifier.size(14.dp),
                )
            }
            Text(
                text = chipLabel,
                style = typography.caption,
                color = colors.inkMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.widthIn(max = 220.dp),
            )
            if (onRemove != null) {
                IconButton(
                    onClick = onRemove,
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(ChatRowTags.ATTACHMENT_REMOVE),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.Close,
                        contentDescription = removeDescription,
                        tint = colors.inkFaint,
                        modifier = Modifier.size(16.dp),
                    )
                }
            }
        }
    }
}

internal fun attachmentKindIcon(kind: ChatWords.AttachmentKind): ImageVector = when (kind) {
    ChatWords.AttachmentKind.Image -> Icons.Outlined.Image
    ChatWords.AttachmentKind.File -> Icons.Outlined.Description
    ChatWords.AttachmentKind.Text -> Icons.AutoMirrored.Outlined.Notes
    ChatWords.AttachmentKind.Review -> Icons.Outlined.RateReview
    ChatWords.AttachmentKind.Page -> Icons.Outlined.Language
    ChatWords.AttachmentKind.Empty -> Icons.Outlined.AttachFile
}

@Composable
private fun ImageThumbnailCard(
    blob: BlobRef,
    label: String,
    bytes: ByteArray?,
    named: Boolean,
    onOpen: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val metrics = AmuxTheme.metrics
    val bitmap = remember(bytes) {
        bytes?.let { data ->
            runCatching { BitmapFactory.decodeByteArray(data, 0, data.size)?.asImageBitmap() }.getOrNull()
        }
    }
    val openDescription = stringResource(R.string.chat_row_image_open_named, label)
    val caption = remember(blob) { ChatWords.chip(AttachmentView.Image(blob)) }

    Column(
        modifier = modifier
            .thumbTarget()
            .clip(RoundedCornerShape(metrics.controlRadius))
            .clickable(role = Role.Button, onClick = onOpen)
            .semantics { contentDescription = openDescription }
            .testTag(ChatRowTags.IMAGE_THUMBNAIL),
        verticalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        Surface(
            modifier = Modifier.size(width = 240.dp, height = 170.dp),
            shape = RoundedCornerShape(metrics.controlRadius),
            color = colors.sunken,
            border = BorderStroke(metrics.hairline, colors.hairline),
        ) {
            if (bitmap != null) {
                Image(
                    bitmap = bitmap,
                    contentDescription = null,
                    contentScale = ContentScale.Fit,
                )
            } else {
                Column(
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.Center,
                    modifier = Modifier.padding(Space.m),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.Image,
                        contentDescription = null,
                        tint = colors.inkFaint,
                        modifier = Modifier.size(28.dp),
                    )
                    if (bytes != null) {
                        Spacer(modifier = Modifier.height(Space.xs))
                        Text(
                            text = stringResource(R.string.chat_row_image_no_preview),
                            style = typography.caption,
                            color = colors.inkFaint,
                        )
                    }
                }
            }
        }
        if (named) {
            Text(
                text = caption,
                style = typography.caption,
                color = colors.inkMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.widthIn(max = 240.dp),
            )
        }
    }
}

@Composable
private fun ImagePlaceholderCard(
    label: String,
    onOpen: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val metrics = AmuxTheme.metrics
    val openDescription = stringResource(R.string.chat_row_image_open_named, label)

    Surface(
        modifier = modifier
            .thumbTarget()
            .clip(RoundedCornerShape(metrics.controlRadius))
            .clickable(role = Role.Button, onClick = onOpen)
            .semantics { contentDescription = openDescription }
            .testTag(ChatRowTags.IMAGE_THUMBNAIL),
        shape = RoundedCornerShape(metrics.controlRadius),
        color = colors.sunken,
        border = BorderStroke(metrics.hairline, colors.hairline),
    ) {
        Row(
            modifier = Modifier.padding(horizontal = Space.m, vertical = Space.s),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            Icon(
                imageVector = Icons.Outlined.Image,
                contentDescription = null,
                tint = colors.inkMuted,
                modifier = Modifier.size(18.dp),
            )
            Text(
                text = label,
                style = typography.caption,
                color = colors.inkMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
private fun ErrorRowCard(
    row: Row,
    kind: RowKind.Error,
    expanded: Boolean,
    onToggleExpand: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val detailed = kind.errorKind.isNotEmpty() && kind.message.isNotEmpty()
    val title = kind.errorKind.ifEmpty { ChatWords.firstLine(kind.message) }
    val attemptsText = when {
        kind.attempts <= 1 -> ""
        kind.gaveUp -> stringResource(R.string.chat_row_error_gave_up, kind.attempts)
        else -> stringResource(R.string.chat_row_error_tries, kind.attempts)
    }
    val tint = if (row.attention) colors.accent else colors.removed
    val cardModifier = if (detailed) {
        Modifier
            .fillMaxWidth()
            .padding(bottom = RowGridMetrics.AfterRunBottom)
            .thumbTarget()
            .clip(RoundedCornerShape(AmuxTheme.metrics.cardRadius))
            .clickable(role = Role.Button, onClick = onToggleExpand)
            .testTag(ChatRowTags.ERROR)
    } else {
        Modifier
            .fillMaxWidth()
            .padding(bottom = RowGridMetrics.AfterRunBottom)
            .testTag(ChatRowTags.ERROR)
    }

    AmuxPanel(
        modifier = cardModifier,
        borderColor = tint.copy(alpha = 0.45f),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.m, vertical = Space.m),
            horizontalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
            verticalAlignment = Alignment.Top,
        ) {
            Icon(
                imageVector = Icons.Outlined.ErrorOutline,
                contentDescription = null,
                tint = tint,
                modifier = Modifier
                    .padding(top = Space.xxs)
                    .size(18.dp),
            )
            Column(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(Space.xs),
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        text = title,
                        style = typography.bodyEmphasis,
                        color = tint,
                        modifier = Modifier.weight(1f),
                    )
                    if (attemptsText.isNotEmpty()) {
                        Text(
                            text = attemptsText,
                            style = typography.caption,
                            color = colors.inkFaint,
                        )
                    }
                    if (detailed) {
                        Icon(
                            imageVector = if (expanded) Icons.Outlined.ExpandLess else Icons.Outlined.ExpandMore,
                            contentDescription = stringResource(
                                if (expanded) R.string.chat_row_collapse else R.string.chat_row_expand,
                            ),
                            tint = colors.inkFaint,
                            modifier = Modifier.size(16.dp),
                        )
                    }
                }
                if (detailed) {
                    SelectionContainer {
                        Text(
                            text = kind.message,
                            style = typography.detail,
                            color = colors.inkMuted,
                            maxLines = if (expanded) Int.MAX_VALUE else 3,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun FeedRuleRow(
    label: String,
    tag: String,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = Space.xs, bottom = RowGridMetrics.AfterRunBottom)
            .testTag(tag),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(RowGridMetrics.GlyphGap),
    ) {
        AmuxHairline(modifier = Modifier.width(RowGridMetrics.GlyphCell))
        Text(
            text = label,
            style = typography.caption,
            color = colors.inkFaint,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        AmuxHairline(modifier = Modifier.weight(1f))
    }
}

@Composable
private fun AskRowView(
    ask: AskRow,
    rail: RailJoin,
    expanded: Boolean,
    onToggleExpand: () -> Unit,
    onOpenLink: (OpenableLink) -> Unit,
    onCopyCode: (String) -> Unit,
) {
    when (ask) {
        is AskRow.Questions -> {
            AskQuestionsRowView(
                questions = ask.questions,
                answers = ask.answers,
                skipped = ask.skipped,
                resolution = ask.resolution,
                reply = ask.reply,
                rail = rail,
                onOpenLink = onOpenLink,
            )
        }
        is AskRow.Plan -> {
            val isOpen = ask.verdict == PlanVerdict.Open
            val sentBackNote = if (ask.verdict == PlanVerdict.SentBack && !ask.note.isNullOrEmpty()) {
                ChatWords.firstLine(ask.note!!)
            } else {
                null
            }
            val verb = when (ask.verdict) {
                PlanVerdict.Open -> stringResource(R.string.chat_row_ask_plan_proposed)
                PlanVerdict.Approved, PlanVerdict.ApprovedSwitching -> stringResource(R.string.chat_row_ask_plan_approved)
                PlanVerdict.SentBack -> stringResource(R.string.chat_row_ask_plan_sent_back)
                PlanVerdict.Dismissed -> stringResource(R.string.chat_row_ask_plan_dismissed)
            }
            val switchedName = ask.modeName.ifEmpty { ask.mode }
            val metaText = if (ask.verdict == PlanVerdict.ApprovedSwitching && switchedName.isNotEmpty()) {
                stringResource(R.string.chat_row_ask_plan_switched, switchedName)
            } else {
                ""
            }
            GridRow(
                kindTag = "plan",
                glyph = Icons.Outlined.Checklist,
                accented = isOpen,
                rail = rail,
                verb = verb,
                subject = if (sentBackNote != null) "" else ask.title.orEmpty(),
                subjectMono = false,
                meta = RowMeta(metaText),
                quote = sentBackNote,
                opens = !isOpen,
                open = expanded,
                onClick = onToggleExpand,
                below = if (isOpen || expanded) {
                    {
                        Surface(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(top = Space.xs),
                            shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                            color = AmuxTheme.colors.sunken,
                        ) {
                            MarkdownView(
                                markdown = ask.body,
                                title = ask.title,
                                onOpenLink = onOpenLink,
                                onCopyCode = onCopyCode,
                                modifier = Modifier.padding(Space.m),
                            )
                        }
                    }
                } else {
                    null
                },
            )
        }
        is AskRow.Form -> {
            val verb = when (ask.resolution) {
                Resolution.Answered -> if (ask.fields.size == 1) {
                    stringResource(R.string.chat_row_ask_form_sent_one)
                } else {
                    stringResource(R.string.chat_row_ask_form_sent_many, ask.fields.size)
                }
                Resolution.Open -> stringResource(R.string.chat_row_ask_form_needs_details, ask.server)
                else -> stringResource(
                    R.string.chat_row_ask_form_other,
                    ChatWords.resolution(ask.resolution, answered = ""),
                    ask.server,
                )
            }
            GridRow(
                kindTag = "form",
                glyph = Icons.Outlined.Checklist,
                accented = ask.resolution == Resolution.Open,
                rail = rail,
                verb = verb,
                subject = if (ask.resolution == Resolution.Answered) ask.server else "",
                detail = if (expanded && ask.message.isNotEmpty()) ask.message else null,
                detailMono = false,
                opens = ask.message.isNotEmpty(),
                open = expanded,
                onClick = onToggleExpand,
            )
        }
        is AskRow.Link -> {
            val verb = when (ask.resolution) {
                Resolution.Answered -> stringResource(R.string.chat_row_ask_link_opened)
                Resolution.Open -> stringResource(R.string.chat_row_ask_link_from)
                else -> ChatWords.resolution(ask.resolution, answered = "")
            }
            val detail = listOf(ask.message, ask.url).filter { it.isNotEmpty() }.joinToString("\n")
            GridRow(
                kindTag = "link",
                glyph = Icons.Outlined.Link,
                accented = ask.resolution == Resolution.Open,
                rail = rail,
                verb = verb,
                subject = ask.server,
                detail = if (expanded && detail.isNotEmpty()) detail else null,
                opens = detail.isNotEmpty(),
                open = expanded,
                onClick = onToggleExpand,
            )
        }
        is AskRow.Grant -> {
            val granted = ask.granted
            val verb = when {
                ask.resolution == Resolution.Answered && granted != null ->
                    stringResource(R.string.chat_row_ask_grant_granted)
                ask.resolution == Resolution.Open ->
                    stringResource(R.string.chat_row_ask_grant_wants)
                else -> ChatWords.resolution(
                    ask.resolution,
                    answered = stringResource(R.string.chat_row_ask_grant_denied),
                )
            }
            val accessSubject = if (granted != null) {
                formatAccessParts(granted.read, granted.write, granted.network, ask.hosts)
            } else {
                formatAccessParts(ask.read, ask.write, ask.network, ask.hosts)
            }
            val durationMeta = granted?.let {
                stringResource(if (it.forSession) R.string.chat_row_ask_grant_session else R.string.chat_row_ask_grant_turn)
            }.orEmpty()
            GridRow(
                kindTag = "grant",
                glyph = Icons.Outlined.Lock,
                accented = ask.resolution == Resolution.Open,
                rail = rail,
                verb = verb,
                subject = accessSubject,
                meta = RowMeta(durationMeta),
                detail = if (expanded && ask.reason.isNotEmpty()) ask.reason else null,
                detailMono = false,
                opens = ask.reason.isNotEmpty(),
                open = expanded,
                onClick = onToggleExpand,
            )
        }
        is AskRow.Unanswerable -> {
            val verb = if (ask.resolution == Resolution.Open) {
                stringResource(R.string.chat_row_ask_unanswerable_open)
            } else {
                val res = ChatWords.resolution(ask.resolution, answered = stringResource(R.string.chat_row_ask_answered))
                stringResource(R.string.chat_row_ask_unanswerable_resolved, res)
            }
            GridRow(
                kindTag = "unanswerable",
                glyph = Icons.Outlined.ErrorOutline,
                accented = ask.resolution == Resolution.Open,
                rail = rail,
                verb = verb,
                detail = if (expanded && ask.reason.isNotEmpty()) ask.reason else null,
                detailMono = false,
                opens = ask.reason.isNotEmpty(),
                open = expanded,
                onClick = onToggleExpand,
            )
        }
    }
}

@Composable
private fun formatAccessParts(
    read: List<String>,
    write: List<String>,
    network: Boolean,
    hosts: List<String>,
): String {
    val parts = mutableListOf<String>()
    if (write.isNotEmpty()) {
        parts.add(stringResource(R.string.chat_row_ask_access_write, write.joinToString(", ")))
    }
    if (read.isNotEmpty()) {
        parts.add(stringResource(R.string.chat_row_ask_access_read, read.joinToString(", ")))
    }
    if (network) {
        if (hosts.isEmpty()) {
            parts.add(stringResource(R.string.chat_row_ask_access_network))
        } else {
            parts.add(stringResource(R.string.chat_row_ask_access_network_hosts, hosts.joinToString(", ")))
        }
    }
    return parts.joinToString(" · ")
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun AskQuestionsRowView(
    questions: List<QuestionView>,
    answers: List<AnswerView>,
    skipped: Int,
    resolution: Resolution,
    reply: String?,
    rail: RailJoin,
    onOpenLink: (OpenableLink) -> Unit,
) {
    val single = questions.size == 1
    val given = answers.size - skipped
    val some = resolution == Resolution.Answered && skipped > 0
    val rawPairs = questions.zip(answers)
    val pairs = if (resolution == Resolution.Replied) {
        rawPairs.filter { !it.second.skipped }
    } else {
        rawPairs
    }

    val verb = if (some && given == 0) {
        stringResource(R.string.chat_row_ask_skipped)
    } else {
        ChatWords.resolution(resolution, answered = stringResource(R.string.chat_row_ask_answered))
    }
    val subject = when {
        some && given > 0 -> stringResource(R.string.chat_row_ask_questions_of, given, questions.size)
        single -> ""
        else -> stringResource(R.string.chat_row_ask_questions_count, questions.size)
    }

    GridRow(
        kindTag = "question",
        glyph = Icons.AutoMirrored.Outlined.HelpOutline,
        accented = resolution == Resolution.Open,
        rail = rail,
        verb = verb,
        subject = subject,
        subjectMono = false,
        below = if (pairs.isNotEmpty() || reply != null) {
            {
                AmuxPanel(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = Space.xs),
                    shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius + 2.dp),
                ) {
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(horizontal = Space.m, vertical = Space.s + Space.xxs),
                        verticalArrangement = Arrangement.spacedBy(Space.s),
                    ) {
                        pairs.forEachIndexed { index, (question, answer) ->
                            if (index > 0) AmuxHairline()
                            Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
                                val promptLabel = if (pairs.size == 1 || question.header.isEmpty()) {
                                    question.question
                                } else {
                                    question.header
                                }
                                Text(
                                    text = rememberInlineMarkdown(promptLabel, workingNote = true, onOpenLink = onOpenLink),
                                    style = AmuxTheme.typography.detail,
                                    color = AmuxTheme.colors.inkMuted,
                                )
                                AskAnswerValueView(answer = answer, onOpenLink = onOpenLink)
                                if (!answer.note.isNullOrEmpty()) {
                                    Text(
                                        text = stringResource(R.string.chat_row_ask_note, ChatWords.firstLine(answer.note!!)),
                                        style = AmuxTheme.typography.detail,
                                        color = AmuxTheme.colors.inkMuted,
                                    )
                                }
                            }
                        }
                        if (!reply.isNullOrEmpty()) {
                            Text(
                                text = stringResource(R.string.chat_row_ask_replied, ChatWords.firstLine(reply)),
                                style = AmuxTheme.typography.detail,
                                color = AmuxTheme.colors.inkMuted,
                            )
                        }
                    }
                }
            }
        } else {
            null
        },
    )
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun AskAnswerValueView(
    answer: AnswerView,
    onOpenLink: (OpenableLink) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val totalCount = answer.picked.size + if (answer.other != null) 1 else 0

    if (answer.hidden || totalCount < 2) {
        val text = if (answer.picked.isEmpty()) {
            AnnotatedString(ChatWords.answer(answer))
        } else {
            rememberInlineMarkdown(answer.picked.first(), onOpenLink = onOpenLink)
        }
        Text(
            text = text,
            style = typography.bodyEmphasis.copy(
                fontStyle = if (answer.picked.isEmpty() && !answer.hidden) FontStyle.Italic else FontStyle.Normal,
            ),
            color = colors.ink,
        )
    } else {
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(Space.xs + Space.xxs),
            verticalArrangement = Arrangement.spacedBy(Space.xs),
        ) {
            answer.picked.forEach { pick ->
                Surface(
                    shape = RoundedCornerShape(50),
                    color = colors.ground,
                    border = BorderStroke(AmuxTheme.metrics.hairline, colors.hairline),
                ) {
                    Text(
                        text = rememberInlineMarkdown(pick, onOpenLink = onOpenLink),
                        style = typography.detail,
                        color = colors.ink,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.padding(horizontal = Space.s + Space.xxs, vertical = Space.xxs),
                    )
                }
            }
            if (answer.other != null) {
                Text(
                    text = stringResource(R.string.chat_row_quoted, answer.other!!),
                    style = typography.detail.copy(fontStyle = FontStyle.Italic),
                    color = colors.ink,
                )
            }
        }
    }
}

private data class RowMeta(
    val runs: List<Run> = emptyList(),
) {
    enum class Ink { Faint, Added, Removed, Accent }

    data class Run(
        val text: String,
        val ink: Ink,
    )

    constructor(text: String) : this(
        if (text.isEmpty()) emptyList() else listOf(Run(text, Ink.Faint)),
    )

    val isEmpty: Boolean
        get() = runs.isEmpty()

    fun then(text: String): RowMeta {
        if (text.isEmpty()) return this
        if (runs.isEmpty()) return RowMeta(text)
        return RowMeta(runs + Run(" · $text", Ink.Faint))
    }

    fun toAnnotatedString(colors: AmuxColors): AnnotatedString = buildAnnotatedString {
        runs.forEach { run ->
            val color = when (run.ink) {
                Ink.Faint -> colors.inkFaint
                Ink.Added -> colors.added
                Ink.Removed -> colors.removed
                Ink.Accent -> colors.accent
            }
            withStyle(SpanStyle(color = color)) {
                append(run.text)
            }
        }
    }

    companion object {
        fun change(added: Int, removed: Int): RowMeta {
            val parts = mutableListOf<Run>()
            if (added > 0) parts.add(Run("+$added", Ink.Added))
            if (added > 0 && removed > 0) parts.add(Run(" ", Ink.Faint))
            if (removed > 0) parts.add(Run("\u2212$removed", Ink.Removed))
            return RowMeta(parts)
        }

        fun standing(standing: AgentStanding): RowMeta =
            RowMeta(listOf(Run(standing.words, inkFor(standing))))

        fun group(state: GroupState): RowMeta {
            val runs = mutableListOf<Run>()
            ChatWords.subagentGroupParts(state).forEachIndexed { index, part ->
                if (index > 0) runs.add(Run(" · ", Ink.Faint))
                runs.add(Run("${part.count} ${part.standing.words}", inkFor(part.standing)))
            }
            return RowMeta(runs)
        }

        private fun inkFor(standing: AgentStanding): Ink = when (standing) {
            AgentStanding.NeedsYou -> Ink.Accent
            AgentStanding.Failed -> Ink.Removed
            AgentStanding.Running, AgentStanding.Idle, AgentStanding.Done, AgentStanding.Stopped -> Ink.Faint
        }
    }
}

private fun callGlyph(phase: CallPhase, defaultGlyph: ImageVector): ImageVector = when (phase) {
    CallPhase.Denied, CallPhase.Asking -> Icons.Outlined.FrontHand
    CallPhase.Cancelled -> Icons.Outlined.Block
    CallPhase.Pending, CallPhase.Running, CallPhase.Succeeded, CallPhase.Failed -> defaultGlyph
}

private fun isAccented(row: Row, phase: CallPhase): Boolean =
    row.attention && phase == CallPhase.Asking

@Composable
private fun GridRow(
    kindTag: String,
    glyph: ImageVector,
    modifier: Modifier = Modifier,
    accented: Boolean = false,
    failed: Boolean = false,
    unresolvedFailure: Boolean = false,
    rail: RailJoin = RailJoin.NONE,
    verb: String = "",
    quiet: Boolean = false,
    subject: String = "",
    subjectMono: Boolean = true,
    subjectEmphasis: Boolean = false,
    meta: RowMeta = RowMeta(),
    note: String? = null,
    quote: String? = null,
    output: List<String> = emptyList(),
    outputTrimmed: Boolean = false,
    moreLines: Long = 0L,
    detail: String? = null,
    detailMono: Boolean = true,
    detailMaxLines: Int = Int.MAX_VALUE,
    opens: Boolean = false,
    open: Boolean = false,
    trailingChevron: Boolean = true,
    pushes: Boolean = false,
    actionDescription: String? = null,
    onClick: () -> Unit = {},
    below: (@Composable () -> Unit)? = null,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val metrics = AmuxTheme.metrics

    val glyphInk = when {
        unresolvedFailure || failed -> colors.removed
        accented -> colors.accent
        else -> colors.inkFaint
    }
    val verbColor = when {
        unresolvedFailure -> colors.removed
        quiet -> colors.inkMuted
        else -> colors.ink
    }
    val hairlineColor = colors.hairline
    val hairlinePx = metrics.hairline

    val railModifier = Modifier
        .fillMaxWidth()
        .padding(start = if (rail.nested) RowGridMetrics.NestedIndent else 0.dp)
        .drawBehind {
            if (rail.continues) {
                val x = (RowGridMetrics.GlyphCell / 2).toPx()
                val startY = if (rail.nested) 0f else (RowGridMetrics.GlyphCell - 2.dp).toPx()
                drawLine(
                    color = hairlineColor,
                    start = Offset(x, startY),
                    end = Offset(x, size.height),
                    strokeWidth = hairlinePx.toPx(),
                )
            }
        }
        .padding(bottom = if (rail.continues) RowGridMetrics.InRunBottom else RowGridMetrics.AfterRunBottom)

    val resolvedActionDesc = actionDescription ?: stringResource(
        if (open) R.string.chat_row_collapse else R.string.chat_row_expand,
    )

    val interactiveModifier = if (opens) {
        Modifier
            .fillMaxWidth()
            .thumbTarget()
            .clip(RoundedCornerShape(Space.s))
            .clickable(role = Role.Button, onClick = onClick)
            .semantics {
                contentDescription = listOf(verb, subject, resolvedActionDesc)
                    .filter { it.isNotEmpty() }
                    .joinToString(", ")
            }
    } else {
        Modifier.fillMaxWidth()
    }

    Box(modifier = modifier.then(railModifier).testTag(ChatRowTags.kind(kindTag))) {
        Row(
            modifier = interactiveModifier.padding(vertical = Space.xs),
            horizontalArrangement = Arrangement.spacedBy(RowGridMetrics.GlyphGap),
            verticalAlignment = Alignment.Top,
        ) {
            Box(
                modifier = Modifier.size(RowGridMetrics.GlyphCell),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = if (unresolvedFailure) Icons.Outlined.Close else glyph,
                    contentDescription = null,
                    tint = glyphInk,
                    modifier = Modifier.size(14.dp),
                )
            }
            Column(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(Space.xs),
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Row(
                        modifier = Modifier.weight(1f),
                        horizontalArrangement = Arrangement.spacedBy(Space.xs + Space.xxs),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        if (verb.isNotEmpty()) {
                            Text(
                                text = verb,
                                style = typography.detail,
                                color = verbColor,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                        if (subject.isNotEmpty()) {
                            Text(
                                text = subject,
                                style = if (subjectMono) typography.monoSmall else typography.detail,
                                color = if (subjectEmphasis) colors.ink else colors.inkMuted,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.weight(1f, fill = false),
                            )
                        }
                    }
                    if (!meta.isEmpty) {
                        Text(
                            text = remember(meta, colors) { meta.toAnnotatedString(colors) },
                            style = typography.caption,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    if (opens && trailingChevron) {
                        Icon(
                            imageVector = when {
                                pushes -> Icons.AutoMirrored.Outlined.KeyboardArrowRight
                                open -> Icons.Outlined.ExpandLess
                                else -> Icons.Outlined.ExpandMore
                            },
                            contentDescription = null,
                            tint = colors.inkFaint,
                            modifier = Modifier.size(14.dp),
                        )
                    }
                }

                if (!note.isNullOrEmpty()) {
                    Text(
                        text = note,
                        style = typography.monoSmall,
                        color = colors.inkFaint,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                if (!quote.isNullOrEmpty()) {
                    Text(
                        text = stringResource(R.string.chat_row_quoted, quote),
                        style = typography.detail,
                        color = colors.inkMuted,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                if (output.isNotEmpty() || outputTrimmed || moreLines > 0L) {
                    Column(verticalArrangement = Arrangement.spacedBy(1.dp)) {
                        if (outputTrimmed) {
                            Text(
                                text = stringResource(R.string.chat_row_command_trimmed),
                                style = typography.monoSmall,
                                color = colors.inkFaint,
                                modifier = Modifier.testTag(ChatRowTags.COMMAND_OUTPUT_TRIMMED),
                            )
                        }
                        output.forEach { line ->
                            Text(
                                text = line,
                                style = typography.monoSmall,
                                color = colors.inkMuted,
                                maxLines = if (open) Int.MAX_VALUE else 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                        if (moreLines > 0L) {
                            val moreText = if (moreLines == 1L) {
                                stringResource(R.string.chat_row_command_more_one)
                            } else {
                                stringResource(R.string.chat_row_command_more_many, moreLines)
                            }
                            Text(
                                text = moreText,
                                style = typography.monoSmall,
                                color = colors.inkFaint,
                                modifier = Modifier.testTag(ChatRowTags.COMMAND_MORE_LINES),
                            )
                        }
                    }
                }
                if (!detail.isNullOrEmpty()) {
                    SelectionContainer {
                        Text(
                            text = detail,
                            style = if (detailMono) typography.monoSmall else typography.detail,
                            color = if (detailMono) colors.inkMuted else colors.ink,
                            maxLines = detailMaxLines,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
                below?.invoke()
            }
        }
    }
}
