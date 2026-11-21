package sh.amux.features

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Send
import androidx.compose.material.icons.automirrored.outlined.ViewList
import androidx.compose.material.icons.outlined.AccountTree
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.ArrowUpward
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material.icons.outlined.Mic
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.outlined.Stop
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.material.icons.outlined.Tune
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxChrome
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxTheme
import sh.amux.design.Motion
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.Activity
import sh.amux.values.AttachmentView
import sh.amux.values.CommandMatch
import sh.amux.values.Composer
import sh.amux.values.ComposerView
import sh.amux.values.ContextView
import sh.amux.values.ControlsSummary
import sh.amux.values.ExitCause
import sh.amux.values.GroupState
import sh.amux.values.JobStop
import sh.amux.values.Kind
import sh.amux.values.PhaseView
import sh.amux.values.QueuedRow
import sh.amux.values.Reach
import sh.amux.values.Refusal
import sh.amux.values.RefusalReason
import sh.amux.values.RefusedPrompt
import sh.amux.values.RouteKind
import sh.amux.values.SentPrompt
import sh.amux.values.SignInView
import sh.amux.values.Underway
import sh.amux.words.ChatWords
import sh.amux.words.EMPTY_GROUP_STATE
import sh.amux.words.nativeRunner
import sh.amux.words.nativeTakesNoMessages
import sh.amux.words.signInDetail

/**
 * What the composer's `+` attach menu offers.
 */
enum class AttachChoice {
    Photo,
    File,
}

/**
 * One draft attachment shown in the composer's chip row.
 */
@Immutable
data class ComposerAttachmentUi(
    val id: String,
    val view: AttachmentView,
    val thumbnailBytes: ByteArray? = null,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ComposerAttachmentUi) return false
        return id == other.id && view == other.view && thumbnailBytes.contentEquals(other.thumbnailBytes)
    }

    override fun hashCode(): Int {
        var result = id.hashCode()
        result = 31 * result + view.hashCode()
        result = 31 * result + (thumbnailBytes?.contentHashCode() ?: 0)
        return result
    }
}

/**
 * Local test tags for [ChatComposer].
 */
object ChatComposerTags {
    const val ROOT: String = "chat.composer"
    const val ACTIVITY: String = "chat.activity"
    const val EXITED_HEAD: String = "chat.exited"
    const val SWITCHED_AWAY: String = "chat.switchedAway"
    const val SWITCH_BACK: String = "chat.switchBack"
    const val SIGN_IN_NOTICE: String = "chat.foot.sign-in"
    const val NOTICE: String = "chat.notice"
    const val QUEUED_STRIP: String = "chat.queued.strip"
    const val SLASH_POPUP: String = "chat.slash.popup"
    const val ATTACHMENTS_ROW: String = "chat.attachments"
    const val ATTACHING: String = "chat.attaching"
    const val CONTROLS_STRIP: String = "chat.controls"
    const val MODEL_CHIP: String = "chat.model"
    const val OVERVIEW_CHIP: String = "chat.composer.overview"
    const val ATTACH_BUTTON: String = "chat.attach"
    const val ATTACH_MENU_PHOTO: String = "chat.attach.photo"
    const val ATTACH_MENU_FILE: String = "chat.attach.file"
    const val INPUT_FIELD: String = "chat.field"
    const val DICTATE_BUTTON: String = "chat.dictate"
    const val INTERRUPT_BUTTON: String = "chat.stop"
    const val SEND_BUTTON: String = "chat.send"
    const val RESUME_BUTTON: String = "chat.resume"
    const val NATIVE_COMPOSER: String = "chat.native"
    const val NATIVE_STOP: String = "chat.native.stop"
    const val NATIVE_STOPPING: String = "chat.native.stopping"
    const val NATIVE_REFUSED: String = "chat.native.refused"

    fun queuedRow(index: Int): String = "chat.queued.$index"
    fun queuedSendNow(index: Int): String = "chat.queued.$index.sendNow"
    fun queuedWithdraw(index: Int): String = "chat.queued.$index.withdraw"
    fun underwayRow(index: Int): String = "chat.underway.$index"
    fun underwayResend(index: Int): String = "chat.underway.$index.resend"
    fun underwayDiscard(index: Int): String = "chat.underway.$index.discard"
    fun refusedRow(index: Int): String = "chat.refused.$index"
    fun refusedDismiss(index: Int): String = "chat.refused.$index.dismiss"
    fun slashMatch(name: String): String = "chat.slash.$name"
}

/**
 * Pure UI state rendered by [ChatComposer].
 */
@Immutable
data class ChatComposerState(
    val composer: ComposerView,
    val draft: String = "",
    val host: String = "",
    val reach: Reach = Reach.Online(RouteKind.Direct),
    val connecting: Boolean = false,
    val phase: PhaseView = PhaseView.Idle,
    val kind: Kind? = null,
    val settled: Boolean = false,
    val canSend: Boolean = true,
    val canResume: Boolean = true,
    val activitySubject: String? = null,
    val liveSubagents: GroupState = EMPTY_GROUP_STATE,
    val signIn: SignInView? = null,
    val queued: List<QueuedRow> = emptyList(),
    val underway: List<SentPrompt> = emptyList(),
    val refused: List<RefusedPrompt> = emptyList(),
    val slashMatches: List<CommandMatch> = emptyList(),
    val attachments: List<ComposerAttachmentUi> = emptyList(),
    val uploadingCount: Int = 0,
    val notice: String? = null,
    val controls: ControlsSummary? = null,
    val context: ContextView? = null,
    val settingsAvailable: Boolean = true,
    val overviewSummary: String? = null,
    val showOverviewChip: Boolean = false,
    val usageLimitMessage: String? = null,
    val dictationEnabled: Boolean = false,
    val dictationActive: Boolean = false,
    val dictationSentence: String? = null,
    val nowMs: Long = 0L,
) {
    val hasDraft: Boolean
        get() = draft.trim().isNotEmpty() || attachments.isNotEmpty()

    val isWorking: Boolean
        get() = phase is PhaseView.Working
}

/**
 * Callbacks emitted by [ChatComposer] without coupling to runtime or platform pickers.
 */
@Immutable
data class ChatComposerCallbacks(
    val onDraftChange: (String) -> Unit = {},
    val onSend: () -> Unit = {},
    val onResume: () -> Unit = {},
    val onInterrupt: () -> Unit = {},
    val onAttach: (AttachChoice) -> Unit = {},
    val onRemoveAttachment: (index: Int) -> Unit = {},
    val onPickCommand: (CommandMatch) -> Unit = {},
    val onSendNow: (QueuedRow) -> Unit = {},
    val onWithdraw: (QueuedRow) -> Unit = {},
    val onResend: (inputId: List<Int>) -> Unit = {},
    val onDiscard: (inputId: List<Int>) -> Unit = {},
    val onDismissRefused: (inputId: List<Int>) -> Unit = {},
    val onSwitchBack: () -> Unit = {},
    val onStopNative: (key: String) -> Unit = {},
    val onOpenSettings: () -> Unit = {},
    val onOpenOverview: () -> Unit = {},
    val onToggleDictation: () -> Unit = {},
)

private const val MAX_SLASH_POPUP_ITEMS = 5

/**
 * Floating Android-native composer dock built on [AmuxChrome], designed to sit
 * above `Modifier.imePadding()`.
 */
@Composable
fun ChatComposer(
    state: ChatComposerState,
    callbacks: ChatComposerCallbacks = ChatComposerCallbacks(),
    modifier: Modifier = Modifier,
) {
    val nativeMode = state.composer.mode as? Composer.Native

    Column(
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = Space.m, vertical = Space.s)
            .testTag(ChatComposerTags.ROOT),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        if (state.composer.switchedAway) {
            SwitchedAwayBanner(onSwitchBack = callbacks.onSwitchBack)
        }

        if (state.signIn != null && nativeMode == null) {
            SignInNoticeCard(
                signIn = state.signIn,
                kind = state.kind,
                host = state.host,
            )
        }

        if (!state.notice.isNullOrEmpty()) {
            Text(
                text = state.notice,
                style = AmuxTheme.typography.detail,
                color = AmuxTheme.colors.removed,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.xs)
                    .testTag(ChatComposerTags.NOTICE),
            )
        }

        if (state.queued.isNotEmpty() || state.underway.isNotEmpty() || state.refused.isNotEmpty()) {
            PromptsQueueStrip(
                queued = state.queued,
                underway = state.underway,
                refused = state.refused,
                host = state.host,
                callbacks = callbacks,
            )
        }

        if (nativeMode == null && state.slashMatches.isNotEmpty()) {
            SlashMatchesPopup(
                matches = state.slashMatches.take(MAX_SLASH_POPUP_ITEMS),
                onPick = callbacks.onPickCommand,
            )
        }

        if (nativeMode != null) {
            NativeSubagentComposerCard(
                stop = nativeMode.stop,
                activity = state.composer.activity,
                activitySubject = state.activitySubject,
                liveSubagents = state.liveSubagents,
                kind = state.kind,
                phase = state.phase,
                nowMs = state.nowMs,
                onStop = callbacks.onStopNative,
            )
        } else {
            StandardComposerCard(
                state = state,
                callbacks = callbacks,
            )
        }
    }
}

@Composable
private fun SwitchedAwayBanner(onSwitchBack: () -> Unit) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    AmuxChrome(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(ChatComposerTags.SWITCHED_AWAY),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(Space.m + Space.xxs),
            verticalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
        ) {
            Text(
                text = stringResource(R.string.chat_composer_switched_away),
                style = typography.detail,
                color = colors.ink,
            )
            AmuxActionButton(
                label = stringResource(R.string.chat_composer_switch_back),
                kind = ActionButtonKind.Primary,
                fillWidth = true,
                onClick = onSwitchBack,
                modifier = Modifier.testTag(ChatComposerTags.SWITCH_BACK),
            )
        }
    }
}

@Composable
private fun SignInNoticeCard(
    signIn: SignInView,
    kind: Kind?,
    host: String,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val title = remember(kind) { ChatWords.needsSignIn(kind) }
    val detail = remember(signIn, kind, host) { ChatWords.signInDetail(signIn, kind = kind, host = host) }

    AmuxChrome(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(ChatComposerTags.SIGN_IN_NOTICE),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(Space.l),
            horizontalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
            verticalAlignment = Alignment.Top,
        ) {
            Icon(
                imageVector = Icons.Outlined.ErrorOutline,
                contentDescription = null,
                tint = colors.accent,
                modifier = Modifier
                    .padding(top = Space.xxs)
                    .size(18.dp),
            )
            Column(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(Space.xs),
            ) {
                Text(
                    text = title,
                    style = typography.bodyEmphasis,
                    color = colors.ink,
                )
                if (detail.isNotEmpty()) {
                    Text(
                        text = detail,
                        style = typography.detail,
                        color = colors.inkMuted,
                    )
                }
            }
        }
    }
}

@Composable
private fun PromptsQueueStrip(
    queued: List<QueuedRow>,
    underway: List<SentPrompt>,
    refused: List<RefusedPrompt>,
    host: String,
    callbacks: ChatComposerCallbacks,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography

    AmuxChrome(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(ChatComposerTags.QUEUED_STRIP),
    ) {
        Column(modifier = Modifier.fillMaxWidth()) {
            var shownAny = false

            queued.forEachIndexed { index, row ->
                if (shownAny) AmuxHairline()
                shownAny = true
                val preview = remember(row.text) { ChatWords.text(row.text) }
                val stateWord = remember(row) { ChatWords.queued(row) }
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(start = Space.l, end = Space.xs, top = Space.s, bottom = Space.s)
                        .testTag(ChatComposerTags.queuedRow(index)),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.Schedule,
                        contentDescription = null,
                        tint = colors.inkMuted,
                        modifier = Modifier.size(16.dp),
                    )
                    Column(
                        modifier = Modifier.weight(1f),
                        verticalArrangement = Arrangement.spacedBy(Space.xxs),
                    ) {
                        Text(
                            text = preview,
                            style = typography.body,
                            color = colors.ink,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Row(
                            horizontalArrangement = Arrangement.spacedBy(Space.xs),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            if (row.steered) {
                                AmuxStatusChip(text = stateWord)
                            } else {
                                Text(
                                    text = stateWord,
                                    style = typography.caption,
                                    color = colors.inkFaint,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                        }
                    }
                    if (row.canSendNow) {
                        val sendNowLabel = stringResource(R.string.chat_composer_send_now)
                        IconButton(
                            onClick = { callbacks.onSendNow(row) },
                            modifier = Modifier
                                .thumbTarget()
                                .testTag(ChatComposerTags.queuedSendNow(index)),
                        ) {
                            Icon(
                                imageVector = Icons.Outlined.ArrowUpward,
                                contentDescription = sendNowLabel,
                                tint = colors.inkMuted,
                                modifier = Modifier.size(18.dp),
                            )
                        }
                    }
                    if (row.canWithdraw) {
                        val withdrawLabel = stringResource(R.string.chat_composer_withdraw)
                        IconButton(
                            onClick = { callbacks.onWithdraw(row) },
                            modifier = Modifier
                                .thumbTarget()
                                .testTag(ChatComposerTags.queuedWithdraw(index)),
                        ) {
                            Icon(
                                imageVector = Icons.Outlined.Edit,
                                contentDescription = withdrawLabel,
                                tint = colors.inkMuted,
                                modifier = Modifier.size(18.dp),
                            )
                        }
                    }
                }
            }

            underway.forEachIndexed { index, prompt ->
                if (shownAny) AmuxHairline()
                shownAny = true
                val preview = remember(prompt.text) { ChatWords.text(prompt.text) }
                val lost = prompt.underway is Underway.MayNotHaveArrived
                val statusWord = remember(prompt.underway, host) {
                    ChatWords.underway(prompt.underway, host = host)
                }
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(start = Space.l, end = Space.xs, top = Space.s, bottom = Space.s)
                        .testTag(ChatComposerTags.underwayRow(index)),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    Icon(
                        imageVector = if (lost) Icons.Outlined.ErrorOutline else Icons.AutoMirrored.Outlined.Send,
                        contentDescription = null,
                        tint = if (lost) colors.accent else colors.inkMuted,
                        modifier = Modifier.size(16.dp),
                    )
                    Column(
                        modifier = Modifier.weight(1f),
                        verticalArrangement = Arrangement.spacedBy(Space.xxs),
                    ) {
                        Text(
                            text = preview,
                            style = typography.body,
                            color = colors.ink,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Text(
                            text = statusWord,
                            style = typography.caption,
                            color = if (lost) colors.accent else colors.inkFaint,
                            maxLines = 2,
                        )
                    }
                    if (lost) {
                        val resendLabel = stringResource(R.string.chat_composer_resend)
                        IconButton(
                            onClick = { callbacks.onResend(prompt.inputId) },
                            modifier = Modifier
                                .thumbTarget()
                                .testTag(ChatComposerTags.underwayResend(index)),
                        ) {
                            Icon(
                                imageVector = Icons.Outlined.Refresh,
                                contentDescription = resendLabel,
                                tint = colors.inkMuted,
                                modifier = Modifier.size(18.dp),
                            )
                        }
                        val discardLabel = stringResource(R.string.chat_composer_discard)
                        IconButton(
                            onClick = { callbacks.onDiscard(prompt.inputId) },
                            modifier = Modifier
                                .thumbTarget()
                                .testTag(ChatComposerTags.underwayDiscard(index)),
                        ) {
                            Icon(
                                imageVector = Icons.Outlined.Close,
                                contentDescription = discardLabel,
                                tint = colors.inkMuted,
                                modifier = Modifier.size(18.dp),
                            )
                        }
                    }
                }
            }

            refused.forEachIndexed { index, prompt ->
                if (shownAny) AmuxHairline()
                shownAny = true
                val reasonText = refusalMessage(prompt.reason)
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(start = Space.l, end = Space.xs, top = Space.s, bottom = Space.s)
                        .testTag(ChatComposerTags.refusedRow(index)),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.ErrorOutline,
                        contentDescription = null,
                        tint = colors.removed,
                        modifier = Modifier.size(16.dp),
                    )
                    Text(
                        text = reasonText,
                        style = typography.detail,
                        color = colors.removed,
                        modifier = Modifier.weight(1f),
                    )
                    val dismissLabel = stringResource(R.string.chat_composer_dismiss)
                    IconButton(
                        onClick = { callbacks.onDismissRefused(prompt.inputId) },
                        modifier = Modifier
                            .thumbTarget()
                            .testTag(ChatComposerTags.refusedDismiss(index)),
                    ) {
                        Icon(
                            imageVector = Icons.Outlined.Close,
                            contentDescription = dismissLabel,
                            tint = colors.inkMuted,
                            modifier = Modifier.size(18.dp),
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun refusalMessage(refusal: Refusal): String {
    val detail = refusal.detail?.trim()
    val why = if (!detail.isNullOrEmpty()) {
        detail
    } else {
        when (refusal.reason) {
            RefusalReason.HostUnreachable -> stringResource(R.string.chat_composer_refusal_host_unreachable)
            RefusalReason.Exited -> stringResource(R.string.chat_composer_refusal_exited)
            RefusalReason.Exiting -> stringResource(R.string.chat_composer_refusal_exiting)
            RefusalReason.Draining -> stringResource(R.string.chat_composer_refusal_draining)
            RefusalReason.ClosedAsk -> stringResource(R.string.chat_composer_refusal_closed_ask)
            RefusalReason.NotQueued -> stringResource(R.string.chat_composer_refusal_not_queued)
            RefusalReason.Unsupported -> stringResource(R.string.chat_composer_refusal_unsupported)
            RefusalReason.ConversationElsewhere -> stringResource(R.string.chat_composer_refusal_elsewhere)
            RefusalReason.TooLong -> stringResource(R.string.chat_composer_refusal_too_long)
            RefusalReason.AgentClosed -> stringResource(R.string.chat_composer_refusal_closed)
            RefusalReason.Unspecified -> stringResource(R.string.chat_composer_refusal_generic)
        }
    }
    return stringResource(R.string.chat_composer_refused_prefix, why)
}

@Composable
private fun SlashMatchesPopup(
    matches: List<CommandMatch>,
    onPick: (CommandMatch) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography

    AmuxChrome(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(ChatComposerTags.SLASH_POPUP),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(vertical = Space.xs),
        ) {
            matches.forEachIndexed { index, match ->
                if (index > 0) AmuxHairline(color = colors.hairline.copy(alpha = 0.5f))
                val command = match.command
                val spoken = remember(command) { ChatWords.command(command) }
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .thumbTarget()
                        .clickable(role = Role.Button, onClick = { onPick(match) })
                        .semantics { contentDescription = spoken }
                        .padding(horizontal = Space.l, vertical = Space.s)
                        .testTag(ChatComposerTags.slashMatch(command.name)),
                    verticalArrangement = Arrangement.spacedBy(Space.xxs),
                ) {
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(Space.xs + Space.xxs),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            text = command.sigil + command.name,
                            style = typography.mono,
                            color = colors.ink,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        if (command.argumentHint.isNotEmpty()) {
                            Text(
                                text = command.argumentHint,
                                style = typography.monoSmall,
                                color = colors.inkFaint,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                    if (command.description.isNotEmpty()) {
                        Text(
                            text = command.description,
                            style = typography.detail,
                            color = colors.inkMuted,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun StandardComposerCard(
    state: ChatComposerState,
    callbacks: ChatComposerCallbacks,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    var attachMenuOpen by remember { mutableStateOf(false) }

    val placeholder = remember(
        state.composer.mode,
        state.host,
        state.reach,
        state.connecting,
        state.isWorking,
        state.usageLimitMessage,
    ) {
        ChatWords.placeholder(
            composer = state.composer.mode,
            host = state.host,
            reach = state.reach,
            connecting = state.connecting,
            working = state.isWorking,
            limit = state.usageLimitMessage,
        )
    }

    val controlsChip = remember(state.controls, state.context) {
        state.controls?.let { ChatWords.chip(it, context = state.context) }
    }

    AmuxChrome(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.l, vertical = Space.m),
            verticalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
        ) {
            val activity = state.composer.activity
            val exitedCause = (state.phase as? PhaseView.Exited)?.cause
            if (activity != null) {
                ActivityLineView(
                    activity = activity,
                    subject = state.activitySubject,
                    subagents = state.liveSubagents,
                    nowMs = state.nowMs,
                )
            } else if (exitedCause != null) {
                ExitedHeadLine(
                    cause = exitedCause,
                    settled = state.settled,
                    host = state.host,
                )
            }

            if (!state.dictationSentence.isNullOrEmpty()) {
                Text(
                    text = state.dictationSentence,
                    style = typography.caption,
                    color = colors.inkMuted,
                )
            }

            if (state.attachments.isNotEmpty() || state.uploadingCount > 0) {
                FlowRow(
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag(ChatComposerTags.ATTACHMENTS_ROW),
                    horizontalArrangement = Arrangement.spacedBy(Space.xs + Space.xxs),
                    verticalArrangement = Arrangement.spacedBy(Space.xs + Space.xxs),
                ) {
                    state.attachments.forEachIndexed { index, attachment ->
                        AttachmentChip(
                            view = attachment.view,
                            thumbnailBytes = attachment.thumbnailBytes,
                            onRemove = { callbacks.onRemoveAttachment(index) },
                        )
                    }
                    if (state.uploadingCount > 0) {
                        Text(
                            text = stringResource(R.string.chat_composer_attaching),
                            style = typography.caption,
                            color = colors.inkFaint,
                            modifier = Modifier
                                .align(Alignment.CenterVertically)
                                .testTag(ChatComposerTags.ATTACHING),
                        )
                    }
                }
            }

            BasicTextField(
                value = state.draft,
                onValueChange = callbacks.onDraftChange,
                textStyle = typography.body.copy(color = colors.ink),
                cursorBrush = SolidColor(colors.ink),
                maxLines = 8,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(vertical = Space.xxs)
                    .semantics { contentDescription = placeholder }
                    .testTag(ChatComposerTags.INPUT_FIELD),
                decorationBox = { innerTextField ->
                    Box(modifier = Modifier.fillMaxWidth()) {
                        if (state.draft.isEmpty()) {
                            Text(
                                text = placeholder,
                                style = typography.body,
                                color = colors.inkFaint,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                        innerTextField()
                    }
                },
            )

            if (controlsChip != null || state.showOverviewChip) {
                ControlsSummaryStrip(
                    chip = controlsChip,
                    warnsPermission = state.controls?.permission?.neverAsks == true,
                    settingsAvailable = state.settingsAvailable,
                    showOverviewChip = state.showOverviewChip,
                    overviewSummary = state.overviewSummary,
                    onOpenSettings = callbacks.onOpenSettings,
                    onOpenOverview = callbacks.onOpenOverview,
                )
            }

            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.xs),
            ) {
                Box {
                    val attachLabel = stringResource(R.string.chat_composer_attach)
                    IconButton(
                        onClick = { attachMenuOpen = !attachMenuOpen },
                        modifier = Modifier
                            .thumbTarget()
                            .testTag(ChatComposerTags.ATTACH_BUTTON),
                    ) {
                        Box(
                            modifier = Modifier
                                .size(34.dp)
                                .clip(CircleShape)
                                .background(colors.sunken),
                            contentAlignment = Alignment.Center,
                        ) {
                            Icon(
                                imageVector = Icons.Outlined.Add,
                                contentDescription = attachLabel,
                                tint = colors.ink,
                                modifier = Modifier.size(18.dp),
                            )
                        }
                    }

                    DropdownMenu(
                        expanded = attachMenuOpen,
                        onDismissRequest = { attachMenuOpen = false },
                    ) {
                        DropdownMenuItem(
                            text = { Text(text = stringResource(R.string.chat_composer_attach_photo)) },
                            leadingIcon = {
                                Icon(
                                    imageVector = Icons.Outlined.Image,
                                    contentDescription = null,
                                    tint = colors.ink,
                                )
                            },
                            onClick = {
                                attachMenuOpen = false
                                callbacks.onAttach(AttachChoice.Photo)
                            },
                            modifier = Modifier
                                .thumbTarget()
                                .testTag(ChatComposerTags.ATTACH_MENU_PHOTO),
                        )
                        DropdownMenuItem(
                            text = { Text(text = stringResource(R.string.chat_composer_attach_file)) },
                            leadingIcon = {
                                Icon(
                                    imageVector = Icons.Outlined.Description,
                                    contentDescription = null,
                                    tint = colors.ink,
                                )
                            },
                            onClick = {
                                attachMenuOpen = false
                                callbacks.onAttach(AttachChoice.File)
                            },
                            modifier = Modifier
                                .thumbTarget()
                                .testTag(ChatComposerTags.ATTACH_MENU_FILE),
                        )
                    }
                }

                Spacer(modifier = Modifier.weight(1f))

                if (state.dictationEnabled) {
                    val dictateLabel = stringResource(
                        if (state.dictationActive) {
                            R.string.chat_composer_stop_dictation
                        } else {
                            R.string.chat_composer_dictate
                        },
                    )
                    IconButton(
                        onClick = callbacks.onToggleDictation,
                        modifier = Modifier
                            .thumbTarget()
                            .testTag(ChatComposerTags.DICTATE_BUTTON),
                    ) {
                        Icon(
                            imageVector = if (state.dictationActive) Icons.Outlined.StopCircle else Icons.Outlined.Mic,
                            contentDescription = dictateLabel,
                            tint = if (state.dictationActive) colors.accent else colors.inkMuted,
                            modifier = Modifier.size(20.dp),
                        )
                    }
                }

                ComposerPrimaryControl(
                    state = state,
                    callbacks = callbacks,
                )
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ControlsSummaryStrip(
    chip: ChatWords.ControlsChip?,
    warnsPermission: Boolean,
    settingsAvailable: Boolean,
    showOverviewChip: Boolean,
    overviewSummary: String?,
    onOpenSettings: () -> Unit,
    onOpenOverview: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val overviewFallback = stringResource(R.string.chat_composer_overview)

    FlowRow(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(ChatComposerTags.CONTROLS_STRIP),
        horizontalArrangement = Arrangement.spacedBy(Space.s),
        verticalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        if (chip != null) {
            val annotated = remember(chip, warnsPermission, colors) {
                buildAnnotatedString {
                    var led = false
                    if (chip.model.isNotEmpty()) {
                        withStyle(SpanStyle(color = colors.inkMuted)) { append(chip.model) }
                        led = true
                    }
                    if (chip.detail.isNotEmpty()) {
                        withStyle(SpanStyle(color = colors.inkMuted)) {
                            if (led) append(" · ")
                            append(chip.detail)
                        }
                        led = true
                    }
                    chip.unusual.forEachIndexed { index, word ->
                        val isWarning = warnsPermission && index == 0
                        withStyle(SpanStyle(color = if (isWarning) colors.removed else colors.ink)) {
                            if (led) append(" · ")
                            append(word)
                        }
                        led = true
                    }
                }
            }
            val spoken = remember(chip) {
                (listOf(chip.model, chip.detail) + chip.unusual)
                    .filter { it.isNotEmpty() }
                    .joinToString(" · ")
            }
            val settingsDescription = stringResource(R.string.chat_composer_settings_named, spoken)
            Surface(
                modifier = Modifier
                    .thumbTarget()
                    .clip(RoundedCornerShape(50))
                    .clickable(enabled = settingsAvailable, role = Role.Button, onClick = onOpenSettings)
                    .semantics { contentDescription = settingsDescription }
                    .testTag(ChatComposerTags.MODEL_CHIP),
                shape = RoundedCornerShape(50),
                color = colors.sunken,
                border = BorderStroke(AmuxTheme.metrics.hairline, colors.hairline),
            ) {
                Row(
                    modifier = Modifier.padding(horizontal = Space.m, vertical = Space.s),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.xs),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.Tune,
                        contentDescription = null,
                        tint = colors.inkFaint,
                        modifier = Modifier.size(13.dp),
                    )
                    Text(
                        text = annotated,
                        style = typography.caption,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.widthIn(max = 220.dp),
                    )
                    Icon(
                        imageVector = Icons.Outlined.ExpandMore,
                        contentDescription = null,
                        tint = colors.inkFaint,
                        modifier = Modifier.size(14.dp),
                    )
                }
            }
        }

        if (showOverviewChip) {
            val overviewText = overviewSummary?.takeIf { it.isNotBlank() } ?: overviewFallback
            Surface(
                modifier = Modifier
                    .thumbTarget()
                    .clip(RoundedCornerShape(50))
                    .clickable(role = Role.Button, onClick = onOpenOverview)
                    .semantics { contentDescription = overviewText }
                    .testTag(ChatComposerTags.OVERVIEW_CHIP),
                shape = RoundedCornerShape(50),
                color = colors.sunken,
                border = BorderStroke(AmuxTheme.metrics.hairline, colors.hairline),
            ) {
                Row(
                    modifier = Modifier.padding(horizontal = Space.m, vertical = Space.s),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.xs),
                ) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Outlined.ViewList,
                        contentDescription = null,
                        tint = colors.inkMuted,
                        modifier = Modifier.size(14.dp),
                    )
                    Text(
                        text = overviewText,
                        style = typography.caption,
                        color = colors.ink,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

@Composable
private fun ComposerPrimaryControl(
    state: ChatComposerState,
    callbacks: ChatComposerCallbacks,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography

    when {
        state.composer.mode is Composer.Resume -> {
            val enabled = state.canResume
            val resumeLabel = stringResource(R.string.chat_composer_resume)
            Button(
                onClick = callbacks.onResume,
                enabled = enabled,
                shape = RoundedCornerShape(50),
                colors = ButtonDefaults.buttonColors(
                    containerColor = colors.ink,
                    contentColor = colors.ground,
                    disabledContainerColor = colors.sunken,
                    disabledContentColor = colors.inkFaint,
                ),
                contentPadding = PaddingValues(horizontal = Space.l, vertical = Space.s),
                modifier = Modifier
                    .thumbTarget()
                    .testTag(ChatComposerTags.RESUME_BUTTON),
            ) {
                Text(
                    text = resumeLabel,
                    style = typography.bodyEmphasis,
                    maxLines = 1,
                )
            }
        }
        !state.hasDraft && state.isWorking -> {
            val interruptLabel = stringResource(R.string.chat_composer_interrupt)
            IconButton(
                onClick = callbacks.onInterrupt,
                modifier = Modifier
                    .thumbTarget()
                    .testTag(ChatComposerTags.INTERRUPT_BUTTON),
            ) {
                Box(
                    modifier = Modifier
                        .size(34.dp)
                        .clip(CircleShape)
                        .background(colors.ink),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        imageVector = Icons.Outlined.Stop,
                        contentDescription = interruptLabel,
                        tint = colors.ground,
                        modifier = Modifier.size(18.dp),
                    )
                }
            }
        }
        else -> {
            val waiting = state.composer.mode is Composer.Waiting
            val enabled = state.canSend && state.hasDraft && !waiting
            val sendLabel = stringResource(
                if (state.isWorking) R.string.chat_composer_queue else R.string.chat_composer_send,
            )
            IconButton(
                onClick = callbacks.onSend,
                enabled = enabled,
                modifier = Modifier
                    .thumbTarget()
                    .testTag(ChatComposerTags.SEND_BUTTON),
            ) {
                Box(
                    modifier = Modifier
                        .size(34.dp)
                        .clip(CircleShape)
                        .background(if (enabled) colors.ink else colors.sunken),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        imageVector = Icons.Outlined.ArrowUpward,
                        contentDescription = sendLabel,
                        tint = if (enabled) colors.ground else colors.inkFaint,
                        modifier = Modifier.size(18.dp),
                    )
                }
            }
        }
    }
}

@Composable
private fun NativeSubagentComposerCard(
    stop: JobStop,
    activity: Activity?,
    activitySubject: String?,
    liveSubagents: GroupState,
    kind: Kind?,
    phase: PhaseView,
    nowMs: Long,
    onStop: (key: String) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val runnerTitle = remember(kind) { ChatWords.nativeRunner(kind) }
    val subtitle = remember(phase) {
        val exited = phase as? PhaseView.Exited
        if (exited != null) ChatWords.exited(exited.cause) else ChatWords.nativeTakesNoMessages
    }

    AmuxChrome(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(ChatComposerTags.NATIVE_COMPOSER),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.l, vertical = Space.m + Space.xxs),
            verticalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
        ) {
            if (activity != null) {
                ActivityLineView(
                    activity = activity,
                    subject = activitySubject,
                    subagents = liveSubagents,
                    nowMs = nowMs,
                )
            }
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.m),
            ) {
                Icon(
                    imageVector = Icons.Outlined.AccountTree,
                    contentDescription = null,
                    tint = colors.inkMuted,
                    modifier = Modifier.size(18.dp),
                )
                Column(
                    modifier = Modifier.weight(1f),
                    verticalArrangement = Arrangement.spacedBy(Space.xxs),
                ) {
                    Text(
                        text = runnerTitle,
                        style = typography.bodyEmphasis,
                        color = colors.ink,
                    )
                    Text(
                        text = subtitle,
                        style = typography.detail,
                        color = colors.inkMuted,
                    )
                }
                when (stop) {
                    is JobStop.Offered -> {
                        AmuxActionButton(
                            label = stringResource(R.string.chat_composer_native_stop),
                            kind = ActionButtonKind.Outline,
                            onClick = { onStop(stop.key) },
                            modifier = Modifier.testTag(ChatComposerTags.NATIVE_STOP),
                        )
                    }
                    is JobStop.Refused -> {
                        AmuxActionButton(
                            label = stringResource(R.string.chat_composer_native_stop),
                            kind = ActionButtonKind.Outline,
                            onClick = { onStop(stop.key) },
                            modifier = Modifier.testTag(ChatComposerTags.NATIVE_STOP),
                        )
                    }
                    JobStop.Stopping -> {
                        Text(
                            text = stringResource(R.string.chat_composer_native_stopping),
                            style = typography.detail,
                            color = colors.inkMuted,
                            modifier = Modifier.testTag(ChatComposerTags.NATIVE_STOPPING),
                        )
                    }
                    JobStop.Unavailable -> Unit
                }
            }
            if (stop is JobStop.Refused && stop.error.isNotEmpty()) {
                Text(
                    text = stop.error,
                    style = typography.detail,
                    color = colors.removed,
                    modifier = Modifier.testTag(ChatComposerTags.NATIVE_REFUSED),
                )
            }
        }
    }
}

@Composable
private fun ExitedHeadLine(
    cause: ExitCause,
    settled: Boolean,
    host: String,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val title = remember(cause, settled) { ChatWords.exited(cause, settled = settled) }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(ChatComposerTags.EXITED_HEAD),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Icon(
            imageVector = Icons.Outlined.StopCircle,
            contentDescription = null,
            tint = colors.inkMuted,
            modifier = Modifier.size(16.dp),
        )
        Text(
            text = title,
            style = typography.bodyEmphasis,
            color = colors.ink,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        if (host.isNotEmpty()) {
            Text(
                text = host,
                style = typography.caption,
                color = colors.inkFaint,
                maxLines = 1,
            )
        }
    }
}

@Composable
private fun ActivityLineView(
    activity: Activity,
    subject: String?,
    subagents: GroupState,
    nowMs: Long,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val elapsedMs = remember(activity, nowMs) {
        if (nowMs > 0L && activity.sinceMs > 0L) {
            maxOf(0L, nowMs - activity.sinceMs)
        } else {
            activity.elapsedMs
        }
    }
    val parts = remember(activity, elapsedMs, subject, subagents) {
        ChatWords.activityParts(
            kind = activity.kind,
            elapsedMs = elapsedMs,
            subject = subject,
            sinceMs = activity.sinceMs,
            subagents = subagents,
        )
    }
    val spoken = remember(activity, elapsedMs, subject, subagents) {
        ChatWords.activity(
            kind = activity.kind,
            elapsedMs = elapsedMs,
            subject = subject,
            sinceMs = activity.sinceMs,
            subagents = subagents,
        )
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .semantics(mergeDescendants = true) { contentDescription = spoken }
            .testTag(ChatComposerTags.ACTIVITY),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            Text(
                text = parts.words,
                style = typography.detail,
                color = colors.inkMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (!parts.subject.isNullOrEmpty()) {
                Text(
                    text = parts.subject!!,
                    style = typography.monoSmall,
                    color = colors.inkMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
            }
            Spacer(modifier = Modifier.weight(1f))
            if (!parts.time.isNullOrEmpty()) {
                Text(
                    text = parts.time!!,
                    style = typography.monoSmall,
                    color = colors.inkMuted,
                    maxLines = 1,
                )
            }
        }
        ActivityHairlineRule()
    }
}

@Composable
private fun ActivityHairlineRule() {
    val colors = AmuxTheme.colors
    val reduced = Motion.isReducedMotion()
    val progress: Float = if (reduced) {
        0.5f
    } else {
        val transition = rememberInfiniteTransition(label = "activityHairline")
        val animated by transition.animateFloat(
            initialValue = 0f,
            targetValue = 1f,
            animationSpec = infiniteRepeatable(
                animation = tween(durationMillis = 1650, easing = LinearEasing),
                repeatMode = RepeatMode.Reverse,
            ),
            label = "activityLeg",
        )
        animated
    }

    val (startFrac, endFrac) = remember(progress, reduced) {
        if (reduced) {
            0.29f to 0.71f
        } else {
            val leading = smootherstep(((progress - 0.04f) / 0.74f).coerceIn(0f, 1f))
            val trailing = smootherstep(((progress - 0.22f) / 0.74f).coerceIn(0f, 1f))
            (0.87f * trailing) to (0.13f + 0.87f * leading)
        }
    }

    val ink = colors.inkMuted.copy(alpha = 0.7f)
    Canvas(
        modifier = Modifier
            .fillMaxWidth()
            .height(1.5.dp),
    ) {
        val x = size.width * startFrac
        val width = (size.width * (endFrac - startFrac)).coerceAtLeast(size.height)
        drawRoundRect(
            color = ink,
            topLeft = Offset(x, 0f),
            size = Size(width, size.height),
            cornerRadius = CornerRadius(size.height / 2f, size.height / 2f),
        )
    }
}

private fun smootherstep(t: Float): Float =
    t * t * t * (t * (t * 6f - 15f) + 10f)
