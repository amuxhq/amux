package sh.amux.features

import androidx.compose.foundation.layout.height
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import sh.amux.design.backdrop
import sh.amux.design.ThumbTarget
import androidx.compose.foundation.layout.heightIn
import sh.amux.design.IconSize
import sh.amux.design.AmuxShapes
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material3.Checkbox
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.InlineRun
import sh.amux.values.OptionView
import sh.amux.values.QuestionResponse
import sh.amux.values.QuestionView

/** Progress kept across navigation for a multi-step question ask. */
@Immutable
data class QuestionDraft(
    val step: Int = 0,
    val picks: List<Set<Int>> = emptyList(),
    val others: List<String?> = emptyList(),
    val highlighted: List<Int?> = emptyList(),
    val skipped: List<Boolean> = emptyList(),
    val notes: List<String> = emptyList(),
    val noting: List<Boolean> = emptyList(),
    val reviewing: Boolean = false,
    val replying: Boolean = false,
    val replyText: String = "",
) {
    fun fits(count: Int): Boolean =
        picks.size == count &&
            others.size == count &&
            highlighted.size == count &&
            skipped.size == count &&
            notes.size == count &&
            noting.size == count
}

@Composable
internal fun QuestionAskSection(
    cardKey: String,
    questions: List<QuestionView>,
    takesNote: Boolean,
    skips: Boolean,
    replies: Boolean,
    keptDraft: QuestionDraft?,
    onDraftChange: (QuestionDraft) -> Unit,
    onRespond: (List<QuestionResponse>) -> Unit,
    onReply: (String, List<QuestionResponse>) -> Unit,
    modifier: Modifier = Modifier,
) {
    val count = questions.size
    val initialDraft = remember(cardKey, count) {
        buildInitialQuestionDraft(count = count, kept = keptDraft)
    }
    var step by remember(cardKey) { mutableIntStateOf(initialDraft.step) }
    var picks by remember(cardKey) { mutableStateOf(initialDraft.picks) }
    var others by remember(cardKey) { mutableStateOf(initialDraft.others) }
    var highlighted by remember(cardKey) { mutableStateOf(initialDraft.highlighted) }
    var skipped by remember(cardKey) { mutableStateOf(initialDraft.skipped) }
    var notes by remember(cardKey) { mutableStateOf(initialDraft.notes) }
    var noting by remember(cardKey) { mutableStateOf(initialDraft.noting) }
    var reviewing by remember(cardKey) { mutableStateOf(initialDraft.reviewing) }
    var replying by remember(cardKey) { mutableStateOf(initialDraft.replying) }
    var replyText by remember(cardKey) { mutableStateOf(initialDraft.replyText) }

    BackHandler(enabled = replying || reviewing || step > 0) {
        when {
            replying -> replying = false
            reviewing -> {
                reviewing = false
                step = (count - 1).coerceAtLeast(0)
            }
            step > 0 -> step -= 1
        }
    }

    LaunchedEffect(step, picks, others, highlighted, skipped, notes, noting, reviewing, replying, replyText) {
        onDraftChange(
            QuestionDraft(
                step = step,
                picks = picks,
                others = others,
                highlighted = highlighted,
                skipped = skipped,
                notes = notes,
                noting = noting,
                reviewing = reviewing,
                replying = replying,
                replyText = replyText,
            ),
        )
    }

    fun answered(index: Int): Boolean {
        if (index !in picks.indices) return false
        return picks[index].isNotEmpty() || others[index]?.isNotBlank() == true
    }

    fun decided(index: Int): Boolean =
        answered(index) || (index in skipped.indices && skipped[index])

    fun buildResponses(
        currentPicks: List<Set<Int>> = picks,
        currentOthers: List<String?> = others,
        currentNotes: List<String> = notes,
    ): List<QuestionResponse> = questions.indices.map { index ->
        val note = currentNotes.getOrElse(index) { "" }.trim()
        val other = currentOthers.getOrNull(index)?.takeIf { it.isNotBlank() }
        val pickedList = if (other != null && !questions[index].multiSelect) {
            emptyList()
        } else {
            currentPicks.getOrElse(index) { emptySet() }.sorted()
        }
        QuestionResponse(
            note = note.ifEmpty { null },
            other = other,
            picked = pickedList,
        )
    }

    val hasPreviews = remember(questions) {
        questions.any { q -> q.options.any { it.preview.isNotEmpty() } }
    }
    val tapAnswers = count == 1 && !questions[0].multiSelect && !hasPreviews && noting.firstOrNull() != true

    fun advanceFrom(index: Int) {
        if (!answered(index)) return
        when {
            count == 1 -> onRespond(buildResponses())
            index + 1 < count -> step = index + 1
            else -> reviewing = true
        }
    }

    fun skipAt(index: Int) {
        if (index !in skipped.indices) return
        val nextPicks = picks.toMutableList().apply { this[index] = emptySet() }
        val nextOthers = others.toMutableList().apply { this[index] = null }
        val nextSkipped = skipped.toMutableList().apply { this[index] = true }
        picks = nextPicks
        others = nextOthers
        skipped = nextSkipped
        when {
            count == 1 -> onRespond(buildResponses(currentPicks = nextPicks, currentOthers = nextOthers))
            index + 1 < count -> step = index + 1
            else -> reviewing = true
        }
    }

    val colors = AmuxTheme.colors
    Column(
        modifier = modifier,
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        if (replying) {
            val replyTitle = stringResource(R.string.ask_question_reply_title)
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = replyTitle,
                    style = AmuxTheme.typography.caption,
                    color = colors.inkMuted,
                    modifier = Modifier.weight(1f),
                )
                TextButton(
                    onClick = { replying = false },
                    modifier = Modifier.testTag(AskReviewTags.ASK_REPLY_BACK),
                ) {
                    Text(
                        text = stringResource(R.string.action_back),
                        style = AmuxTheme.typography.detail,
                        color = colors.inkMuted,
                    )
                }
            }
            AmuxField(
                value = replyText,
                onValueChange = { replyText = it },
                singleLine = false,
                minLines = 2,
                maxLines = 5,
                modifier = Modifier
                    .fillMaxWidth()
                    .semantics { contentDescription = replyTitle }
                    .testTag(AskReviewTags.ASK_REPLY_INPUT),
            )
            AmuxActionButton(
                label = stringResource(R.string.ask_question_reply_send),
                kind = ActionButtonKind.Primary,
                fillWidth = true,
                enabled = replyText.trim().isNotEmpty(),
                onClick = { onReply(replyText, buildResponses()) },
                modifier = Modifier.testTag(AskReviewTags.ASK_REPLY_SEND),
            )
        } else {
            if (count > 1) {
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(Space.xs),
                ) {
                    questions.forEachIndexed { index, q ->
                        val title = q.header.ifEmpty {
                            stringResource(R.string.ask_question_step_number, index + 1)
                        }
                        QuestionStepChip(
                            title = title,
                            current = !reviewing && step == index,
                            done = decided(index),
                            onClick = {
                                reviewing = false
                                step = index
                            },
                            modifier = Modifier.testTag(AskReviewTags.askStep(index)),
                        )
                    }
                    QuestionStepChip(
                        title = stringResource(R.string.ask_question_review_step),
                        current = reviewing,
                        done = false,
                        onClick = { reviewing = true },
                        modifier = Modifier.testTag(AskReviewTags.ASK_STEP_REVIEW),
                    )
                }
            }

            if (reviewing) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f, fill = false)
                        .verticalScroll(rememberScrollState()),
                    verticalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    questions.forEachIndexed { index, q ->
                        val summary = questionSummaryText(
                            question = q,
                            picked = picks.getOrElse(index) { emptySet() },
                            other = others.getOrNull(index),
                            isSkipped = skipped.getOrElse(index) { false },
                        )
                        val heading = if (q.header.isEmpty()) {
                            q.questionRuns.plainText()
                        } else {
                            "${q.header} · ${q.questionRuns.plainText()}"
                        }
                        val noteText = notes.getOrElse(index) { "" }.trim()
                        Column(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
                                .background(colors.sunken)
                                .clickable(role = Role.Button) {
                                    reviewing = false
                                    step = index
                                }
                                .heightIn(min = ThumbTarget.MinSize)
                                .padding(horizontal = Space.m, vertical = Space.s)
                                // Whether this step is done, as the other
                                // clients report it; the summary says it aloud.
                                .semantics {
                                    reportedValue = when {
                                        answered(index) -> "answered"
                                        decided(index) -> "skipped"
                                        else -> "open"
                                    }
                                }
                                .testTag(AskReviewTags.askReview(index)),
                            verticalArrangement = Arrangement.spacedBy(Space.xxs),
                        ) {
                            Text(
                                text = heading,
                                style = AmuxTheme.typography.detail,
                                color = colors.inkMuted,
                                maxLines = 2,
                                overflow = TextOverflow.Ellipsis,
                            )
                            Text(
                                text = summary,
                                style = AmuxTheme.typography.bodyEmphasis,
                                color = if (decided(index)) colors.ink else colors.accent,
                            )
                            if (noteText.isNotEmpty()) {
                                Text(
                                    text = stringResource(R.string.ask_question_note_prefix, noteText),
                                    style = AmuxTheme.typography.caption,
                                    color = colors.inkMuted,
                                    maxLines = 2,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                        }
                    }
                }

                AmuxActionButton(
                    label = stringResource(R.string.ask_question_send_answers),
                    kind = ActionButtonKind.Primary,
                    fillWidth = true,
                    enabled = questions.indices.all(::decided),
                    onClick = { onRespond(buildResponses()) },
                    modifier = Modifier.testTag(AskReviewTags.ASK_SEND),
                )
            } else if (step in questions.indices) {
                val index = step
                val currentQuestion = questions[index]
                val bodyScrollState = key(cardKey, index) { rememberScrollState() }

                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f, fill = false)
                        .verticalScroll(bodyScrollState),
                    verticalArrangement = Arrangement.spacedBy(Space.m),
                ) {
                    Text(
                        text = currentQuestion.questionRuns.plainText(),
                        style = AmuxTheme.typography.body,
                        color = colors.ink,
                        modifier = Modifier.testTag(AskReviewTags.ASK_QUESTION),
                    )

                    val previewOptionIndex = run {
                        val hi = highlighted.getOrNull(index)
                        if (hi != null && hi in currentQuestion.options.indices && currentQuestion.options[hi].preview.isNotEmpty()) {
                            hi
                        } else {
                            currentQuestion.options.indexOfFirst { it.preview.isNotEmpty() }.takeIf { it >= 0 }
                        }
                    }
                    if (previewOptionIndex != null) {
                        val option = currentQuestion.options[previewOptionIndex]
                        Column(
                            modifier = Modifier
                                .fillMaxWidth()
                                .testTag(AskReviewTags.ASK_PREVIEW),
                            verticalArrangement = Arrangement.spacedBy(Space.xs),
                        ) {
                            Text(
                                text = option.labelRuns.plainText(),
                                style = AmuxTheme.typography.caption,
                                color = colors.inkMuted,
                            )
                            AskSubjectBox(text = option.preview, maxLines = 8)
                        }
                    }

                    Column(
                        modifier = Modifier.fillMaxWidth(),
                        verticalArrangement = Arrangement.spacedBy(Space.s),
                    ) {
                        currentQuestion.options.forEachIndexed { position, option ->
                            val isSelected = position in picks.getOrElse(index) { emptySet() }
                            QuestionOptionRow(
                                option = option,
                                multi = currentQuestion.multiSelect,
                                isSelected = isSelected,
                                onClick = {
                                    val nextSkipped = skipped.toMutableList().apply { this[index] = false }
                                    val nextHighlighted = highlighted.toMutableList().apply { this[index] = position }
                                    skipped = nextSkipped
                                    highlighted = nextHighlighted
                                    if (currentQuestion.multiSelect) {
                                        val nextPicks = picks.toMutableList()
                                        val currentSet = nextPicks[index]
                                        nextPicks[index] = if (position in currentSet) currentSet - position else currentSet + position
                                        picks = nextPicks
                                    } else {
                                        val nextOthers = others.toMutableList().apply { this[index] = null }
                                        val nextPicks = picks.toMutableList().apply { this[index] = setOf(position) }
                                        others = nextOthers
                                        picks = nextPicks
                                        if (tapAnswers) {
                                            onRespond(buildResponses(currentPicks = nextPicks, currentOthers = nextOthers))
                                        } else if (!hasPreviews && !noting.getOrElse(index) { false }) {
                                            if (count == 1) {
                                                onRespond(buildResponses(currentPicks = nextPicks, currentOthers = nextOthers))
                                            } else if (index + 1 < count) {
                                                step = index + 1
                                            } else {
                                                reviewing = true
                                            }
                                        }
                                    }
                                },
                                modifier = Modifier.testTag(AskReviewTags.askOption(position)),
                            )
                        }

                        if (currentQuestion.allowOther) {
                            val otherValue = others.getOrNull(index)
                            if (otherValue != null) {
                                val otherLabel = stringResource(R.string.ask_question_something_else)
                                AmuxField(
                                    value = otherValue,
                                    onValueChange = { typed ->
                                        val nextOthers = others.toMutableList().apply { this[index] = typed }
                                        val nextSkipped = skipped.toMutableList().apply { this[index] = false }
                                        others = nextOthers
                                        skipped = nextSkipped
                                        if (!currentQuestion.multiSelect) {
                                            picks = picks.toMutableList().apply { this[index] = emptySet() }
                                        }
                                    },
                                    placeholder = otherLabel,
                                    visualTransformation = if (currentQuestion.secret) {
                                        PasswordVisualTransformation()
                                    } else {
                                        VisualTransformation.None
                                    },
                                    modifier = Modifier
                                        .fillMaxWidth()
                                        .semantics { contentDescription = otherLabel }
                                        .testTag(AskReviewTags.ASK_OTHER_INPUT),
                                )
                            } else {
                                QuestionOptionRow(
                                    option = stringResource(R.string.ask_question_something_else_ellipsis).let { label ->
                                        // The app's own words, which hold no markup.
                                        OptionView(
                                            label = label,
                                            description = "",
                                            preview = "",
                                            recommended = false,
                                            labelRuns = listOf(
                                                InlineRun(
                                                    text = label,
                                                    strong = false,
                                                    emphasis = false,
                                                    strikethrough = false,
                                                    code = false,
                                                ),
                                            ),
                                            descriptionRuns = emptyList(),
                                        )
                                    },
                                    multi = currentQuestion.multiSelect,
                                    isSelected = false,
                                    quiet = true,
                                    onClick = {
                                        others = others.toMutableList().apply { this[index] = "" }
                                        if (!currentQuestion.multiSelect) {
                                            picks = picks.toMutableList().apply { this[index] = emptySet() }
                                        }
                                    },
                                    modifier = Modifier.testTag(AskReviewTags.ASK_OPTION_OTHER),
                                )
                            }
                        }
                    }

                    if (takesNote) {
                        if (noting.getOrElse(index) { false }) {
                            val notePlaceholder = stringResource(R.string.ask_question_note_placeholder)
                            AmuxField(
                                value = notes.getOrElse(index) { "" },
                                onValueChange = { typed ->
                                    notes = notes.toMutableList().apply { this[index] = typed }
                                },
                                placeholder = notePlaceholder,
                                singleLine = false,
                                minLines = 1,
                                maxLines = 4,
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .semantics { contentDescription = notePlaceholder }
                                    .testTag(AskReviewTags.askQuestionNote(index)),
                            )
                        } else {
                            TextButton(
                                onClick = {
                                    noting = noting.toMutableList().apply { this[index] = true }
                                },
                                // Clear of the buttons below, so the two
                                // targets don't overlap.
                                modifier = Modifier
                                    .padding(bottom = Space.s)
                                    .testTag(AskReviewTags.askAddNote(index)),
                                contentPadding = PaddingValues(horizontal = Space.xs, vertical = Space.xs),
                            ) {
                                Text(
                                    text = stringResource(R.string.ask_question_add_note),
                                    style = AmuxTheme.typography.bodyEmphasis,
                                    color = colors.ink,
                                )
                            }
                        }
                    }
                }

                val isTypingOther = others.getOrNull(index) != null
                if (!tapAnswers || isTypingOther || skips) {
                    val selectedCount = picks.getOrElse(index) { emptySet() }.size
                    val singleQuestion = count == 1
                    val nextLabel = when {
                        currentQuestion.multiSelect && selectedCount > 0 -> stringResource(
                            if (singleQuestion) R.string.ask_question_send_selected else R.string.ask_question_next_selected,
                            selectedCount,
                        )
                        singleQuestion -> stringResource(R.string.ask_question_send)
                        else -> stringResource(R.string.ask_question_next)
                    }

                    AskButtonPair(
                        primary = if (!tapAnswers || isTypingOther) {
                            {
                                AmuxActionButton(
                                    label = nextLabel,
                                    kind = ActionButtonKind.Primary,
                                    fillWidth = true,
                                    enabled = answered(index),
                                    onClick = { advanceFrom(index) },
                                    modifier = Modifier.testTag(AskReviewTags.ASK_NEXT),
                                )
                            }
                        } else {
                            null
                        },
                        secondary = if (skips) {
                            {
                                AmuxActionButton(
                                    label = stringResource(R.string.ask_question_skip),
                                    kind = ActionButtonKind.Outline,
                                    fillWidth = true,
                                    onClick = { skipAt(index) },
                                    modifier = Modifier.testTag(AskReviewTags.ASK_SKIP),
                                )
                            }
                        } else {
                            null
                        },
                    )
                }
            }

            if (replies) {
                TextButton(
                    onClick = { replying = true },
                    modifier = Modifier.testTag(AskReviewTags.ASK_REPLY_LINK),
                    contentPadding = PaddingValues(horizontal = Space.xs, vertical = Space.xs),
                ) {
                    Text(
                        text = stringResource(R.string.ask_question_reply_instead),
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.ink,
                    )
                }
            }
        }
    }
}

private fun buildInitialQuestionDraft(
    count: Int,
    kept: QuestionDraft?,
): QuestionDraft {
    if (kept != null && kept.fits(count)) return kept
    return QuestionDraft(
        step = 0,
        picks = List(count) { emptySet() },
        others = List(count) { null },
        highlighted = List(count) { null },
        skipped = List(count) { false },
        notes = List(count) { "" },
        noting = List(count) { false },
        reviewing = false,
        replying = false,
        replyText = "",
    )
}

@Composable
private fun questionSummaryText(
    question: QuestionView,
    picked: Set<Int>,
    other: String?,
    isSkipped: Boolean,
): String {
    val parts = picked.sorted().mapNotNull { pos ->
        question.options.getOrNull(pos)?.let { it.labelRuns.plainText() }
    }.toMutableList()
    if (!other.isNullOrBlank()) {
        parts += if (question.secret) {
            stringResource(R.string.ask_question_answered_hidden)
        } else {
            "\u201C$other\u201D"
        }
    }
    return when {
        parts.isNotEmpty() -> parts.joinToString(", ")
        isSkipped -> stringResource(R.string.ask_question_skipped)
        else -> stringResource(R.string.ask_question_not_answered)
    }
}

@Composable
private fun QuestionStepChip(
    title: String,
    current: Boolean,
    done: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    // The step being answered is filled with the interactive tint, like a
    // chosen chip, rather than an ink slab that outshouts the question.
    val container = if (current) colors.accentFill else AmuxTheme.backdrop
    val content = when {
        current -> colors.onAccent
        done -> colors.ink
        else -> colors.inkMuted
    }
    Surface(
        modifier = modifier
            .thumbTarget()
            .clip(AmuxShapes.Pill)
            .clickable(role = Role.Tab, onClick = onClick)
            .semantics { selected = current },
        shape = AmuxShapes.Pill,
        color = container,
        contentColor = content,
        border = if (current) null else BorderStroke(AmuxTheme.metrics.hairline, colors.hairline),
    ) {
        Row(
            modifier = Modifier.padding(horizontal = Space.m, vertical = Space.s),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.xs),
        ) {
            if (done && !current) {
                Icon(
                    imageVector = Icons.Outlined.Check,
                    contentDescription = null,
                    tint = content,
                    modifier = Modifier.size(IconSize.Small),
                )
            }
            Text(
                text = title,
                style = AmuxTheme.typography.caption,
                color = content,
                maxLines = 1,
            )
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun QuestionOptionRow(
    option: OptionView,
    multi: Boolean,
    isSelected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    quiet: Boolean = false,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val indicatorShape = RoundedCornerShape(if (multi) 5.dp else 9.dp)
    val rowShape = RoundedCornerShape(AmuxTheme.metrics.controlRadius)
    val labelPlain = option.labelRuns.plainText()
    val detailPlain = if (option.description != option.label) {
        option.descriptionRuns.plainText()
    } else {
        ""
    }
    val labelStyle = if (quiet) typography.body else typography.bodyEmphasis
    // The row is the one control, so TalkBack reads its words and whether it
    // is chosen as one stop.
    val choose = if (multi) {
        Modifier.toggleable(value = isSelected, role = Role.Checkbox, onValueChange = { onClick() })
    } else {
        Modifier.selectable(selected = isSelected, role = Role.RadioButton, onClick = onClick)
    }

    Row(
        modifier = modifier
            .fillMaxWidth()
            .clip(rowShape)
            .background(colors.sunken)
            .then(
                if (isSelected) Modifier.border(1.5.dp, colors.ink, rowShape) else Modifier,
            )
            .then(choose)
            // Picked or not, read the same for a radio and a checkbox; the
            // option by its own words, without its detail.
            .semantics {
                reportedLabel = labelPlain
                if (isSelected) reportedValue = "selected"
            }
            .heightIn(min = ThumbTarget.MinSize)
            .padding(horizontal = Space.m + Space.xxs, vertical = 11.dp),
        verticalAlignment = Alignment.Top,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        // The box sits on the label's first line, not the middle of a
        // label and detail that run to several lines.
        val firstLine = with(LocalDensity.current) { labelStyle.lineHeight.toDp() }
        Box(
            modifier = Modifier.height(firstLine),
            contentAlignment = Alignment.Center,
        ) {
            Box(
                modifier = Modifier
                    .size(18.dp)
                    .clip(indicatorShape)
                    .background(if (isSelected) colors.ink else Color.Transparent)
                    .border(
                        width = 1.5.dp,
                        color = if (isSelected) colors.ink else colors.inkFaint,
                        shape = indicatorShape,
                    ),
                contentAlignment = Alignment.Center,
            ) {
                if (isSelected) {
                    Icon(
                        imageVector = Icons.Outlined.Check,
                        contentDescription = null,
                        tint = colors.raised,
                        modifier = Modifier.size(12.dp),
                    )
                }
            }
        }
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(Space.xxs),
        ) {
            // The tag beside the label while both fit, under it when they
            // do not; neither is broken mid-word.
            FlowRow(
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalArrangement = Arrangement.spacedBy(Space.xs),
                itemVerticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = labelPlain,
                    style = labelStyle,
                    color = if (quiet) colors.inkMuted else colors.ink,
                )
                if (option.recommended) {
                    Text(
                        text = stringResource(R.string.ask_question_recommended).uppercase(),
                        style = typography.sectionTitle,
                        color = colors.inkMuted,
                        maxLines = 1,
                        modifier = Modifier
                            .border(AmuxTheme.metrics.hairline, colors.hairline, RoundedCornerShape(5.dp))
                            .padding(horizontal = 5.dp, vertical = Space.xxs),
                    )
                }
            }
            if (detailPlain.isNotEmpty()) {
                Text(
                    text = detailPlain,
                    style = typography.detail,
                    color = colors.inkMuted,
                )
            }
        }
    }
}
