package sh.amux.features

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
import androidx.compose.material.icons.outlined.Repeat
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
import sh.amux.values.EditLine
import sh.amux.values.LineKind
import sh.amux.words.ChatWords

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
            imageVector = Icons.Outlined.Repeat,
            contentDescription = null,
            tint = colors.inkMuted,
            modifier = Modifier.size(IconSize.Inline),
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

@Composable
internal fun EditDiffPreview(
    lines: List<EditLine>,
    showWhole: Boolean,
) {
    val colors = AmuxTheme.colors
    val shown = if (showWhole) lines.take(DIFF_WHOLE_CAP) else lines.take(DIFF_PREVIEW_LIMIT)
    val addedTint = colors.added.copy(alpha = 0.14f)
    val removedTint = colors.removed.copy(alpha = 0.14f)
    val ellipsis = stringResource(R.string.ask_diff_hunk_ellipsis)

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
            .background(colors.sunken)
            .horizontalScroll(rememberScrollState())
            .padding(vertical = Space.xs),
    ) {
        shown.forEach { editLine ->
            when (editLine) {
                is EditLine.File -> {
                    Text(
                        text = ChatWords.fileHeader(editLine.value),
                        style = AmuxTheme.typography.monoSmall,
                        color = colors.inkMuted,
                        modifier = Modifier.padding(horizontal = Space.s, vertical = Space.xxs),
                    )
                }
                is EditLine.Hunk -> {
                    val label = if (editLine.newStart > 0) {
                        "$ellipsis ${stringResource(R.string.ask_diff_line_start, editLine.newStart)}"
                    } else {
                        ellipsis
                    }
                    Text(
                        text = label,
                        style = AmuxTheme.typography.monoSmall,
                        color = colors.inkFaint,
                        modifier = Modifier.padding(horizontal = Space.s, vertical = Space.xxs),
                    )
                }
                is EditLine.Line -> {
                    val patch = editLine.value
                    val wash = when (patch.kind) {
                        LineKind.Added -> addedTint
                        LineKind.Removed -> removedTint
                        LineKind.Context -> Color.Transparent
                    }
                    val prefix = when (patch.kind) {
                        LineKind.Added -> "+"
                        LineKind.Removed -> "\u2212"
                        LineKind.Context -> " "
                    }
                    val prefixColor = when (patch.kind) {
                        LineKind.Added -> colors.added
                        LineKind.Removed -> colors.removed
                        LineKind.Context -> colors.inkFaint
                    }
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .background(wash)
                            .padding(horizontal = Space.s, vertical = 1.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        if (patch.number != null) {
                            Text(
                                text = patch.number.toString(),
                                style = AmuxTheme.typography.monoSmall,
                                color = colors.inkFaint,
                                textAlign = TextAlign.End,
                                modifier = Modifier
                                    .width(32.dp)
                                    .padding(end = Space.s),
                            )
                        }
                        Text(
                            text = prefix,
                            style = AmuxTheme.typography.monoSmall,
                            color = prefixColor,
                            modifier = Modifier.width(12.dp),
                        )
                        Text(
                            text = patch.text,
                            style = AmuxTheme.typography.monoSmall,
                            color = colors.ink,
                        )
                    }
                }
            }
        }
    }
}
