package sh.amux.features

import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.ChoiceOutcome
import sh.amux.words.ChatWords
import sh.amux.words.OpenableLink

@Composable
internal fun LinkAskSection(
    card: AskCard,
    link: AskBody.Link,
    onAction: (AskAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    var opened by remember(card.key) { mutableStateOf(false) }
    val openable = remember(link.url) { OpenableLink.parse(link.url) }
    val doneIndex = card.choices.indexOfFirst { it.outcome == ChoiceOutcome.OpenLink }.takeIf { it >= 0 }
    val declineIndex = card.choices.indexOfFirst { it.outcome != ChoiceOutcome.OpenLink }.takeIf { it >= 0 }

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
            if (link.message.isNotEmpty()) {
                AskWhyText(text = link.message)
            }
            AskSubjectBox(text = link.url, maxLines = 3)
            if (link.localPage) {
                AskWhyText(text = stringResource(R.string.ask_link_local_page))
            }
        }

        if (!link.localPage && openable != null && !opened) {
            AmuxActionButton(
                label = stringResource(R.string.ask_link_open),
                kind = ActionButtonKind.Primary,
                fillWidth = true,
                onClick = {
                    opened = true
                    onAction(AskAction.OpenUrl(openable.url))
                },
                modifier = Modifier.testTag(AskReviewTags.ASK_LINK_OPEN),
            )
        }

        AskButtonPair(
            primary = doneIndex?.let { idx ->
                {
                    AmuxActionButton(
                        label = ChatWords.choice(card.choices[idx]),
                        kind = if (opened) ActionButtonKind.Primary else ActionButtonKind.Outline,
                        fillWidth = true,
                        onClick = { onAction(AskAction.Choose(idx, null, askKey = card.key)) },
                        modifier = Modifier.testTag(AskReviewTags.askChoice(idx)),
                    )
                }
            },
            secondary = declineIndex?.let { idx ->
                {
                    AmuxActionButton(
                        label = ChatWords.choice(card.choices[idx]),
                        kind = ActionButtonKind.Outline,
                        fillWidth = true,
                        onClick = { onAction(AskAction.Choose(idx, null, askKey = card.key)) },
                        modifier = Modifier.testTag(AskReviewTags.askChoice(idx)),
                    )
                }
            },
        )
    }
}

@Composable
internal fun AccessAskSection(
    card: AskCard,
    access: AskBody.Access,
    forSession: Boolean,
    onForSessionChange: (Boolean) -> Unit,
    onAction: (AskAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val turnIndex = card.choices.indexOfFirst { it.outcome == ChoiceOutcome.GrantForTurn }.takeIf { it >= 0 }
    val sessionIndex = card.choices.indexOfFirst { it.outcome == ChoiceOutcome.GrantForSession }.takeIf { it >= 0 }
    val denyIndices = card.choices.indices.filter { it != turnIndex && it != sessionIndex }
    val howLong = stringResource(R.string.ask_access_how_long)

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
            if (access.reason.isNotEmpty()) {
                AskWhyText(text = access.reason)
            }
            val hasFacts = access.write.isNotEmpty() || access.read.isNotEmpty() || access.network
            if (hasFacts) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
                        .background(colors.sunken)
                        .padding(horizontal = Space.m, vertical = Space.s),
                    verticalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    access.write.forEach { path ->
                        AccessFactRow(label = stringResource(R.string.ask_access_write_to), value = path)
                    }
                    access.read.forEach { path ->
                        AccessFactRow(label = stringResource(R.string.ask_access_read), value = path)
                    }
                    if (access.network) {
                        val hostText = if (access.hosts.isEmpty()) {
                            stringResource(R.string.ask_access_any_host)
                        } else {
                            access.hosts.joinToString(", ")
                        }
                        AccessFactRow(
                            label = stringResource(R.string.ask_access_network),
                            value = hostText,
                        )
                    }
                }
            }
        }

        // How long a grant lasts, when the agent offers both, then one
        // Grant for the length chosen beside the ways to refuse.
        if (turnIndex != null && sessionIndex != null) {
            AmuxSegmentedChoice(
                options = listOf(false, true),
                selected = forSession,
                label = { session ->
                    stringResource(if (session) R.string.ask_access_this_session else R.string.ask_access_this_turn)
                },
                onSelect = onForSessionChange,
                tag = { session -> if (session) AskReviewTags.ASK_GRANT_SESSION else AskReviewTags.ASK_GRANT_TURN },
                modifier = Modifier.semantics {
                    contentDescription = howLong
                },
            )
        }
        val grantIndex = (if (forSession) sessionIndex else turnIndex) ?: turnIndex ?: sessionIndex
        AskButtonPair(
            buttons = buildList {
                if (grantIndex != null) {
                    add {
                        AmuxActionButton(
                            label = stringResource(R.string.ask_access_grant),
                            kind = ActionButtonKind.Primary,
                            fillWidth = true,
                            onClick = { onAction(AskAction.Choose(grantIndex, null, askKey = card.key)) },
                            modifier = Modifier.testTag(AskReviewTags.ASK_GRANT),
                        )
                    }
                }
                denyIndices.forEach { idx ->
                    add {
                        AmuxActionButton(
                            label = ChatWords.choice(card.choices[idx]),
                            kind = ActionButtonKind.Outline,
                            fillWidth = true,
                            onClick = { onAction(AskAction.Choose(idx, null, askKey = card.key)) },
                            modifier = Modifier.testTag(AskReviewTags.askChoice(idx)),
                        )
                    }
                }
            },
        )
    }
}

@Composable
private fun AccessFactRow(
    label: String,
    value: String,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Text(
            text = label,
            style = AmuxTheme.typography.body,
            color = colors.ink,
        )
        Spacer(modifier = Modifier.weight(1f))
        Text(
            text = value,
            style = AmuxTheme.typography.monoSmall,
            color = colors.inkMuted,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
            textAlign = TextAlign.End,
        )
    }
}
