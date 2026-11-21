package sh.amux.features

import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.graphics.Color
import sh.amux.design.ThumbTarget
import androidx.compose.foundation.layout.heightIn
import sh.amux.design.IconSize
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.CallMade
import androidx.compose.material.icons.automirrored.outlined.CallReceived
import androidx.compose.material.icons.automirrored.outlined.HelpOutline
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.Block
import androidx.compose.material.icons.outlined.Build
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.FrontHand
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material.icons.outlined.MoreHoriz
import androidx.compose.material.icons.outlined.PlayArrow
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Stop
import androidx.compose.material.icons.outlined.SwapHoriz
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material.icons.outlined.VerifiedUser
import androidx.compose.material3.Icon
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
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxColors
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.values.AgentKey
import sh.amux.values.AttachmentView
import sh.amux.values.BlobRef
import sh.amux.values.CallPhase
import sh.amux.values.DecisionView
import sh.amux.values.EnvelopeKind
import sh.amux.values.FileRow
import sh.amux.values.GroupState
import sh.amux.values.OpenedAgent
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Segment
import sh.amux.values.SendState
import sh.amux.words.AgentStanding
import sh.amux.words.ChatWords
import sh.amux.words.segment
import sh.amux.words.OpenableLink
import sh.amux.words.RailJoin
import sh.amux.words.RunPlacement
import sh.amux.words.subagentGroupParts

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
    val onOpenImage: (hash: List<Int>, label: String) -> Unit = { _, _ -> },
    val onOpenAttachment: (AttachmentView) -> Unit = {},
    val onOpenLink: (OpenableLink) -> Unit = {},
    val onCopyCode: (String) -> Unit = {},
)

internal object RowGridMetrics {
    val GlyphCell: Dp = 20.dp
    val GlyphGap: Dp = 10.dp
    val NestedIndent: Dp = GlyphCell + GlyphGap
    /** The gap under a step that the next step joins by the rail, as iOS spaces a run. */
    val InRunBottom: Dp = 10.dp

    /** The gap under the last step of a run. */
    val AfterRunBottom: Dp = 14.dp

    /** The rail's clearance from the glyph above it and the glyph below it. */
    val RailGap: Dp = 2.dp
    val ProseBottom: Dp = 12.dp
    const val THUMBNAIL_SIZE_LIMIT: Long = 16L * 1024L * 1024L
    const val COMMAND_OUTPUT_HEAD_LINES: Int = 3
}

/**
 * Renders one chat [Row] according to the shared chat vocabulary.
 *
 * Returns early when [shown] is false (defaults to `!row.collapsed`) or
 * [Row.kind] is [RowKind.Hidden]. Callers that evaluate subagent-parent
 * expansion (via `ChatRowSource.shows`) pass `shown = true` so expanded
 * child steps render.
 */
@Composable
fun ChatRowView(
    row: Row,
    modifier: Modifier = Modifier,
    expanded: Boolean = false,
    shown: Boolean = !row.collapsed,
    rail: RailJoin = RailJoin.NONE,
    placement: RunPlacement = RunPlacement.Own,
    openedAgents: Map<List<Int>, OpenedAgent> = emptyMap(),
    opened: OpenedAgent? = row.opens?.let { openedAgents[it] },
    groupState: GroupState? = null,
    imageBytes: (BlobRef) -> ByteArray? = { null },
    callbacks: ChatRowCallbacks = ChatRowCallbacks(),
) {
    if (!shown || row.kind is RowKind.Hidden) return

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
                segments = kind.text.map { it.segment },
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
                segments = kind.text.map { it.segment },
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
                                imageBytes = imageBytes,
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
            .heightIn(min = ThumbTarget.MinSize)
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
                    .size(IconSize.Control),
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
                            modifier = Modifier.size(IconSize.Inline),
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

internal data class RowMeta(
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

internal fun callGlyph(phase: CallPhase, defaultGlyph: ImageVector): ImageVector = when (phase) {
    CallPhase.Denied, CallPhase.Asking -> Icons.Outlined.FrontHand
    CallPhase.Cancelled -> Icons.Outlined.Block
    CallPhase.Pending, CallPhase.Running, CallPhase.Succeeded, CallPhase.Failed -> defaultGlyph
}

internal fun isAccented(row: Row, phase: CallPhase): Boolean =
    row.attention && phase == CallPhase.Asking

/**
 * Draws a run's rail: a hairline down the glyph column at the row's outer
 * start edge, from under the row's glyph ([fromGlyph]) or its top, to its bottom,
 * where the next step's glyph cell begins.
 *
 * Apply it before any start padding: drawn after one, the line would be
 * measured from the indented edge and a nested step's rail would jog
 * sideways onto its own glyph instead of carrying its parent's on.
 */
internal fun Modifier.runRail(
    continues: Boolean,
    fromGlyph: Boolean,
    color: Color,
    width: Dp,
): Modifier = if (!continues) {
    this
} else {
    drawBehind {
        // Measured from the row's start edge, which is its right edge when
        // the layout runs right to left.
        val fromStart = RunRail.x.toPx()
        val x = if (layoutDirection == LayoutDirection.Rtl) size.width - fromStart else fromStart
        val top = if (fromGlyph) RunRail.top.toPx() else 0f
        val bottom = size.height - if (fromGlyph) RowGridMetrics.RailGap.toPx() else 0f
        drawLine(
            color = color,
            start = Offset(x, top),
            end = Offset(x, bottom),
            strokeWidth = width.toPx(),
        )
    }
}

/** Where a run's rail runs, measured from a row's outer top-start corner. */
internal object RunRail {
    /** The glyph column's centre: the same for every step, nested or not. */
    val x: Dp = RowGridMetrics.GlyphCell / 2

    /** Just under the glyph cell, clear of the glyph drawn in it. */
    val top: Dp = RowGridMetrics.GlyphCell + RowGridMetrics.RailGap
}

@Composable
internal fun GridRow(
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
        .runRail(
            continues = rail.continues,
            // A step set in under its parent carries the parent's rail past
            // its own glyph, so the line starts at the row's top.
            fromGlyph = !rail.nested,
            color = hairlineColor,
            width = hairlinePx,
        )
        .padding(start = if (rail.nested) RowGridMetrics.NestedIndent else 0.dp)
    val gapBelow = if (rail.continues) RowGridMetrics.InRunBottom else RowGridMetrics.AfterRunBottom

    // TalkBack hears a step as one stop: its own words, read in the order
    // they are drawn (what it did, to what, its facts, note and quote), then
    // whatever it shows under them, such as a plan's text or the answers
    // given. No label stands in for those words, since a label would hide
    // the text under it. A step that opens in place says whether it is
    // open, and its tap is named for what it does.
    val clickLabel = actionDescription ?: stringResource(
        if (open) R.string.chat_row_collapse else R.string.chat_row_expand,
    )
    val stateWord = if (opens && !pushes) {
        stringResource(if (open) R.string.state_expanded else R.string.state_collapsed)
    } else {
        null
    }

    // A step that opens takes the gap under it into its target, so the
    // target reaches 48 dp while adding as little as it can to the run's
    // rhythm, and ends where the next step's begins.
    val interactiveModifier = if (opens) {
        Modifier
            .fillMaxWidth()
            .heightIn(min = ThumbTarget.MinSize)
            .clip(RoundedCornerShape(Space.s))
            .clickable(role = Role.Button, onClickLabel = clickLabel, onClick = onClick)
            .semantics(mergeDescendants = true) {
                if (stateWord != null) stateDescription = stateWord
            }
            .padding(bottom = gapBelow)
    } else {
        Modifier
            .fillMaxWidth()
            .semantics(mergeDescendants = true) {}
            .padding(bottom = gapBelow)
    }

    Box(modifier = modifier.then(railModifier).testTag(ChatRowTags.kind(kindTag))) {
        Row(
            modifier = interactiveModifier,
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
                    modifier = Modifier.size(IconSize.Small),
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
                            modifier = Modifier.size(IconSize.Small),
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
