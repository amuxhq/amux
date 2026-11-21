package sh.amux.features

import sh.amux.design.AmuxShapes
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.HelpOutline
import androidx.compose.material.icons.outlined.Checklist
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.values.AnswerView
import sh.amux.values.AskRow
import sh.amux.values.PlanVerdict
import sh.amux.values.QuestionView
import sh.amux.values.Resolution
import sh.amux.words.ChatWords
import sh.amux.words.OpenableLink
import sh.amux.words.RailJoin

@Composable
internal fun AskRowView(
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
                                markdown = ask.body.source,
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
            val fieldsSummary = ask.fields.takeIf { it.isNotEmpty() }?.joinToString(", ")
            GridRow(
                kindTag = "form",
                glyph = Icons.Outlined.Checklist,
                accented = ask.resolution == Resolution.Open,
                rail = rail,
                verb = verb,
                subject = if (ask.resolution == Resolution.Answered) ask.server else "",
                meta = if (fieldsSummary != null) RowMeta(fieldsSummary) else RowMeta(),
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
                    shape = AmuxShapes.Pill,
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
