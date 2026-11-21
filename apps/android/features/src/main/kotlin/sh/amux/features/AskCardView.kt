package sh.amux.features

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
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.FormatListBulleted
import androidx.compose.material.icons.automirrored.outlined.HelpOutline
import androidx.compose.material.icons.outlined.ArrowCircleUp
import androidx.compose.material.icons.outlined.Ballot
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.FrontHand
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.MoreHoriz
import androidx.compose.material.icons.outlined.PriorityHigh
import androidx.compose.material.icons.outlined.Repeat
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CheckboxDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.CardState
import sh.amux.values.Choice
import sh.amux.values.ChoiceOutcome
import sh.amux.values.EditLine
import sh.amux.values.FieldProblem
import sh.amux.values.FormField
import sh.amux.values.FormFieldKind
import sh.amux.values.FormProblem
import sh.amux.values.FormValue
import sh.amux.values.LineKind
import sh.amux.values.OptionView
import sh.amux.values.QuestionResponse
import sh.amux.values.QuestionView
import sh.amux.values.Refusal
import sh.amux.values.RefusalReason
import sh.amux.words.ChatWords
import sh.amux.words.MarkdownDocument
import sh.amux.words.OpenableLink
import kotlin.math.floor

/**
 * Semantic `Modifier.testTag(...)` constants for Ask, Overview, Chat Settings,
 * and Review composables.
 */
object AskReviewTags {
    // Ask card
    const val ASK_CARD: String = "ask"
    const val ASK_SENDING: String = "ask.sending"
    const val ASK_REJECTED: String = "ask.rejected"
    const val ASK_NOT_CONFIRMED: String = "ask.not-confirmed"
    const val ASK_DISMISSED: String = "ask.dismissed"
    const val ASK_POSITION: String = "ask.position"
    const val ASK_MORE: String = "ask.more"
    const val ASK_INTERRUPT_MENU: String = "ask.interrupt.menu"
    const val ASK_STOP: String = "ask.stop"
    const val ASK_RESEND: String = "ask.resend"
    const val ASK_DISCARD: String = "ask.discard"
    const val ASK_SUBJECT: String = "ask.subject"
    const val ASK_DIFF_TOGGLE: String = "ask.diff"
    const val ASK_DENY: String = "ask.deny"
    const val ASK_NOTE: String = "ask.note"
    const val ASK_NOTE_BACK: String = "ask.note.back"
    const val ASK_PLAN_BODY: String = "ask.plan.body"
    const val ASK_PLAN_SWITCH: String = "ask.plan.auto"
    const val ASK_PLAN_APPROVE: String = "ask.approve"
    const val ASK_SUBMIT: String = "ask.submit"
    const val ASK_LINK_OPEN: String = "ask.open"
    const val ASK_GRANT_TURN: String = "ask.grant.turn"
    const val ASK_GRANT_SESSION: String = "ask.grant.session"
    const val ASK_QUESTION: String = "ask.question"
    const val ASK_PREVIEW: String = "ask.preview"
    const val ASK_OPTION_OTHER: String = "ask.option.other"
    const val ASK_OTHER_INPUT: String = "ask.other"
    const val ASK_SKIP: String = "ask.skip"
    const val ASK_NEXT: String = "ask.next"
    const val ASK_SEND: String = "ask.send"
    const val ASK_STEP_REVIEW: String = "ask.step.review"
    const val ASK_REPLY_LINK: String = "ask.reply"
    const val ASK_REPLY_BACK: String = "ask.reply.back"
    const val ASK_REPLY_INPUT: String = "ask.reply.text"
    const val ASK_REPLY_SEND: String = "ask.reply.send"

    fun askChoice(index: Int): String = "ask.choice.$index"
    fun askStep(index: Int): String = "ask.step.$index"
    fun askOption(position: Int): String = "ask.option.$position"
    fun askAddNote(index: Int): String = "ask.addNote.$index"
    fun askQuestionNote(index: Int): String = "ask.note.$index"
    fun askReview(index: Int): String = "ask.review.$index"
    fun askField(name: String): String = "ask.field.$name"
    fun askFieldProblem(name: String): String = "ask.field.$name.problem"
    fun askFieldChoice(name: String, optionIndex: Int): String = "ask.field.$name.option.$optionIndex"

    // Chat overview
    const val OVERVIEW_SHEET: String = "chat.overview"
    const val OVERVIEW_CLOSE: String = "chat.overview.close"
    const val OVERVIEW_PLACE: String = "chat.place"
    const val OVERVIEW_TASKS: String = "chat.overview.tasks"
    const val OVERVIEW_STOP_DIALOG: String = "chat.overview.stop.dialog"
    const val OVERVIEW_STOP_CONFIRM: String = "chat.overview.stop"
    const val OVERVIEW_STOP_CANCEL: String = "chat.overview.stop-cancel"
    const val OVERVIEW_USAGE: String = "chat.overview.usage"
    const val OVERVIEW_CONTEXT: String = "chat.overview.context"
    const val OVERVIEW_REVIEW: String = "chat.overview.review"
    const val OVERVIEW_CHANGES_LOADING: String = "chat.overview.changes.loading"
    const val OVERVIEW_CHANGES_ERROR: String = "chat.overview.changes.error"
    const val OVERVIEW_CHANGES_RETRY: String = "chat.overview.changes.retry"
    const val OVERVIEW_CHANGES_NONE: String = "chat.overview.changes.none"
    const val OVERVIEW_STRIP: String = "chat.overview.strip"
    const val OVERVIEW_STRIP_CHANGES: String = "chat.overview.strip.changes"

    fun overviewTask(index: Int): String = "chat.overview.task.$index"
    fun overviewJob(index: Int): String = "chat.overview.job.$index"
    fun overviewJobStop(index: Int): String = "chat.overview.job.$index.stop"
    fun overviewServer(name: String): String = "chat.overview.server.$name"
    fun overviewUsageWindow(index: Int): String = "chat.overview.usage.$index"
    fun overviewComparison(name: String): String = "chat.overview.comparison.$name"
    fun overviewFile(path: String): String = "chat.overview.file.$path"

    // Chat settings
    const val CHAT_SETTINGS_SHEET: String = "chat.settings"
    const val CHAT_SETTINGS_CLOSE: String = "chat.settings.close"
    const val CHAT_SETTINGS_TYPING_HINT: String = "chat.settings.typing"
    const val CHAT_SETTINGS_CYCLE: String = "chat.settings.cycle"
    const val CHAT_SETTINGS_CYCLES_HINT: String = "chat.settings.mode.cycles"
    const val CHAT_SETTINGS_CONTEXT: String = "chat.settings.context"

    fun chatSettingsModel(value: String): String = "chat.settings.model.$value"
    fun chatSettingsEffort(value: String): String = "chat.settings.effort.$value"
    fun chatSettingsPermission(value: String): String = "chat.settings.mode.$value"
    fun chatSettingsMode(value: String): String = "chat.settings.workmode.$value"
    fun chatSettingsCommand(name: String): String = "chat.settings.command.$name"

    // Review page
    const val REVIEW_SCREEN: String = "review"
    const val REVIEW_BACK: String = "review.back"
    const val REVIEW_SEND: String = "review.send"
    const val REVIEW_LOADING: String = "review.loading"
    const val REVIEW_SWITCHING: String = "review.switching"
    const val REVIEW_ERROR: String = "review.error"
    const val REVIEW_RETRY: String = "review.retry"
    const val REVIEW_EMPTY: String = "review.empty"
    const val REVIEW_FILE_COMMENT: String = "review.file.comment"
    const val REVIEW_COMMENT_SHEET: String = "review.comment.sheet"
    const val REVIEW_COMMENT_INPUT: String = "review.note"
    const val REVIEW_COMMENT_SAVE: String = "review.save"
    const val REVIEW_COMMENT_ADD: String = "review.add"
    const val REVIEW_COMMENT_DELETE: String = "review.delete"
    const val REVIEW_COMMENT_CANCEL: String = "review.cancel"

    fun reviewComparison(name: String): String = "review.comparison.$name"
    fun reviewFileChip(index: Int): String = "review.file.$index"
    fun reviewLine(fileIndex: Int, hunkIndex: Int, lineIndex: Int): String =
        "review.line.$fileIndex.$hunkIndex.$lineIndex"
    fun reviewCommentThread(index: Int): String = "review.comment.thread.$index"
}

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

/**
 * Pure Kotlin schema validation matching `ui_view::form_problems` so [AskCardView]
 * validates forms offline in tests and storybook and can also be overridden by
 * a bridge-backed validator.
 */
fun validateFormFields(
    fields: List<FormField>,
    values: List<FormValue>,
): List<FormProblem> {
    val problems = mutableListOf<FormProblem>()
    fields.forEachIndexed { index, field ->
        val value = values.getOrNull(index) ?: field.initial
        val problem = validateSingleField(field, value)
        if (problem != null) {
            problems += FormProblem(field = index, problem = problem)
        }
    }
    return problems
}

private fun validateSingleField(field: FormField, value: FormValue): FieldProblem? =
    when (val kind = field.kind) {
        is FormFieldKind.Text -> {
            val trimmed = (value as? FormValue.Text)?.value?.trim().orEmpty()
            val minLen = kind.minLength
            val maxLen = kind.maxLength
            when {
                trimmed.isEmpty() -> if (field.required) FieldProblem.Required else null
                minLen != null && trimmed.length < minLen -> FieldProblem.TooShort(minLen)
                maxLen != null && trimmed.length > maxLen -> FieldProblem.TooLong(maxLen)
                else -> null
            }
        }
        is FormFieldKind.Number -> {
            val trimmed = (value as? FormValue.Text)?.value?.trim().orEmpty()
            val minVal = kind.minimum
            val maxVal = kind.maximum
            if (trimmed.isEmpty()) {
                if (field.required) FieldProblem.Required else null
            } else {
                val parsed = trimmed.toDoubleOrNull()
                when {
                    parsed == null || !parsed.isFinite() -> FieldProblem.NotANumber
                    kind.integer && floor(parsed) != parsed -> FieldProblem.NotWholeNumber
                    minVal != null && parsed < minVal -> FieldProblem.BelowMinimum(minVal)
                    maxVal != null && parsed > maxVal -> FieldProblem.AboveMaximum(maxVal)
                    else -> null
                }
            }
        }
        FormFieldKind.Toggle -> null
        is FormFieldKind.Choice -> {
            val chosen = (value as? FormValue.Choice)?.value
            if (chosen == null || chosen !in kind.options.indices) {
                if (field.required) FieldProblem.Required else null
            } else {
                null
            }
        }
        is FormFieldKind.Many -> {
            val picked = (value as? FormValue.Many)?.value.orEmpty()
                .filter { it in kind.options.indices }
                .distinct()
            val minItems = kind.minItems
            val maxItems = kind.maxItems
            when {
                picked.isEmpty() -> if (field.required) FieldProblem.Required else null
                minItems != null && picked.size < minItems -> FieldProblem.TooFew(minItems)
                maxItems != null && picked.size > maxItems -> FieldProblem.TooMany(maxItems)
                else -> null
            }
        }
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
) {
    val colors = AmuxTheme.colors
    val triggerInterrupt = {
        if (onInterrupt != null) onInterrupt() else onAction(AskAction.Interrupt)
    }

    // Hoisted above the CardState branch so typed denial / send-back notes and
    // plan toggles survive Sending -> Rejected for the same ask key.
    var noting by remember(card.key) { mutableStateOf(false) }
    var noteText by remember(card.key) { mutableStateOf("") }
    var planAutoAccept by remember(card.key) { mutableStateOf(false) }

    AmuxPanel(
        modifier = modifier
            .fillMaxWidth()
            .testTag(AskReviewTags.ASK_CARD),
        shape = RoundedCornerShape(AmuxTheme.metrics.cardRadius),
        color = colors.raised,
        borderColor = if (card.state is CardState.Open || card.state is CardState.Rejected) {
            colors.accent.copy(alpha = 0.45f)
        } else {
            colors.hairline
        },
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
                    modifier = Modifier.size(18.dp),
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
                        keptQuestionDraft = keptQuestionDraft,
                        onQuestionDraftChange = onQuestionDraftChange,
                        keptFormValues = keptFormValues,
                        onFormValuesChange = onFormValuesChange,
                        validateForm = validateForm,
                        onAction = onAction,
                        onInterrupt = triggerInterrupt,
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
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Box(
            modifier = Modifier
                .size(28.dp)
                .clip(CircleShape)
                .background(colors.accent),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                imageVector = askBodyIcon(card.body),
                contentDescription = null,
                tint = colors.onAccent,
                modifier = Modifier.size(16.dp),
            )
        }
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(Space.xxs),
        ) {
            if (card.state is CardState.Open) {
                Text(
                    text = stringResource(R.string.ask_needs_answer),
                    style = AmuxTheme.typography.badge,
                    color = colors.accent,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Text(
                text = ChatWords.headline(card),
                style = AmuxTheme.typography.bodyEmphasis,
                color = colors.ink,
            )
        }
        val position = ChatWords.position(card)
        if (position != null) {
            AmuxStatusChip(
                text = position,
                modifier = Modifier.testTag(AskReviewTags.ASK_POSITION),
            )
        }
        if (card.stopsTurn) {
            Box {
                val moreLabel = stringResource(R.string.action_more_options)
                IconButton(
                    onClick = { menuExpanded = true },
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(AskReviewTags.ASK_MORE),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.MoreHoriz,
                        contentDescription = moreLabel,
                        tint = colors.inkMuted,
                        modifier = Modifier.size(20.dp),
                    )
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
                                modifier = Modifier.size(18.dp),
                            )
                        },
                        onClick = {
                            menuExpanded = false
                            onInterrupt()
                        },
                        modifier = Modifier
                            .thumbTarget()
                            .testTag(AskReviewTags.ASK_INTERRUPT_MENU),
                    )
                }
            }
        }
    }
}

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

@Composable
private fun refusalMessage(refusal: Refusal): String {
    val detail = refusal.detail?.trim()
    if (!detail.isNullOrEmpty()) return detail
    return when (refusal.reason) {
        RefusalReason.Exited -> stringResource(R.string.ask_refusal_exited)
        RefusalReason.Exiting -> stringResource(R.string.ask_refusal_exiting)
        RefusalReason.AgentClosed -> stringResource(R.string.ask_refusal_agent_closed)
        RefusalReason.Draining -> stringResource(R.string.ask_refusal_draining)
        RefusalReason.HostUnreachable -> stringResource(R.string.ask_refusal_host_unreachable)
        RefusalReason.Unsupported -> stringResource(R.string.ask_refusal_unsupported)
        RefusalReason.ClosedAsk -> stringResource(R.string.ask_refusal_closed_ask)
        RefusalReason.NotQueued -> stringResource(R.string.ask_refusal_not_queued)
        RefusalReason.ConversationElsewhere -> stringResource(R.string.ask_refusal_conversation_elsewhere)
        RefusalReason.TooLong -> stringResource(R.string.ask_refusal_too_long)
        RefusalReason.Unspecified -> stringResource(R.string.ask_refusal_unspecified)
    }
}

private fun askBodyIcon(body: AskBody): ImageVector = when (body) {
    is AskBody.Command, is AskBody.Edit, is AskBody.Tool -> Icons.Outlined.FrontHand
    is AskBody.Question -> Icons.AutoMirrored.Outlined.HelpOutline
    is AskBody.Plan -> Icons.AutoMirrored.Outlined.FormatListBulleted
    is AskBody.Form -> Icons.Outlined.Ballot
    is AskBody.Link -> Icons.Outlined.Link
    is AskBody.Access -> Icons.Outlined.Lock
    is AskBody.Unanswerable -> Icons.Outlined.PriorityHigh
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
    keptQuestionDraft: QuestionDraft?,
    onQuestionDraftChange: (QuestionDraft) -> Unit,
    keptFormValues: List<FormValue>?,
    onFormValuesChange: (List<FormValue>) -> Unit,
    validateForm: (List<FormField>, List<FormValue>) -> List<FormProblem>,
    onAction: (AskAction) -> Unit,
    onInterrupt: () -> Unit,
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
                AskSubjectBox(
                    text = body.path,
                    added = body.added,
                    removed = body.removed,
                    maxLines = 2,
                )
                if (body.lines.isNotEmpty()) {
                    EditDiffPreview(
                        lines = body.lines,
                        showWhole = showWholeDiff,
                    )
                    if (body.lines.size > DIFF_PREVIEW_LIMIT) {
                        TextButton(
                            onClick = { showWholeDiff = !showWholeDiff },
                            modifier = Modifier
                                .thumbTarget()
                                .testTag(AskReviewTags.ASK_DIFF_TOGGLE),
                            contentPadding = PaddingValues(horizontal = Space.xs, vertical = Space.xs),
                        ) {
                            Text(
                                text = stringResource(
                                    if (showWholeDiff) R.string.ask_diff_show_less else R.string.ask_diff_show_whole,
                                ),
                                style = AmuxTheme.typography.bodyEmphasis,
                                color = AmuxTheme.colors.ink,
                            )
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
            PlanAskSection(
                card = card,
                plan = body,
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

private fun isDenyOutcome(outcome: ChoiceOutcome): Boolean = when (outcome) {
    ChoiceOutcome.Deny,
    ChoiceOutcome.DenyAndStop,
    ChoiceOutcome.Decline,
    ChoiceOutcome.SendBack,
    -> true
    else -> false
}

@Composable
private fun PermissionAskSection(
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

@Composable
private fun NoteStepSection(
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
                modifier = Modifier
                    .thumbTarget()
                    .testTag(AskReviewTags.ASK_NOTE_BACK),
            ) {
                Text(
                    text = stringResource(R.string.action_back),
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
            }
        }

        if (takesNote) {
            OutlinedTextField(
                value = note,
                onValueChange = onNoteChange,
                placeholder = {
                    Text(
                        text = stringResource(R.string.ask_note_placeholder),
                        style = AmuxTheme.typography.body,
                        color = colors.inkFaint,
                    )
                },
                textStyle = AmuxTheme.typography.body.copy(color = colors.ink),
                minLines = 2,
                maxLines = 5,
                shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                colors = amuxTextFieldColors(),
                modifier = Modifier
                    .fillMaxWidth()
                    .semantics { contentDescription = heading }
                    .testTag(AskReviewTags.ASK_NOTE),
            )
        }

        val ordered = denyIndices.sortedByDescending { card.choices[it].outcome == ChoiceOutcome.DenyAndStop }
        val trimmed = note.trim()
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            ordered.forEach { index ->
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
                    modifier = Modifier
                        .weight(1f)
                        .testTag(AskReviewTags.askChoice(index)),
                )
            }
        }
    }
}

@Composable
private fun PlanAskSection(
    card: AskCard,
    plan: AskBody.Plan,
    autoAccept: Boolean,
    onAutoAcceptChange: (Boolean) -> Unit,
    noting: Boolean,
    onNotingChange: (Boolean) -> Unit,
    note: String,
    onNoteChange: (String) -> Unit,
    onAction: (AskAction) -> Unit,
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
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
                        .background(colors.sunken)
                        .clickable(role = Role.Switch) { onAutoAcceptChange(!autoAccept) }
                        .padding(horizontal = Space.m, vertical = Space.s)
                        .thumbTarget()
                        .testTag(AskReviewTags.ASK_PLAN_SWITCH),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
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
                    Switch(
                        checked = autoAccept,
                        onCheckedChange = onAutoAcceptChange,
                        colors = amuxSwitchColors(),
                    )
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

@Composable
private fun FormAskSection(
    card: AskCard,
    form: AskBody.Form,
    keptValues: List<FormValue>?,
    onValuesChange: (List<FormValue>) -> Unit,
    validateForm: (List<FormField>, List<FormValue>) -> List<FormProblem>,
    onAction: (AskAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val initialValues = remember(card.key, form.fields) {
        if (keptValues != null && keptValues.size == form.fields.size) {
            keptValues
        } else {
            form.fields.map { it.initial }
        }
    }
    var values by remember(card.key) { mutableStateOf(initialValues) }
    var touched by remember(card.key) { mutableStateOf(emptySet<Int>()) }

    val problems = validateForm(form.fields, values)
    val problemsByField = problems.associateBy { it.field }

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
            if (form.message.isNotEmpty()) {
                AskWhyText(text = form.message)
            }
            form.fields.forEachIndexed { index, field ->
                val currentValue = values.getOrElse(index) { field.initial }
                val rawProblem = problemsByField[index]?.problem
                val shownProblem = when {
                    rawProblem == null -> null
                    rawProblem != FieldProblem.Required -> rawProblem
                    index in touched -> rawProblem
                    else -> null
                }
                FormFieldControl(
                    field = field,
                    value = currentValue,
                    problem = shownProblem,
                    onValueChange = { updated ->
                        val next = values.toMutableList()
                        while (next.size <= index) next.add(form.fields[next.size].initial)
                        next[index] = updated
                        values = next
                        touched = touched + index
                        onValuesChange(next)
                    },
                )
            }
        }

        val submitIndex = card.choices.indexOfFirst { it.outcome == ChoiceOutcome.Submit }.takeIf { it >= 0 }
        val declineIndex = card.choices.indexOfFirst { it.outcome != ChoiceOutcome.Submit }.takeIf { it >= 0 }

        AskButtonPair(
            primary = submitIndex?.let {
                {
                    AmuxActionButton(
                        label = stringResource(R.string.ask_form_submit),
                        kind = ActionButtonKind.Primary,
                        fillWidth = true,
                        enabled = problems.isEmpty(),
                        onClick = { onAction(AskAction.Submit(values, askKey = card.key)) },
                        modifier = Modifier.testTag(AskReviewTags.ASK_SUBMIT),
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
private fun FormFieldControl(
    field: FormField,
    value: FormValue,
    problem: FieldProblem?,
    onValueChange: (FormValue) -> Unit,
) {
    val colors = AmuxTheme.colors
    val fieldLabel = if (field.required) {
        stringResource(R.string.ask_form_required_label, field.title)
    } else {
        field.title
    }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        when (val kind = field.kind) {
            is FormFieldKind.Text, is FormFieldKind.Number -> {
                Text(
                    text = fieldLabel,
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
                if (field.description.isNotEmpty()) {
                    Text(
                        text = field.description,
                        style = AmuxTheme.typography.caption,
                        color = colors.inkFaint,
                    )
                }
                val text = (value as? FormValue.Text)?.value.orEmpty()
                OutlinedTextField(
                    value = text,
                    onValueChange = { onValueChange(FormValue.Text(it)) },
                    placeholder = {
                        Text(
                            text = field.title,
                            style = AmuxTheme.typography.body,
                            color = colors.inkFaint,
                        )
                    },
                    textStyle = AmuxTheme.typography.body.copy(color = colors.ink),
                    singleLine = true,
                    isError = problem != null,
                    keyboardOptions = KeyboardOptions(
                        keyboardType = if (kind is FormFieldKind.Number && (kind.minimum ?: -1.0) >= 0.0) {
                            KeyboardType.Decimal
                        } else {
                            KeyboardType.Text
                        },
                    ),
                    shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                    colors = amuxTextFieldColors(),
                    modifier = Modifier
                        .fillMaxWidth()
                        .semantics { contentDescription = fieldLabel }
                        .testTag(AskReviewTags.askField(field.name)),
                )
            }
            FormFieldKind.Toggle -> {
                val checked = (value as? FormValue.Toggle)?.value ?: false
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
                        .clickable(role = Role.Switch) { onValueChange(FormValue.Toggle(!checked)) }
                        .padding(vertical = Space.xs)
                        .thumbTarget()
                        .testTag(AskReviewTags.askField(field.name)),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(
                            text = field.title,
                            style = AmuxTheme.typography.body,
                            color = colors.ink,
                        )
                        if (field.description.isNotEmpty()) {
                            Text(
                                text = field.description,
                                style = AmuxTheme.typography.caption,
                                color = colors.inkMuted,
                            )
                        }
                    }
                    Switch(
                        checked = checked,
                        onCheckedChange = { onValueChange(FormValue.Toggle(it)) },
                        colors = amuxSwitchColors(),
                    )
                }
            }
            is FormFieldKind.Choice -> {
                var expanded by remember(field.name) { mutableStateOf(false) }
                val chosenIndex = (value as? FormValue.Choice)?.value
                val chosenLabel = chosenIndex?.let { kind.options.getOrNull(it) }
                    ?: stringResource(R.string.ask_form_choose)

                Text(
                    text = fieldLabel,
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
                if (field.description.isNotEmpty()) {
                    Text(
                        text = field.description,
                        style = AmuxTheme.typography.caption,
                        color = colors.inkFaint,
                    )
                }
                Box(modifier = Modifier.fillMaxWidth()) {
                    Surface(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
                            .clickable(role = Role.DropdownList) { expanded = true }
                            .thumbTarget()
                            .testTag(AskReviewTags.askField(field.name)),
                        shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                        color = colors.sunken,
                        border = BorderStroke(AmuxTheme.metrics.hairline, colors.hairline),
                    ) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(horizontal = Space.m, vertical = Space.m),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(
                                text = chosenLabel,
                                style = AmuxTheme.typography.body,
                                color = if (chosenIndex != null) colors.ink else colors.inkMuted,
                                modifier = Modifier.weight(1f),
                            )
                            Icon(
                                imageVector = Icons.Outlined.KeyboardArrowDown,
                                contentDescription = null,
                                tint = colors.inkMuted,
                                modifier = Modifier.size(18.dp),
                            )
                        }
                    }
                    DropdownMenu(
                        expanded = expanded,
                        onDismissRequest = { expanded = false },
                        containerColor = colors.raised,
                    ) {
                        kind.options.forEachIndexed { optionIndex, option ->
                            DropdownMenuItem(
                                text = {
                                    Text(
                                        text = option,
                                        style = AmuxTheme.typography.body,
                                        color = colors.ink,
                                    )
                                },
                                onClick = {
                                    expanded = false
                                    onValueChange(FormValue.Choice(optionIndex))
                                },
                                modifier = Modifier
                                    .thumbTarget()
                                    .testTag(AskReviewTags.askFieldChoice(field.name, optionIndex)),
                            )
                        }
                    }
                }
            }
            is FormFieldKind.Many -> {
                val picked = (value as? FormValue.Many)?.value.orEmpty().toSet()
                Text(
                    text = fieldLabel,
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
                if (field.description.isNotEmpty()) {
                    Text(
                        text = field.description,
                        style = AmuxTheme.typography.caption,
                        color = colors.inkFaint,
                    )
                }
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag(AskReviewTags.askField(field.name)),
                    verticalArrangement = Arrangement.spacedBy(Space.xs),
                ) {
                    kind.options.forEachIndexed { optionIndex, option ->
                        val isChecked = optionIndex in picked
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
                                .clickable(role = Role.Checkbox) {
                                    val next = if (isChecked) picked - optionIndex else picked + optionIndex
                                    onValueChange(FormValue.Many(next.sorted()))
                                }
                                .thumbTarget()
                                .padding(horizontal = Space.xs)
                                .testTag(AskReviewTags.askFieldChoice(field.name, optionIndex)),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(Space.s),
                        ) {
                            Checkbox(
                                checked = isChecked,
                                onCheckedChange = { checked ->
                                    val next = if (checked) picked + optionIndex else picked - optionIndex
                                    onValueChange(FormValue.Many(next.sorted()))
                                },
                                colors = CheckboxDefaults.colors(
                                    checkedColor = colors.ink,
                                    uncheckedColor = colors.inkMuted,
                                    checkmarkColor = colors.ground,
                                ),
                            )
                            Text(
                                text = option,
                                style = AmuxTheme.typography.body,
                                color = colors.ink,
                            )
                        }
                    }
                }
            }
        }

        if (problem != null) {
            Text(
                text = ChatWords.fieldProblem(problem),
                style = AmuxTheme.typography.detail,
                color = colors.accent,
                modifier = Modifier.testTag(AskReviewTags.askFieldProblem(field.name)),
            )
        }
    }
}

@Composable
private fun LinkAskSection(
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
private fun AccessAskSection(
    card: AskCard,
    access: AskBody.Access,
    onAction: (AskAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val turnIndex = card.choices.indexOfFirst { it.outcome == ChoiceOutcome.GrantForTurn }.takeIf { it >= 0 }
    val sessionIndex = card.choices.indexOfFirst { it.outcome == ChoiceOutcome.GrantForSession }.takeIf { it >= 0 }
    val denyIndices = card.choices.indices.filter { it != turnIndex && it != sessionIndex }

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

        Column(
            modifier = Modifier.fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            if (turnIndex != null) {
                AmuxActionButton(
                    label = stringResource(R.string.ask_access_grant_turn),
                    kind = ActionButtonKind.Primary,
                    fillWidth = true,
                    onClick = { onAction(AskAction.Choose(turnIndex, null, askKey = card.key)) },
                    modifier = Modifier.testTag(AskReviewTags.ASK_GRANT_TURN),
                )
            }
            if (sessionIndex != null) {
                AmuxActionButton(
                    label = stringResource(R.string.ask_access_grant_session),
                    kind = if (turnIndex == null) ActionButtonKind.Primary else ActionButtonKind.Outline,
                    fillWidth = true,
                    onClick = { onAction(AskAction.Choose(sessionIndex, null, askKey = card.key)) },
                    modifier = Modifier.testTag(AskReviewTags.ASK_GRANT_SESSION),
                )
            }
            denyIndices.forEach { idx ->
                AmuxActionButton(
                    label = ChatWords.choice(card.choices[idx]),
                    kind = ActionButtonKind.Outline,
                    fillWidth = true,
                    onClick = { onAction(AskAction.Choose(idx, null, askKey = card.key)) },
                    modifier = Modifier.testTag(AskReviewTags.askChoice(idx)),
                )
            }
        }
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

@Composable
private fun QuestionAskSection(
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
        return picks[index].isNotEmpty() || !others[index]?.trim().isNullOrEmpty()
    }

    fun decided(index: Int): Boolean =
        answered(index) || (index in skipped.indices && skipped[index])

    fun buildResponses(
        currentPicks: List<Set<Int>> = picks,
        currentOthers: List<String?> = others,
        currentNotes: List<String> = notes,
    ): List<QuestionResponse> = questions.indices.map { index ->
        val note = currentNotes.getOrElse(index) { "" }.trim()
        val other = currentOthers.getOrNull(index)?.trim()?.takeIf { it.isNotEmpty() }
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
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(AskReviewTags.ASK_REPLY_BACK),
                ) {
                    Text(
                        text = stringResource(R.string.action_back),
                        style = AmuxTheme.typography.detail,
                        color = colors.inkMuted,
                    )
                }
            }
            OutlinedTextField(
                value = replyText,
                onValueChange = { replyText = it },
                textStyle = AmuxTheme.typography.body.copy(color = colors.ink),
                minLines = 2,
                maxLines = 5,
                shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                colors = amuxTextFieldColors(),
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
                            MarkdownDocument.inline(q.question).plainText
                        } else {
                            "${q.header} · ${MarkdownDocument.inline(q.question).plainText}"
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
                                .padding(horizontal = Space.m, vertical = Space.s)
                                .thumbTarget()
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

                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f, fill = false)
                        .verticalScroll(rememberScrollState()),
                    verticalArrangement = Arrangement.spacedBy(Space.m),
                ) {
                    Text(
                        text = MarkdownDocument.inline(currentQuestion.question).plainText,
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
                                text = MarkdownDocument.inline(option.label).plainText,
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
                                OutlinedTextField(
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
                                    placeholder = {
                                        Text(
                                            text = otherLabel,
                                            style = AmuxTheme.typography.body,
                                            color = colors.inkFaint,
                                        )
                                    },
                                    textStyle = AmuxTheme.typography.body.copy(color = colors.ink),
                                    singleLine = true,
                                    visualTransformation = if (currentQuestion.secret) {
                                        PasswordVisualTransformation()
                                    } else {
                                        VisualTransformation.None
                                    },
                                    shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                                    colors = amuxTextFieldColors(),
                                    modifier = Modifier
                                        .fillMaxWidth()
                                        .semantics { contentDescription = otherLabel }
                                        .testTag(AskReviewTags.ASK_OTHER_INPUT),
                                )
                            } else {
                                QuestionOptionRow(
                                    option = OptionView(
                                        label = stringResource(R.string.ask_question_something_else_ellipsis),
                                        description = "",
                                        preview = "",
                                        recommended = false,
                                    ),
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
                            OutlinedTextField(
                                value = notes.getOrElse(index) { "" },
                                onValueChange = { typed ->
                                    notes = notes.toMutableList().apply { this[index] = typed }
                                },
                                placeholder = {
                                    Text(
                                        text = notePlaceholder,
                                        style = AmuxTheme.typography.body,
                                        color = colors.inkFaint,
                                    )
                                },
                                textStyle = AmuxTheme.typography.body.copy(color = colors.ink),
                                minLines = 1,
                                maxLines = 4,
                                shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                                colors = amuxTextFieldColors(),
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
                                modifier = Modifier
                                    .thumbTarget()
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
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(AskReviewTags.ASK_REPLY_LINK),
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
        question.options.getOrNull(pos)?.let { MarkdownDocument.inline(it.label).plainText }
    }.toMutableList()
    val trimmedOther = other?.trim()
    if (!trimmedOther.isNullOrEmpty()) {
        parts += if (question.secret) {
            stringResource(R.string.ask_question_answered_hidden)
        } else {
            "\u201C$trimmedOther\u201D"
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
    val container = if (current) colors.ink else colors.ground
    val content = when {
        current -> colors.ground
        done -> colors.ink
        else -> colors.inkMuted
    }
    Surface(
        modifier = modifier
            .thumbTarget()
            .clip(RoundedCornerShape(50))
            .clickable(role = Role.Tab, onClick = onClick)
            .semantics { selected = current },
        shape = RoundedCornerShape(50),
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
                    modifier = Modifier.size(14.dp),
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
    val indicatorShape = RoundedCornerShape(if (multi) 5.dp else 9.dp)
    val labelPlain = MarkdownDocument.inline(option.label).plainText
    val detailPlain = if (option.description != option.label) {
        MarkdownDocument.inline(option.description).plainText
    } else {
        ""
    }

    Row(
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
            .background(colors.sunken)
            .border(
                width = if (isSelected) 1.5.dp else AmuxTheme.metrics.hairline,
                color = if (isSelected) colors.ink else colors.hairline.copy(alpha = 0.5f),
                shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
            )
            .clickable(role = if (multi) Role.Checkbox else Role.RadioButton, onClick = onClick)
            .thumbTarget()
            .padding(horizontal = Space.m, vertical = Space.s)
            .semantics { selected = isSelected },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
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
                    tint = colors.ground,
                    modifier = Modifier.size(12.dp),
                )
            }
        }
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(Space.xxs),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                Text(
                    text = labelPlain,
                    style = if (quiet) AmuxTheme.typography.body else AmuxTheme.typography.bodyEmphasis,
                    color = if (quiet) colors.inkMuted else colors.ink,
                    modifier = Modifier.weight(1f, fill = false),
                )
                if (option.recommended) {
                    AmuxStatusChip(text = stringResource(R.string.ask_question_recommended))
                }
            }
            if (detailPlain.isNotEmpty()) {
                Text(
                    text = detailPlain,
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
            }
        }
    }
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
            .thumbTarget()
            .padding(horizontal = Space.m, vertical = Space.s),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Icon(
            imageVector = Icons.Outlined.Repeat,
            contentDescription = null,
            tint = colors.inkMuted,
            modifier = Modifier.size(16.dp),
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
        Icon(
            imageVector = Icons.Outlined.ChevronRight,
            contentDescription = null,
            tint = colors.inkFaint,
            modifier = Modifier.size(16.dp),
        )
    }
}

@Composable
private fun AskSubjectBox(
    text: String,
    modifier: Modifier = Modifier,
    added: Int? = null,
    removed: Int? = null,
    maxLines: Int = Int.MAX_VALUE,
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
                overflow = TextOverflow.Ellipsis,
            )
        }
        if (added != null && removed != null) {
            DiffDeltaCounts(added = added, removed = removed)
        }
    }
}

@Composable
private fun AskWhyText(text: String) {
    Text(
        text = text,
        style = AmuxTheme.typography.detail,
        color = AmuxTheme.colors.inkMuted,
    )
}

private const val DIFF_PREVIEW_LIMIT: Int = 12
private const val DIFF_WHOLE_CAP: Int = 400

@Composable
private fun EditDiffPreview(
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

@Composable
internal fun DiffDeltaCounts(
    added: Int,
    removed: Int,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = modifier,
        horizontalArrangement = Arrangement.spacedBy(Space.xs),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (added > 0) {
            Text(
                text = stringResource(R.string.review_diff_added, added),
                style = AmuxTheme.typography.monoSmall,
                color = colors.added,
            )
        }
        if (removed > 0) {
            Text(
                text = stringResource(R.string.review_diff_removed, removed),
                style = AmuxTheme.typography.monoSmall,
                color = colors.removed,
            )
        }
    }
}

@Composable
private fun AskButtonPair(
    primary: (@Composable () -> Unit)?,
    secondary: (@Composable () -> Unit)?,
) {
    if (primary == null && secondary == null) return
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        if (primary != null) {
            Box(modifier = Modifier.weight(1f)) { primary() }
        }
        if (secondary != null) {
            Box(modifier = Modifier.weight(1f)) { secondary() }
        }
    }
}

@Composable
internal fun amuxTextFieldColors() = OutlinedTextFieldDefaults.colors(
    focusedBorderColor = AmuxTheme.colors.ink,
    unfocusedBorderColor = AmuxTheme.colors.hairline,
    errorBorderColor = AmuxTheme.colors.accent,
    focusedContainerColor = AmuxTheme.colors.ground,
    unfocusedContainerColor = AmuxTheme.colors.ground,
    cursorColor = AmuxTheme.colors.ink,
)

@Composable
internal fun amuxSwitchColors() = SwitchDefaults.colors(
    checkedThumbColor = AmuxTheme.colors.onAccent,
    checkedTrackColor = AmuxTheme.colors.accent,
    uncheckedThumbColor = AmuxTheme.colors.inkMuted,
    uncheckedTrackColor = AmuxTheme.colors.sunken,
    uncheckedBorderColor = AmuxTheme.colors.hairline,
)
