package sh.amux.features

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.key
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.ChoiceOutcome
import sh.amux.words.ChatWords

@Composable
internal fun PlanAskSection(
    card: AskCard,
    plan: AskBody.Plan,
    autoAccept: Boolean,
    onAutoAcceptChange: (Boolean) -> Unit,
    noting: Boolean,
    onNotingChange: (Boolean) -> Unit,
    note: String,
    onNoteChange: (String) -> Unit,
    onAction: (AskAction) -> Unit,
    onCopyCode: (String) -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    BackHandler(enabled = noting) {
        onNotingChange(false)
    }

    val approveIndices = card.choices.indices.filter { card.choices[it].outcome is ChoiceOutcome.ApprovePlan }
    val switchedIndex = approveIndices.firstOrNull {
        val outcome = card.choices[it].outcome as? ChoiceOutcome.ApprovePlan
        outcome != null && outcome.mode.isNotEmpty()
    }
    val plainIndex = approveIndices.firstOrNull {
        val outcome = card.choices[it].outcome as? ChoiceOutcome.ApprovePlan
        outcome != null && outcome.mode.isEmpty()
    }
    val otherIndices = card.choices.indices.filter { it !in approveIndices }

    Column(
        modifier = modifier,
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        val planTitle = plan.title
        if (!planTitle.isNullOrBlank() || plan.body.isNotBlank()) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .weight(1f, fill = false)
                    .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
                    .background(colors.sunken)
                    .heightIn(max = 220.dp)
                    .verticalScroll(rememberScrollState())
                    .padding(Space.m)
                    .testTag(AskReviewTags.ASK_PLAN_BODY),
                verticalArrangement = Arrangement.spacedBy(Space.xs),
            ) {
                if (plan.body.isNotBlank()) {
                    MarkdownView(
                        markdown = plan.body,
                        title = planTitle,
                        onOpenLink = { link -> onAction(AskAction.OpenUrl(link.url)) },
                        onCopyCode = onCopyCode,
                    )
                } else if (!planTitle.isNullOrBlank()) {
                    Text(
                        text = planTitle,
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.ink,
                    )
                }
            }
        }

        if (noting) {
            NoteStepSection(
                card = card,
                denyIndices = otherIndices,
                note = note,
                onNoteChange = onNoteChange,
                onBack = { onNotingChange(false) },
                onChoose = { index, text -> onAction(AskAction.Choose(index, text, askKey = card.key)) },
            )
        } else {
            if (switchedIndex != null && plainIndex != null) {
                val switchedChoice = card.choices[switchedIndex]
                val switchTitle = ChatWords.planSwitch(switchedChoice)
                AmuxSwitchRow(
                    checked = autoAccept,
                    onCheckedChange = onAutoAcceptChange,
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
                        .background(colors.sunken)
                        .testTag(AskReviewTags.ASK_PLAN_SWITCH),
                    contentPadding = PaddingValues(horizontal = Space.m, vertical = Space.s),
                    colors = amuxSwitchColors(),
                    spacing = Space.s,
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(
                            text = switchTitle,
                            style = AmuxTheme.typography.body,
                            color = colors.ink,
                        )
                        Text(
                            text = stringResource(R.string.ask_plan_as_work_starts),
                            style = AmuxTheme.typography.caption,
                            color = colors.inkMuted,
                        )
                    }
                }
            }

            val activeApproveIndex = (if (autoAccept) switchedIndex else plainIndex) ?: approveIndices.firstOrNull()
            val approveLabel = if (activeApproveIndex != null) {
                val outcome = card.choices[activeApproveIndex].outcome as? ChoiceOutcome.ApprovePlan
                if (outcome != null && outcome.mode.isNotEmpty()) {
                    stringResource(
                        R.string.ask_plan_approve_and_switch,
                        outcome.modeName.ifEmpty { outcome.mode },
                    )
                } else {
                    stringResource(R.string.ask_plan_approve)
                }
            } else {
                null
            }

            val firstOther = otherIndices.firstOrNull()
            AskButtonPair(
                primary = if (activeApproveIndex != null && approveLabel != null) {
                    {
                        AmuxActionButton(
                            label = approveLabel,
                            kind = ActionButtonKind.Primary,
                            fillWidth = true,
                            onClick = { onAction(AskAction.Choose(activeApproveIndex, null, askKey = card.key)) },
                            modifier = Modifier.testTag(AskReviewTags.ASK_PLAN_APPROVE),
                        )
                    }
                } else {
                    null
                },
                secondary = firstOther?.let { idx ->
                    {
                        val choice = card.choices[idx]
                        AmuxActionButton(
                            label = ChatWords.choice(choice),
                            kind = ActionButtonKind.Outline,
                            fillWidth = true,
                            onClick = {
                                if (choice.takesNote) {
                                    onNotingChange(true)
                                } else {
                                    onAction(AskAction.Choose(idx, null, askKey = card.key))
                                }
                            },
                            modifier = Modifier.testTag(AskReviewTags.askChoice(idx)),
                        )
                    }
                },
            )
        }
    }
}
