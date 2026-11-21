package sh.amux.features

import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.BoxWithConstraints
import sh.amux.design.ThumbTarget
import androidx.compose.foundation.layout.heightIn
import sh.amux.design.IconSize
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Repeat
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.key
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.values.AskCard
import sh.amux.values.ChoiceOutcome
import sh.amux.values.FilePatch
import sh.amux.values.FileRow
import sh.amux.values.PatchLine
import sh.amux.values.LineKind
import sh.amux.words.ChatWords
import androidx.compose.runtime.remember
import androidx.compose.ui.text.AnnotatedString

@Composable
internal fun PermissionAskSection(
    card: AskCard,
    noting: Boolean,
    onNotingChange: (Boolean) -> Unit,
    note: String,
    onNoteChange: (String) -> Unit,
    onAction: (AskAction) -> Unit,
    modifier: Modifier = Modifier,
    subject: @Composable () -> Unit,
) {
    BackHandler(enabled = noting) {
        onNotingChange(false)
    }

    val primaryIndex = card.choices.indexOfFirst { it.primary }.takeIf { it >= 0 }
    val denyIndices = card.choices.indices.filter { isDenyOutcome(card.choices[it].outcome) }
    val scopeIndices = card.choices.indices.filter { it != primaryIndex && it !in denyIndices }

    Column(
        modifier = modifier,
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .weight(1f, fill = false)
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            subject()

            if (!noting && scopeIndices.isNotEmpty()) {
                Column(
                    modifier = Modifier.fillMaxWidth(),
                    verticalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    scopeIndices.forEach { index ->
                        val row = ChatWords.scopeRow(card.choices[index])
                        AskScopeChoiceRow(
                            title = row.title,
                            detail = row.detail,
                            onClick = { onAction(AskAction.Choose(index, null, askKey = card.key)) },
                            modifier = Modifier.testTag(AskReviewTags.askChoice(index)),
                        )
                    }
                }
            }
        }

        if (noting) {
            NoteStepSection(
                card = card,
                denyIndices = denyIndices,
                note = note,
                onNoteChange = onNoteChange,
                onBack = { onNotingChange(false) },
                onChoose = { index, text -> onAction(AskAction.Choose(index, text, askKey = card.key)) },
            )
        } else {
            val firstDeny = denyIndices.firstOrNull()
            AskButtonPair(
                primary = primaryIndex?.let { idx ->
                    {
                        AmuxActionButton(
                            label = ChatWords.button(card.choices[idx]),
                            kind = ActionButtonKind.Primary,
                            fillWidth = true,
                            onClick = { onAction(AskAction.Choose(idx, null, askKey = card.key)) },
                            modifier = Modifier.testTag(AskReviewTags.askChoice(idx)),
                        )
                    }
                },
                secondary = firstDeny?.let { idx ->
                    if (denyIndices.size == 1 && !card.choices[idx].takesNote) {
                        {
                            AmuxActionButton(
                                label = ChatWords.button(card.choices[idx]),
                                kind = ActionButtonKind.Outline,
                                fillWidth = true,
                                onClick = { onAction(AskAction.Choose(idx, null, askKey = card.key)) },
                                modifier = Modifier.testTag(AskReviewTags.askChoice(idx)),
                            )
                        }
                    } else {
                        {
                            AmuxActionButton(
                                label = stringResource(R.string.ask_deny_ellipsis),
                                kind = ActionButtonKind.Outline,
                                fillWidth = true,
                                onClick = { onNotingChange(true) },
                                modifier = Modifier.testTag(AskReviewTags.ASK_DENY),
                            )
                        }
                    }
                },
            )
        }
    }
}

private fun isDenyOutcome(outcome: ChoiceOutcome): Boolean = when (outcome) {
    ChoiceOutcome.Deny,
    ChoiceOutcome.DenyAndStop,
    ChoiceOutcome.Decline,
    ChoiceOutcome.SendBack,
    -> true
    else -> false
}

@Composable
private fun AskScopeChoiceRow(
    title: String,
    detail: String?,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
            .border(AmuxTheme.metrics.hairline, colors.hairline, RoundedCornerShape(AmuxTheme.metrics.controlRadius))
            .clickable(role = Role.Button, onClick = onClick)
            .heightIn(min = ThumbTarget.MinSize)
            .padding(horizontal = Space.m, vertical = Space.s),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Icon(
            imageVector = Icons.Rounded.Repeat,
            contentDescription = null,
            tint = colors.inkMuted,
            modifier = Modifier.size(IconSize.Row),
        )
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(Space.xxs),
        ) {
            Text(
                text = title,
                style = AmuxTheme.typography.body,
                color = colors.ink,
            )
            if (!detail.isNullOrEmpty()) {
                Text(
                    text = detail,
                    style = AmuxTheme.typography.monoSmall,
                    color = colors.inkMuted,
                )
            }
        }
        AmuxDisclosureChevron()
    }
}

internal const val DIFF_PREVIEW_LIMIT: Int = 12

private const val DIFF_WHOLE_CAP: Int = 400

/** One line of an edit's preview: where a file starts, where a hunk starts, or a patch line. */
internal sealed interface EditLine {
    data class File(val value: FileRow) : EditLine
    data class Hunk(val newStart: Int) : EditLine
    /** A patch line, and where it is: its file's place in the patch, and its place among that file's lines. */
    data class Line(val value: PatchLine, val file: Int = 0, val index: Int = 0) : EditLine
}

/**
 * An edit's patch as the preview's lines: each file's header in a patch of
 * several, each hunk's start where the provider said it, then its lines.
 */
internal fun editLines(patch: List<FilePatch>): List<EditLine> = patch.flatMapIndexed { fileIndex, file ->
    var index = 0
    listOfNotNull(file.file?.let { EditLine.File(it) }) +
        file.hunks.flatMap { hunk ->
            listOfNotNull(hunk.start?.let { EditLine.Hunk(it.new) }) +
                hunk.lines.map { EditLine.Line(it, file = fileIndex, index = index++) }
        }
}

/**
 * A short diff inline, green and red, with the whole of it one tap away.
 * Where each hunk starts reads as its first line in the new file.
 *
 * Every row is as wide as the widest line, or the card when that is wider,
 * so the added and removed washes run edge to edge rather than stopping
 * where each line's text ends; long lines scroll sideways together.
 */
@Composable
internal fun EditDiffPreview(
    lines: List<EditLine>,
    showWhole: Boolean,
    /** The patch [lines] were read from, whose code is coloured file by file. */
    patch: List<FilePatch> = emptyList(),
    /** The file an edit of one file is in, which its patch does not name. */
    path: String = "",
) {
    val colors = AmuxTheme.colors
    // Every file's part is asked for, shown or folded away, so showing the
    // whole diff asks for nothing new.
    val coloured = patch.mapIndexed { index, file ->
        key(index) {
            rememberHighlighted(CodeHighlighting.Request.Patch(path = file.file?.path ?: path, hunks = file.hunks))
        }
    }
    val runs = remember(patch, coloured) {
        patch.mapIndexed { index, file ->
            coloured[index].runsFor(file.hunks.flatMap { hunk -> hunk.lines.map { it.text } })
        }
    }
    val shown = if (showWhole) lines.take(DIFF_WHOLE_CAP) else lines.take(DIFF_PREVIEW_LIMIT)
    val addedTint = colors.added.copy(alpha = 0.18f)
    val removedTint = colors.removed.copy(alpha = 0.18f)
    val shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius)

    BoxWithConstraints(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(colors.sunken),
    ) {
        val viewport = maxWidth
        Column(
            modifier = Modifier
                .horizontalScroll(rememberScrollState())
                .padding(vertical = 6.dp),
        ) {
            Column(
                modifier = Modifier
                    .width(IntrinsicSize.Max)
                    .widthIn(min = viewport),
            ) {
                shown.forEach { editLine ->
                    val (text, ink, wash) = when (editLine) {
                        is EditLine.File -> Triple(
                            AnnotatedString(ChatWords.fileHeader(editLine.value)),
                            colors.inkMuted,
                            Color.Transparent,
                        )
                        is EditLine.Hunk -> Triple(
                            AnnotatedString(stringResource(R.string.ask_diff_line_start, editLine.newStart)),
                            colors.inkFaint,
                            Color.Transparent,
                        )
                        is EditLine.Line -> {
                            val line = editLine.value
                            val code = runs.getOrNull(editLine.file)?.getOrNull(editLine.index)
                                ?.let { colors.codeText(it) }
                                ?: AnnotatedString(line.text)
                            when (line.kind) {
                                LineKind.Added -> Triple(AnnotatedString("+") + code, colors.ink, addedTint)
                                LineKind.Removed -> Triple(AnnotatedString("-") + code, colors.ink, removedTint)
                                LineKind.Context -> Triple(AnnotatedString(" ") + code, colors.ink, Color.Transparent)
                            }
                        }
                    }
                    Text(
                        text = text,
                        style = AmuxTheme.typography.monoSmall,
                        color = ink,
                        maxLines = 1,
                        softWrap = false,
                        modifier = Modifier
                            .fillMaxWidth()
                            .background(wash)
                            .padding(horizontal = Space.s),
                    )
                }
            }
        }
    }
}
