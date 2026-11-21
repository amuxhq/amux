package sh.amux.features

import sh.amux.design.IconSize
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.material.icons.automirrored.filled.FormatListBulleted
import androidx.compose.material.icons.automirrored.filled.ListAlt
import androidx.compose.material.icons.filled.FrontHand
import androidx.compose.material.icons.filled.Link
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.MoreHoriz
import androidx.compose.material.icons.filled.PriorityHigh
import androidx.compose.material.icons.filled.QuestionMark
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.unit.Constraints
import androidx.compose.foundation.clickable
import androidx.compose.ui.semantics.Role
import sh.amux.design.spillTarget
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.FormatListBulleted
import androidx.compose.material.icons.automirrored.outlined.HelpOutline
import androidx.compose.material.icons.outlined.ArrowCircleUp
import androidx.compose.material.icons.outlined.Ballot
import androidx.compose.material.icons.outlined.FrontHand
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.MoreHoriz
import androidx.compose.material.icons.outlined.PriorityHigh
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.CardState
import sh.amux.values.ChoiceOutcome
import sh.amux.values.FormField
import sh.amux.values.FormProblem
import sh.amux.values.FormValue
import sh.amux.values.QuestionResponse
import sh.amux.values.Refusal
import sh.amux.values.RefusalReason
import sh.amux.words.ChatNotices
import sh.amux.words.ChatWords


/** What the person did on an ask card. */
sealed interface AskAction {
    /** The choice at this position on the card, with the optional note it takes. */
    data class Choose(
        val index: Int,
        val note: String? = null,
        val askKey: String = "",
    ) : AskAction

    /** One response per question, in order. */
    data class Respond(
        val responses: List<QuestionResponse>,
        val askKey: String = "",
    ) : AskAction

    /** The person's own words instead of answering, with what they answered so far. */
    data class Reply(
        val text: String,
        val soFar: List<QuestionResponse>,
        val askKey: String = "",
    ) : AskAction

    /** A form's Submit, with one value per field in the form's order. */
    data class Submit(
        val values: List<FormValue>,
        val askKey: String = "",
    ) : AskAction

    /** Requests opening a validated web or email link from a Link ask. */
    data class OpenUrl(val url: String) : AskAction

    /** Interrupts the turn, dismissing the open ask while keeping the agent live. */
    data object Interrupt : AskAction

    /** Resends an answer that was not confirmed after a dropped connection. */
    data object Resend : AskAction

    /** Discards an unconfirmed answer. */
    data object Discard : AskAction
}

/**
 * The head ask docked in place of the chat composer.
 *
 * Renders every [CardState] and every [AskBody] inside an [AmuxPanel] using the
 * single attention accent for the ask header and failures, muted diff tones for
 * edit previews, and 48dp touch targets on every control.
 */
@Composable
fun AskCardView(
    card: AskCard,
    onAction: (AskAction) -> Unit,
    modifier: Modifier = Modifier,
    keptQuestionDraft: QuestionDraft? = null,
    onQuestionDraftChange: (QuestionDraft) -> Unit = {},
    keptFormValues: List<FormValue>? = null,
    onFormValuesChange: (List<FormValue>) -> Unit = {},
    validateForm: (List<FormField>, List<FormValue>) -> List<FormProblem> = ::validateFormFields,
    onInterrupt: (() -> Unit)? = null,
    onCopyCode: (String) -> Unit = {},
) {
    val colors = AmuxTheme.colors
    val triggerInterrupt = {
        if (onInterrupt != null) onInterrupt() else onAction(AskAction.Interrupt)
    }

    // Hoisted above the CardState branch so typed denial / send-back notes,
    // plan toggles and how long a grant lasts survive Sending -> Rejected for
    // the same ask key.
    var noting by remember(card.key) { mutableStateOf(false) }
    var noteText by remember(card.key) { mutableStateOf("") }
    var planAutoAccept by remember(card.key) { mutableStateOf(false) }
    var grantForSession by remember(card.key) { mutableStateOf(false) }

    AmuxPanel(
        modifier = modifier
            .fillMaxWidth()
            .testTag(AskReviewTags.ASK_CARD),
        // Docked over the transcript, the card takes the floating radius,
        // as iOS's does.
        // The card spends the accent once, on its mark; the frame stays
        // neutral in every state.
        shape = RoundedCornerShape(AmuxTheme.metrics.floatRadius),
        color = colors.raised,
        borderColor = colors.hairline,
    ) {
        if (card.state is CardState.Sending) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.l, vertical = Space.m)
                    .testTag(AskReviewTags.ASK_SENDING),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                Icon(
                    imageVector = Icons.Outlined.ArrowCircleUp,
                    contentDescription = null,
                    tint = colors.inkMuted,
                    modifier = Modifier.size(IconSize.Control),
                )
                Text(
                    text = stringResource(R.string.ask_sending, ChatWords.headline(card)),
                    style = AmuxTheme.typography.detail,
                    color = colors.ink,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
            }
        } else {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(Space.l),
                verticalArrangement = Arrangement.spacedBy(Space.m),
            ) {
                AskCardHeader(
                    card = card,
                    onInterrupt = triggerInterrupt,
                )
                AskStateBanner(
                    state = card.state,
                    onResend = { onAction(AskAction.Resend) },
                    onDiscard = { onAction(AskAction.Discard) },
                )
                if (card.state is CardState.Open || card.state is CardState.Rejected) {
                    AskCardBody(
                        card = card,
                        noting = noting,
                        onNotingChange = { noting = it },
                        noteText = noteText,
                        onNoteTextChange = { noteText = it },
                        planAutoAccept = planAutoAccept,
                        onPlanAutoAcceptChange = { planAutoAccept = it },
                        grantForSession = grantForSession,
                        onGrantForSessionChange = { grantForSession = it },
                        keptQuestionDraft = keptQuestionDraft,
                        onQuestionDraftChange = onQuestionDraftChange,
                        keptFormValues = keptFormValues,
                        onFormValuesChange = onFormValuesChange,
                        validateForm = validateForm,
                        onAction = onAction,
                        onInterrupt = triggerInterrupt,
                        onCopyCode = onCopyCode,
                        modifier = Modifier
                            .fillMaxWidth()
                            .weight(1f, fill = false),
                    )
                }
            }
        }
    }
}

@Composable
private fun AskCardHeader(
    card: AskCard,
    onInterrupt: () -> Unit,
) {
    val colors = AmuxTheme.colors
    var menuExpanded by remember(card.key) { mutableStateOf(false) }

    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
    ) {
        Box(
            modifier = Modifier
                .size(AskMarkSize)
                .clip(CircleShape)
                .background(colors.accentFill),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                imageVector = askBodyIcon(card.body),
                contentDescription = null,
                tint = colors.onAccent,
                modifier = Modifier.size(IconSize.Small),
            )
        }
        Text(
            text = ChatWords.headline(card),
            style = AmuxTheme.typography.bodyEmphasis,
            color = colors.ink,
            modifier = Modifier
                .weight(1f)
                .semantics { heading() },
        )
        val position = ChatWords.position(card)
        if (position != null) {
            Text(
                text = position,
                style = AmuxTheme.typography.monoSmall,
                color = colors.inkMuted,
                maxLines = 1,
                modifier = Modifier.testTag(AskReviewTags.ASK_POSITION),
            )
        }
        if (card.stopsTurn) {
            Box {
                val moreLabel = stringResource(R.string.action_more)
                // A 28 dp hairline circle on the card's trailing edge, its
                // 48 dp target spilling into the card's padding.
                AmuxEdgeTarget(
                    footprint = AskMoreSize,
                    onClick = { menuExpanded = true },
                    contentDescription = moreLabel,
                    modifier = Modifier.testTag(AskReviewTags.ASK_MORE),
                ) {
                    Box(
                        modifier = Modifier
                            .size(AskMoreSize)
                            .border(AmuxTheme.metrics.hairline, colors.hairline, CircleShape),
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(
                            imageVector = Icons.Filled.MoreHoriz,
                            contentDescription = null,
                            tint = colors.inkMuted,
                            modifier = Modifier.size(IconSize.Inline),
                        )
                    }
                }
                DropdownMenu(
                    expanded = menuExpanded,
                    onDismissRequest = { menuExpanded = false },
                    containerColor = colors.raised,
                ) {
                    DropdownMenuItem(
                        text = {
                            Text(
                                text = stringResource(R.string.ask_interrupt),
                                style = AmuxTheme.typography.body,
                                color = colors.removed,
                            )
                        },
                        leadingIcon = {
                            Icon(
                                imageVector = Icons.Outlined.StopCircle,
                                contentDescription = null,
                                tint = colors.removed,
                                modifier = Modifier.size(IconSize.Control),
                            )
                        },
                        onClick = {
                            menuExpanded = false
                            onInterrupt()
                        },
                        modifier = Modifier.testTag(AskReviewTags.ASK_INTERRUPT_MENU),
                    )
                }
            }
        }
    }
}

/** The accent disc an ask opens with, and the hairline circle round its ⋯. */
private val AskMarkSize = 24.dp
private val AskMoreSize = 28.dp

@Composable
private fun AskStateBanner(
    state: CardState,
    onResend: () -> Unit,
    onDiscard: () -> Unit,
) {
    val colors = AmuxTheme.colors
    when (state) {
        is CardState.Rejected -> {
            val reason = refusalMessage(state.value)
            Text(
                text = stringResource(R.string.ask_rejected, reason),
                style = AmuxTheme.typography.detail,
                color = colors.accent,
                modifier = Modifier.testTag(AskReviewTags.ASK_REJECTED),
            )
        }
        CardState.NotConfirmed -> {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(AskReviewTags.ASK_NOT_CONFIRMED),
                verticalArrangement = Arrangement.spacedBy(Space.m),
            ) {
                Text(
                    text = stringResource(R.string.ask_not_confirmed),
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
                AskButtonPair(
                    primary = {
                        AmuxActionButton(
                            label = stringResource(R.string.ask_resend),
                            kind = ActionButtonKind.Primary,
                            fillWidth = true,
                            onClick = onResend,
                            modifier = Modifier.testTag(AskReviewTags.ASK_RESEND),
                        )
                    },
                    secondary = {
                        AmuxActionButton(
                            label = stringResource(R.string.ask_discard),
                            kind = ActionButtonKind.Outline,
                            fillWidth = true,
                            onClick = onDiscard,
                            modifier = Modifier.testTag(AskReviewTags.ASK_DISCARD),
                        )
                    },
                )
            }
        }
        CardState.Dismissed -> {
            Text(
                text = stringResource(R.string.ask_dismissed),
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
                modifier = Modifier.testTag(AskReviewTags.ASK_DISMISSED),
            )
        }
        CardState.Open, CardState.Sending -> Unit
    }
}

/** Why it did not go, in the words every refusal in the chat uses. */
private fun refusalMessage(refusal: Refusal): String = ChatNotices.refusal(refusal)

private fun askBodyIcon(body: AskBody): ImageVector = when (body) {
    is AskBody.Command, is AskBody.Edit, is AskBody.Tool -> Icons.Filled.FrontHand
    is AskBody.Question -> Icons.Filled.QuestionMark
    is AskBody.Plan -> Icons.AutoMirrored.Filled.FormatListBulleted
    is AskBody.Form -> Icons.AutoMirrored.Filled.ListAlt
    is AskBody.Link -> Icons.Filled.Link
    is AskBody.Access -> Icons.Filled.Lock
    is AskBody.Unanswerable -> Icons.Filled.PriorityHigh
}

@Composable
private fun AskCardBody(
    card: AskCard,
    noting: Boolean,
    onNotingChange: (Boolean) -> Unit,
    noteText: String,
    onNoteTextChange: (String) -> Unit,
    planAutoAccept: Boolean,
    onPlanAutoAcceptChange: (Boolean) -> Unit,
    grantForSession: Boolean,
    onGrantForSessionChange: (Boolean) -> Unit,
    keptQuestionDraft: QuestionDraft?,
    onQuestionDraftChange: (QuestionDraft) -> Unit,
    keptFormValues: List<FormValue>?,
    onFormValuesChange: (List<FormValue>) -> Unit,
    validateForm: (List<FormField>, List<FormValue>) -> List<FormProblem>,
    onAction: (AskAction) -> Unit,
    onInterrupt: () -> Unit,
    onCopyCode: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    when (val body = card.body) {
        is AskBody.Question -> {
            QuestionAskSection(
                cardKey = card.key,
                questions = body.value,
                takesNote = card.questionNote,
                skips = card.questionSkip,
                replies = card.questionReply,
                keptDraft = keptQuestionDraft,
                onDraftChange = onQuestionDraftChange,
                onRespond = { onAction(AskAction.Respond(it, askKey = card.key)) },
                onReply = { text, soFar -> onAction(AskAction.Reply(text, soFar, askKey = card.key)) },
                modifier = modifier,
            )
        }
        is AskBody.Command -> {
            PermissionAskSection(
                card = card,
                noting = noting,
                onNotingChange = onNotingChange,
                note = noteText,
                onNoteChange = onNoteTextChange,
                onAction = onAction,
                modifier = modifier,
            ) {
                AskSubjectBox(text = body.command)
                val desc = body.description.ifEmpty { body.reason }
                val cwdLabel = if (body.cwd.isNotEmpty()) {
                    stringResource(R.string.ask_command_in_cwd, body.cwd)
                } else {
                    ""
                }
                val purpose = listOf(desc, cwdLabel).filter { it.isNotEmpty() }.joinToString(" · ")
                if (purpose.isNotEmpty()) {
                    AskWhyText(text = purpose)
                }
            }
        }
        is AskBody.Edit -> {
            var showWholeDiff by remember(card.key) { mutableStateOf(false) }
            PermissionAskSection(
                card = card,
                noting = noting,
                onNotingChange = onNotingChange,
                note = noteText,
                onNoteChange = onNoteTextChange,
                onAction = onAction,
                modifier = modifier,
            ) {
                // A path too long for its line is cut in its middle, so the
                // file's name stays. Compose cuts a middle on one line only.
                AskSubjectBox(
                    text = body.path,
                    added = body.added,
                    removed = body.removed,
                    maxLines = 1,
                    overflow = TextOverflow.MiddleEllipsis,
                )
                if (body.lines.isNotEmpty()) {
                    EditDiffPreview(
                        lines = body.lines,
                        showWhole = showWholeDiff,
                    )
                    if (body.lines.size > DIFF_PREVIEW_LIMIT) {
                        val toggleLabel = stringResource(
                            if (showWholeDiff) R.string.ask_diff_show_less else R.string.ask_diff_show_whole,
                        )
                        Row(
                            modifier = Modifier
                                .spillTarget()
                                .clickable(role = Role.Button) { showWholeDiff = !showWholeDiff }
                                .testTag(AskReviewTags.ASK_DIFF_TOGGLE),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(5.dp),
                        ) {
                            Text(
                                text = toggleLabel,
                                style = AmuxTheme.typography.bodyEmphasis,
                                color = AmuxTheme.colors.ink,
                            )
                            AmuxDisclosureChevron(tint = AmuxTheme.colors.inkMuted)
                        }
                    }
                }
                if (body.reason.isNotEmpty()) {
                    AskWhyText(text = body.reason)
                }
            }
        }
        is AskBody.Tool -> {
            PermissionAskSection(
                card = card,
                noting = noting,
                onNotingChange = onNotingChange,
                note = noteText,
                onNoteChange = onNoteTextChange,
                onAction = onAction,
                modifier = modifier,
            ) {
                if (body.arguments.isNotEmpty()) {
                    AskSubjectBox(text = body.arguments, maxLines = 8)
                }
            }
        }
        is AskBody.Plan -> {
            // The plan reads under its own heading in the chat; the card is
            // only the decision.
            PlanAskSection(
                card = card,
                autoAccept = planAutoAccept,
                onAutoAcceptChange = onPlanAutoAcceptChange,
                noting = noting,
                onNotingChange = onNotingChange,
                note = noteText,
                onNoteChange = onNoteTextChange,
                onAction = onAction,
                modifier = modifier,
            )
        }
        is AskBody.Form -> {
            FormAskSection(
                card = card,
                form = body,
                keptValues = keptFormValues,
                onValuesChange = onFormValuesChange,
                validateForm = validateForm,
                onAction = onAction,
                modifier = modifier,
            )
        }
        is AskBody.Link -> {
            LinkAskSection(
                card = card,
                link = body,
                onAction = onAction,
                modifier = modifier,
            )
        }
        is AskBody.Access -> {
            AccessAskSection(
                card = card,
                access = body,
                forSession = grantForSession,
                onForSessionChange = onGrantForSessionChange,
                onAction = onAction,
                modifier = modifier,
            )
        }
        is AskBody.Unanswerable -> {
            Column(
                modifier = modifier,
                verticalArrangement = Arrangement.spacedBy(Space.m),
            ) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f, fill = false)
                        .verticalScroll(rememberScrollState()),
                ) {
                    val reason = body.reason.ifEmpty {
                        stringResource(R.string.ask_unanswerable_default)
                    }
                    AskWhyText(text = reason)
                }
                AmuxActionButton(
                    label = stringResource(R.string.ask_interrupt),
                    kind = ActionButtonKind.Outline,
                    fillWidth = true,
                    onClick = onInterrupt,
                    modifier = Modifier.testTag(AskReviewTags.ASK_STOP),
                )
            }
        }
    }
}

@Composable
internal fun NoteStepSection(
    card: AskCard,
    denyIndices: List<Int>,
    note: String,
    onNoteChange: (String) -> Unit,
    onBack: () -> Unit,
    onChoose: (Int, String?) -> Unit,
) {
    val colors = AmuxTheme.colors
    val required = denyIndices.any { card.choices[it].outcome == ChoiceOutcome.SendBack }
    val takesNote = denyIndices.any { card.choices[it].takesNote }
    val heading = stringResource(
        if (required) R.string.ask_note_what_should_change else R.string.ask_note_why_optional,
    )

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (takesNote) {
                Text(
                    text = heading,
                    style = AmuxTheme.typography.caption,
                    color = colors.inkMuted,
                    modifier = Modifier.weight(1f),
                )
            } else {
                Spacer(modifier = Modifier.weight(1f))
            }
            TextButton(
                onClick = onBack,
                modifier = Modifier.testTag(AskReviewTags.ASK_NOTE_BACK),
            ) {
                Text(
                    text = stringResource(R.string.action_back),
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
            }
        }

        if (takesNote) {
            AmuxField(
                value = note,
                onValueChange = onNoteChange,
                placeholder = stringResource(R.string.ask_note_placeholder),
                singleLine = false,
                minLines = 2,
                maxLines = 5,
                modifier = Modifier
                    .fillMaxWidth()
                    .semantics { contentDescription = heading }
                    .testTag(AskReviewTags.ASK_NOTE),
            )
        }

        val ordered = denyIndices.sortedByDescending { card.choices[it].outcome == ChoiceOutcome.DenyAndStop }
        val trimmed = note.trim()
        AskButtonPair(
            buttons = ordered.map { index ->
                @Composable {
                    val choice = card.choices[index]
                    val isStop = choice.outcome == ChoiceOutcome.DenyAndStop
                    val enabled = !(choice.outcome == ChoiceOutcome.SendBack && trimmed.isEmpty())
                    val buttonLabel = ChatWords.choice(choice.copy(takesNote = false))
                    AmuxActionButton(
                        label = buttonLabel,
                        kind = if (isStop && ordered.size > 1) ActionButtonKind.Outline else ActionButtonKind.Primary,
                        fillWidth = true,
                        enabled = enabled,
                        onClick = {
                            val outgoing = if (choice.takesNote && trimmed.isNotEmpty()) trimmed else null
                            onChoose(index, outgoing)
                        },
                        modifier = Modifier.testTag(AskReviewTags.askChoice(index)),
                    )
                }
            },
        )
    }
}

@Composable
internal fun AskSubjectBox(
    text: String,
    modifier: Modifier = Modifier,
    added: Int? = null,
    removed: Int? = null,
    maxLines: Int = Int.MAX_VALUE,
    overflow: TextOverflow = TextOverflow.Ellipsis,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
            .background(colors.sunken)
            .padding(horizontal = Space.m, vertical = Space.s)
            .testTag(AskReviewTags.ASK_SUBJECT),
        verticalAlignment = Alignment.Top,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        SelectionContainer(modifier = Modifier.weight(1f)) {
            Text(
                text = text,
                style = AmuxTheme.typography.mono,
                color = colors.ink,
                maxLines = maxLines,
                overflow = overflow,
            )
        }
        if (added != null && removed != null) {
            DiffDeltaCounts(added = added, removed = removed)
        }
    }
}

@Composable
internal fun AskWhyText(text: String) {
    Text(
        text = text,
        style = AmuxTheme.typography.detail,
        color = AmuxTheme.colors.inkMuted,
    )
}

@Composable
internal fun AskButtonPair(
    primary: (@Composable () -> Unit)?,
    secondary: (@Composable () -> Unit)?,
) {
    AskButtonPair(buttons = listOfNotNull(primary, secondary))
}

/**
 * A card's buttons side by side, sharing the width equally, or one above the
 * other when any label would not fit whole beside the others, as at the
 * largest text sizes. A label is never cut short.
 */
@Composable
internal fun AskButtonPair(buttons: List<@Composable () -> Unit>) {
    if (buttons.isEmpty()) return
    val gap = Space.s
    Layout(
        contents = buttons,
        modifier = Modifier.fillMaxWidth(),
    ) { measurables, constraints ->
        val each = measurables.map { it.first() }
        val gapPx = gap.roundToPx()
        if (!constraints.hasBoundedWidth) {
            // No width to share, as in a horizontal scroll: each button takes
            // its own width, side by side.
            val placeables = each.map { it.measure(constraints.copy(minWidth = 0)) }
            val height = placeables.maxOf { it.height }
            val total = placeables.sumOf { it.width } + gapPx * (placeables.size - 1)
            return@Layout layout(total, height) {
                var x = 0
                placeables.forEach { placeable ->
                    placeable.place(x, (height - placeable.height) / 2)
                    x += placeable.width + gapPx
                }
            }
        }
        val width = constraints.maxWidth
        val share = (width - gapPx * (each.size - 1)) / each.size
        val sideBySide = each.all { it.maxIntrinsicWidth(constraints.maxHeight) <= share }
        if (sideBySide) {
            val placeables = each.map { it.measure(Constraints.fixedWidth(share)) }
            val height = placeables.maxOf { it.height }
            layout(width, height) {
                var x = 0
                placeables.forEach { placeable ->
                    placeable.place(x, (height - placeable.height) / 2)
                    x += share + gapPx
                }
            }
        } else {
            val placeables = each.map { it.measure(Constraints.fixedWidth(width)) }
            val height = placeables.sumOf { it.height } + gapPx * (placeables.size - 1)
            layout(width, height) {
                var y = 0
                placeables.forEach { placeable ->
                    placeable.place(0, y)
                    y += placeable.height + gapPx
                }
            }
        }
    }
}
