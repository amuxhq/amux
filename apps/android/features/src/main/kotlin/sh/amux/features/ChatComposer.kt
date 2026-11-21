package sh.amux.features

import androidx.compose.foundation.layout.width
import androidx.compose.foundation.indication
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.automirrored.outlined.CallSplit
import androidx.compose.material.icons.outlined.ArrowCircleUp
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.RadioButtonChecked
import androidx.compose.material.icons.outlined.RadioButtonUnchecked
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.ripple
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.stateDescription
import sh.amux.values.AgentKey
import sh.amux.values.Attention
import sh.amux.values.FleetCard
import sh.amux.values.TaskMark
import sh.amux.values.TasksView
import sh.amux.design.IconSize
import sh.amux.design.AmuxShapes
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
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.input.TextFieldLineLimits
import androidx.compose.foundation.text.input.InputTransformation
import androidx.compose.foundation.text.input.TextFieldBuffer
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.text.TextRange
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
import androidx.compose.runtime.Stable
import androidx.compose.runtime.State
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
import sh.amux.design.ThumbTarget
import sh.amux.design.spillTarget
import androidx.compose.material.icons.filled.Stop
import androidx.compose.foundation.layout.heightIn
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
import sh.amux.words.ChatNotices
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
    const val DICTATION_SETTINGS: String = "chat.dictationSettings"
    const val ACTIVITY: String = "chat.activity"
    const val EXITED_HEAD: String = "chat.exited"
    const val SWITCHED_AWAY: String = "chat.switchedAway"
    const val SWITCH_BACK: String = "chat.switchBack"
    const val SIGN_IN_NOTICE: String = "chat.foot.sign-in"
    const val NOTICE: String = "chat.notice"
    const val DOCK: String = "chat.dock"
    const val TASKS: String = "chat.tasks"
    const val BACKGROUND: String = "chat.background"
    const val SLASH_POPUP: String = "chat.slash.popup"
    const val ATTACHMENTS_ROW: String = "chat.attachments"
    const val ATTACHING: String = "chat.attaching"
    const val MODEL_CHIP: String = "chat.model"
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

    fun task(index: Int): String = "chat.tasks.$index"
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
    /** How many of the composer's edits the model had taken when it wrote [draft]. */
    val draftEdits: Int = 0,
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
    /** The agent's task list, which heads the dock above the composer. */
    val tasks: TasksView? = null,
    /** The agents this one started, listed when the dock's head opens. */
    val children: List<FleetCard> = emptyList(),
    /** How many of the agent's jobs still run in the background. */
    val backgroundJobs: Int = 0,
    /** Whether the dock's head starts open. */
    val dockOpen: Boolean = false,
    val slashMatches: List<CommandMatch> = emptyList(),
    val attachments: List<ComposerAttachmentUi> = emptyList(),
    val uploadingCount: Int = 0,
    val controls: ControlsSummary? = null,
    val context: ContextView? = null,
    val settingsAvailable: Boolean = true,
    val usageLimitMessage: String? = null,
    val dictationEnabled: Boolean = false,
    val dictationActive: Boolean = false,
    val dictationSentence: String? = null,
    val dictationDenied: Boolean = false,
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
    val onDraftChange: (String, FieldEdit?) -> Unit = { _, _ -> },
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
    val onOpenAgent: (AgentKey) -> Unit = {},
    /**
     * Where the dock's unfolded state is kept, when the caller keeps it:
     * then [ChatComposerState.dockOpen] is the state, and it outlives the
     * composer leaving the screen while an ask is docked. Without it the
     * dock keeps its own, starting from [ChatComposerState.dockOpen].
     */
    val onDockOpenChange: ((Boolean) -> Unit)? = null,
    val onToggleDictation: () -> Unit = {},
    val onOpenDictationSettings: () -> Unit = {},
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
    clock: ElapsedClock = ElapsedClock.Still,
) {
    val nativeMode = state.composer.mode as? Composer.Native

    Column(
        modifier = modifier
            .fillMaxWidth()
            .testTag(ChatComposerTags.ROOT),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        // A sign-in problem stands above a composer that still sends:
        // sending is how someone who signed in elsewhere tries again. A
        // provider subagent takes no messages, so it has no notice.
        if (state.signIn != null && nativeMode == null) {
            SignInNoticeCard(
                signIn = state.signIn,
                kind = state.kind,
                host = state.host,
            )
        }

        ChatDockCard(state = state, callbacks = callbacks)

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
                clock = clock,
                onStop = callbacks.onStopNative,
            )
        } else {
            StandardComposerCard(
                state = state,
                callbacks = callbacks,
                clock = clock,
            )
        }
    }
}

@Composable
internal fun SwitchedAwayBanner(onSwitchBack: () -> Unit) {
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
                    .size(IconSize.Control),
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

/**
 * What docks above the composer, in one card with hairlines between its
 * parts: the agent's task list as one row that opens in place to the list
 * and the agents this one started, what still runs in the background, then
 * the queued prompts, this phone's prompts not yet in the chat, and the ones
 * refused. The card goes when every part is empty.
 */
@Composable
private fun ChatDockCard(
    state: ChatComposerState,
    callbacks: ChatComposerCallbacks,
) {
    val tasks = state.tasks?.takeIf { it.total > 0 || it.entries.isNotEmpty() }
    val children = state.children
    val head = tasks != null || children.isNotEmpty()
    val jobs = state.backgroundJobs
    if (!head && jobs == 0 && state.queued.isEmpty() && state.underway.isEmpty() && state.refused.isEmpty()) {
        return
    }
    var ownExpanded by rememberSaveable { mutableStateOf(state.dockOpen) }
    val hoisted = callbacks.onDockOpenChange
    val expanded = if (hoisted != null) state.dockOpen else ownExpanded
    val toggleExpanded = {
        if (hoisted != null) hoisted(!state.dockOpen) else ownExpanded = !ownExpanded
    }

    AmuxChrome(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(ChatComposerTags.DOCK),
    ) {
        Column(modifier = Modifier.fillMaxWidth()) {
            var shownAny = false
            fun rule(): Boolean {
                val draw = shownAny
                shownAny = true
                return draw
            }

            if (head) {
                if (expanded) {
                    if (tasks != null && tasks.entries.isNotEmpty()) {
                        rule()
                        DockTaskList(tasks)
                        AmuxHairline()
                    }
                    if (children.isNotEmpty()) {
                        rule()
                        DockStarted(children = children, onOpen = callbacks.onOpenAgent)
                        AmuxHairline()
                    }
                }
                rule()
                DockHead(
                    tasks = tasks,
                    started = children.size,
                    expanded = expanded,
                    onToggle = toggleExpanded,
                )
            }

            if (jobs > 0) {
                if (rule()) AmuxHairline()
                DockBackground(count = jobs, onOpen = callbacks.onOpenOverview)
            }

            state.queued.forEachIndexed { index, row ->
                if (rule()) AmuxHairline()
                val preview = remember(row.text) { ChatWords.text(row.text) }
                val stateWord = remember(row) { ChatWords.queued(row) }
                DockTrayRow(
                    text = preview,
                    state = stateWord,
                    warn = false,
                    icon = Icons.Outlined.Schedule,
                    modifier = Modifier.testTag(ChatComposerTags.queuedRow(index)),
                ) {
                    if (row.canSendNow) {
                        AmuxEdgeIconButton(
                            icon = Icons.Outlined.ArrowCircleUp,
                            contentDescription = stringResource(R.string.chat_composer_send_now),
                            onClick = { callbacks.onSendNow(row) },
                            modifier = Modifier.testTag(ChatComposerTags.queuedSendNow(index)),
                        )
                    }
                    if (row.canWithdraw) {
                        AmuxEdgeIconButton(
                            icon = Icons.Outlined.Edit,
                            contentDescription = stringResource(R.string.chat_composer_withdraw),
                            onClick = { callbacks.onWithdraw(row) },
                            modifier = Modifier.testTag(ChatComposerTags.queuedWithdraw(index)),
                        )
                    }
                }
            }

            state.underway.forEachIndexed { index, prompt ->
                if (rule()) AmuxHairline()
                val preview = remember(prompt.text) { ChatWords.text(prompt.text) }
                val lost = prompt.underway is Underway.MayNotHaveArrived
                val statusWord = remember(prompt.underway, state.host) {
                    ChatWords.underway(prompt.underway, host = state.host)
                }
                DockTrayRow(
                    text = preview,
                    state = statusWord,
                    warn = lost,
                    icon = if (lost) Icons.Outlined.ErrorOutline else Icons.AutoMirrored.Outlined.Send,
                    modifier = Modifier.testTag(ChatComposerTags.underwayRow(index)),
                ) {
                    if (lost) {
                        AmuxEdgeIconButton(
                            icon = Icons.Outlined.Refresh,
                            contentDescription = stringResource(R.string.chat_composer_resend),
                            onClick = { callbacks.onResend(prompt.inputId) },
                            modifier = Modifier.testTag(ChatComposerTags.underwayResend(index)),
                        )
                        AmuxEdgeIconButton(
                            icon = Icons.Outlined.Close,
                            contentDescription = stringResource(R.string.chat_composer_discard),
                            onClick = { callbacks.onDiscard(prompt.inputId) },
                            modifier = Modifier.testTag(ChatComposerTags.underwayDiscard(index)),
                        )
                    }
                }
            }

            state.refused.forEachIndexed { index, prompt ->
                if (rule()) AmuxHairline()
                val reasonText = refusalMessage(prompt.reason)
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = DockRowHeight)
                        .padding(horizontal = Space.l, vertical = Space.s)
                        .testTag(ChatComposerTags.refusedRow(index)),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.m),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.ErrorOutline,
                        contentDescription = null,
                        tint = AmuxTheme.colors.removed,
                        modifier = Modifier.size(DockGlyph),
                    )
                    Text(
                        text = reasonText,
                        style = AmuxTheme.typography.detail,
                        color = AmuxTheme.colors.removed,
                        modifier = Modifier.weight(1f),
                    )
                    AmuxEdgeIconButton(
                        icon = Icons.Outlined.Close,
                        contentDescription = stringResource(R.string.chat_composer_dismiss),
                        onClick = { callbacks.onDismissRefused(prompt.inputId) },
                        modifier = Modifier.testTag(ChatComposerTags.refusedDismiss(index)),
                    )
                }
            }
        }
    }
}

/** The dock's head: the task in hand and how far the list has got, or how many agents it started. */
@Composable
private fun DockHead(
    tasks: TasksView?,
    started: Int,
    expanded: Boolean,
    onToggle: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val title = remember(tasks, started) { tasks?.let(ChatWords::headTask) ?: ChatWords.started(started) }
    val progress = tasks?.let { stringResource(R.string.chat_dock_progress, it.done, it.total) }
    val label = progress?.let { "$it $title" } ?: title
    val spoken = if (started > 0) "$label, ${ChatWords.started(started)}" else label
    val expandedWord = stringResource(R.string.state_expanded)
    val collapsedWord = stringResource(R.string.state_collapsed)
    Row(
        modifier = Modifier
            .testTag(ChatComposerTags.TASKS)
            .fillMaxWidth()
            .heightIn(min = DockHeadHeight)
            .clickable(role = Role.Button, onClick = onToggle)
            .clearAndSetSemantics {
                role = Role.Button
                contentDescription = spoken
                stateDescription = if (expanded) expandedWord else collapsedWord
                onClick { onToggle(); true }
            }
            .padding(horizontal = Space.l),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
    ) {
        if (tasks != null) {
            Text(
                text = progress.orEmpty(),
                style = typography.monoSmall,
                color = colors.inkMuted,
            )
        }
        Text(
            text = title,
            style = typography.body,
            color = colors.ink,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        if (started > 0) {
            Row(
                modifier = Modifier
                    .height(26.dp)
                    .background(colors.sunken, AmuxShapes.Pill)
                    .padding(horizontal = Space.s + Space.xxs),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.xs),
            ) {
                Icon(
                    imageVector = Icons.AutoMirrored.Outlined.CallSplit,
                    contentDescription = null,
                    tint = colors.ink,
                    modifier = Modifier.size(IconSize.Small),
                )
                Text(
                    text = started.toString(),
                    style = typography.monoSmall,
                    color = colors.ink,
                )
            }
        }
        Icon(
            imageVector = if (expanded) Icons.Outlined.ExpandMore else Icons.Outlined.ExpandLess,
            contentDescription = null,
            tint = colors.inkMuted,
            modifier = Modifier.size(IconSize.Row),
        )
    }
}

@Composable
private fun DockTaskList(tasks: TasksView) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Space.l, vertical = Space.m + Space.xxs),
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        tasks.entries.forEachIndexed { index, entry ->
            val markWord = when (entry.mark) {
                TaskMark.Done -> stringResource(R.string.task_done)
                TaskMark.Current -> stringResource(R.string.task_current)
                TaskMark.Todo -> stringResource(R.string.task_todo)
            }
            Row(
                modifier = Modifier
                    .testTag(ChatComposerTags.task(index))
                    .fillMaxWidth()
                    .clearAndSetSemantics {
                        contentDescription = entry.subject
                        stateDescription = markWord
                    },
                verticalAlignment = Alignment.Top,
                horizontalArrangement = Arrangement.spacedBy(Space.m),
            ) {
                Box(
                    modifier = Modifier
                        .width(DockGlyph)
                        .heightIn(min = with(LocalDensity.current) { typography.body.lineHeight.toDp() }),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        imageVector = when (entry.mark) {
                            TaskMark.Done -> Icons.Outlined.Check
                            TaskMark.Current -> Icons.Outlined.RadioButtonChecked
                            TaskMark.Todo -> Icons.Outlined.RadioButtonUnchecked
                        },
                        contentDescription = null,
                        tint = if (entry.mark == TaskMark.Todo) colors.inkMuted else colors.inkFaint,
                        modifier = Modifier.size(IconSize.Small),
                    )
                }
                Text(
                    text = entry.subject,
                    style = if (entry.mark == TaskMark.Current) typography.bodyEmphasis else typography.body,
                    color = if (entry.mark == TaskMark.Done) colors.inkFaint else colors.ink,
                    modifier = Modifier.weight(1f),
                )
            }
        }
    }
}

/** The agents this one started, each one tap from its own chat. */
@Composable
private fun DockStarted(
    children: List<FleetCard>,
    onOpen: (AgentKey) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    Column(modifier = Modifier.fillMaxWidth().padding(bottom = 6.dp)) {
        Text(
            text = stringResource(R.string.chat_dock_started).uppercase(),
            style = typography.sectionTitle,
            color = colors.inkFaint,
            modifier = Modifier
                .padding(start = Space.l, end = Space.l, top = Space.m + Space.xxs, bottom = Space.xs)
                .semantics { heading() },
        )
        children.forEach { child ->
            val needsYou = child.attention == Attention.NeedsYou || child.familyAttention == Attention.NeedsYou
            val stateText = remember(child) { ChatWords.childState(child) }
            Row(
                modifier = Modifier
                    .testTag(ChatScreenTags.familyChild(child.name))
                    .fillMaxWidth()
                    .heightIn(min = 54.dp)
                    .clickable(role = Role.Button) { onOpen(child.agent) }
                    .semantics(mergeDescendants = true) {}
                    .padding(horizontal = Space.l),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
            ) {
                Box(modifier = Modifier.width(DockGlyph), contentAlignment = Alignment.Center) {
                    if (needsYou) AmuxAttentionDot(size = 8.dp)
                }
                Column(
                    modifier = Modifier.weight(1f),
                    verticalArrangement = Arrangement.spacedBy(Space.xxs),
                ) {
                    Text(
                        text = child.name,
                        style = typography.identifier,
                        color = colors.ink,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        text = stateText,
                        style = typography.monoSmall,
                        color = if (child.attention == Attention.NeedsYou) colors.accent else colors.inkFaint,
                        maxLines = 1,
                    )
                }
                AmuxDisclosureChevron()
            }
        }
    }
}

/** What runs on after a turn, counted; the overview names each one. */
@Composable
private fun DockBackground(count: Int, onOpen: () -> Unit) {
    val colors = AmuxTheme.colors
    val words = ChatWords.inBackground(count)
    val hint = stringResource(R.string.chat_dock_background_hint)
    Row(
        modifier = Modifier
            .testTag(ChatComposerTags.BACKGROUND)
            .fillMaxWidth()
            .heightIn(min = DockHeadHeight)
            .clickable(role = Role.Button, onClickLabel = hint, onClick = onOpen)
            .semantics(mergeDescendants = true) {}
            .padding(horizontal = Space.l),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
    ) {
        Icon(
            imageVector = Icons.Outlined.Settings,
            contentDescription = null,
            tint = colors.inkMuted,
            modifier = Modifier.size(DockGlyph),
        )
        Text(
            text = words,
            style = AmuxTheme.typography.body,
            color = colors.ink,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        AmuxDisclosureChevron()
    }
}

/** A prompt on one line after its glyph, its state under it, and its acts as glyphs at the row's end. */
@Composable
private fun DockTrayRow(
    text: String,
    state: String,
    warn: Boolean,
    icon: ImageVector,
    modifier: Modifier = Modifier,
    actions: @Composable () -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    Row(
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = DockRowHeight)
            .padding(horizontal = Space.l, vertical = Space.s),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            tint = if (warn) colors.accent else colors.inkMuted,
            modifier = Modifier.size(DockGlyph),
        )
        Column(
            modifier = Modifier
                .weight(1f)
                .semantics(mergeDescendants = true) {},
            verticalArrangement = Arrangement.spacedBy(Space.xxs),
        ) {
            Text(
                text = text,
                style = typography.body,
                color = colors.ink,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = state,
                style = typography.caption,
                color = if (warn) colors.accent else colors.inkFaint,
                maxLines = 2,
            )
        }
        // Two glyphs side by side keep their 48 dp targets apart.
        Row(
            horizontalArrangement = Arrangement.spacedBy(AmuxEdgeSpill * 2),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            actions()
        }
    }
}

/** The dock's rows: a head or background row, and a prompt row with its state under it. */
private val DockHeadHeight = 50.dp
private val DockRowHeight = 52.dp

/** The column the dock's leading glyphs sit in. */
private val DockGlyph = 18.dp

/** "Not sent · why", in the words every refusal in the chat uses. */
@Composable
private fun refusalMessage(refusal: Refusal): String =
    stringResource(R.string.chat_composer_refused_prefix, ChatNotices.refusal(refusal))

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
                        .heightIn(min = ThumbTarget.MinSize)
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
    clock: ElapsedClock,
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

    // At the largest text sizes the model's words get a row of their own:
    // beside the buttons there is room for only a few letters.
    val chipOwnRow = LocalDensity.current.fontScale >= LARGE_TEXT_SCALE

    AmuxChrome(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = Space.l, end = Space.l, top = Space.m + Space.xxs, bottom = Space.m),
            verticalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
        ) {
            val activity = state.composer.activity
            val exitedCause = (state.phase as? PhaseView.Exited)?.cause
            if (activity != null) {
                ActivityLineView(
                    activity = activity,
                    subject = state.activitySubject,
                    subagents = state.liveSubagents,
                    clock = clock,
                )
            } else if (exitedCause != null) {
                ExitedHeadLine(
                    cause = exitedCause,
                    settled = state.settled,
                    host = state.host,
                )
            }

            if (!state.dictationSentence.isNullOrEmpty()) {
                // The message field's touch area grows up from its text, so
                // the Settings button keeps clear of it and grows up instead.
                Column(
                    modifier = Modifier.padding(bottom = if (state.dictationDenied) Space.l else 0.dp),
                    verticalArrangement = Arrangement.spacedBy(Space.xxs),
                ) {
                    Text(
                        text = state.dictationSentence,
                        style = typography.caption,
                        color = colors.inkMuted,
                    )
                    // Refused once, Android stops asking; only its settings
                    // can allow the microphone again.
                    if (state.dictationDenied) {
                        Box(
                            modifier = Modifier
                                .spillTarget(spill = Alignment.Bottom)
                                .clickable(role = Role.Button, onClick = callbacks.onOpenDictationSettings)
                                .testTag(ChatComposerTags.DICTATION_SETTINGS),
                        ) {
                            Text(
                                text = stringResource(R.string.chat_composer_dictation_settings),
                                style = typography.caption,
                                color = colors.accent,
                            )
                        }
                    }
                }
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

            val field = rememberDraftField(
                draft = state.draft,
                draftEdits = state.draftEdits,
                onDraftChange = callbacks.onDraftChange,
            )
            BasicTextField(
                state = field.state,
                inputTransformation = field.typing,
                textStyle = typography.body.copy(color = colors.ink),
                cursorBrush = SolidColor(colors.accent),
                lineLimits = TextFieldLineLimits.MultiLine(maxHeightInLines = 8),
                // The field is drawn its text's height, so the card does not
                // grow around a one-line draft; its touch area still reaches
                // 48 dp, growing up into the card's padding only, since the
                // controls' targets sit close below it.
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(vertical = Space.xxs)
                    .spillTarget(spill = Alignment.Bottom)
                    .semantics { contentDescription = placeholder }
                    .testTag(ChatComposerTags.INPUT_FIELD),
                decorator = { innerTextField ->
                    // The target grows upward from the text, so the text
                    // keeps to the bottom of it, where the layout put it.
                    Box(
                        modifier = Modifier.fillMaxWidth(),
                        contentAlignment = Alignment.BottomStart,
                    ) {
                        if (field.state.text.isEmpty()) {
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

            val warnsPermission = state.controls?.permission?.neverAsks == true
            if (chipOwnRow && controlsChip != null) {
                ModelChip(
                    chip = controlsChip,
                    warnsPermission = warnsPermission,
                    enabled = state.settingsAvailable,
                    wraps = true,
                    onClick = callbacks.onOpenSettings,
                )
            }

            // Wide enough apart that the plus disc's target, which spills
            // past its drawn edge, stops short of the chip's.
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                Box {
                    val attachLabel = stringResource(R.string.chat_composer_attach)
                    // The disc's edge sits on the card's text column; its
                    // 48 dp target spills into the card's padding.
                    AmuxEdgeTarget(
                        footprint = ComposerDisc,
                        onClick = { attachMenuOpen = !attachMenuOpen },
                        contentDescription = attachLabel,
                        modifier = Modifier.testTag(ChatComposerTags.ATTACH_BUTTON),
                    ) {
                        Box(
                            modifier = Modifier
                                .size(ComposerDisc)
                                .clip(CircleShape)
                                .background(colors.sunken),
                            contentAlignment = Alignment.Center,
                        ) {
                            Icon(
                                imageVector = Icons.Outlined.Add,
                                contentDescription = null,
                                tint = colors.ink,
                                modifier = Modifier.size(IconSize.Control),
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
                            modifier = Modifier.testTag(ChatComposerTags.ATTACH_MENU_PHOTO),
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
                            modifier = Modifier.testTag(ChatComposerTags.ATTACH_MENU_FILE),
                        )
                    }
                }

                // The chip may take all the room between the plus and the
                // controls at the end, and shortens only when that runs out.
                Box(modifier = Modifier.weight(1f), contentAlignment = Alignment.CenterStart) {
                    if (!chipOwnRow && controlsChip != null) {
                        ModelChip(
                            chip = controlsChip,
                            warnsPermission = warnsPermission,
                            enabled = state.settingsAvailable,
                            wraps = false,
                            onClick = callbacks.onOpenSettings,
                        )
                    }
                }

                if (state.dictationEnabled) {
                    val dictateLabel = stringResource(
                        if (state.dictationActive) {
                            R.string.chat_composer_stop_dictation
                        } else {
                            R.string.chat_composer_dictate
                        },
                    )
                    // Drawn by its glyph so the controls row stays the
                    // disc's height; the gap keeps its 48 dp target clear of
                    // the send disc's.
                    AmuxEdgeIconButton(
                        icon = if (state.dictationActive) Icons.Outlined.StopCircle else Icons.Outlined.Mic,
                        contentDescription = dictateLabel,
                        onClick = callbacks.onToggleDictation,
                        tint = if (state.dictationActive) colors.accent else colors.inkMuted,
                        modifier = Modifier.testTag(ChatComposerTags.DICTATE_BUTTON),
                    )
                    // Enough room that the glyph's 48 dp target and the
                    // disc's meet without overlapping, as iOS spaces them.
                    Spacer(modifier = Modifier.width(Space.m + Space.xxs))
                }

                ComposerPrimaryControl(
                    state = state,
                    callbacks = callbacks,
                )
            }
        }
    }
}

/**
 * The model and its effort, then a permission or mode other than the normal
 * one, as one borderless capsule. A permission that stops asking reads in
 * the failure red. A tap opens the agent's settings.
 *
 * Beside the buttons the words stay on one line and shorten at their end;
 * on a row of their own ([wraps]) they wrap whole, in a rounded box, since a
 * capsule's ends would cut into a second line.
 */
@Composable
private fun ModelChip(
    chip: ChatWords.ControlsChip,
    warnsPermission: Boolean,
    enabled: Boolean,
    wraps: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val annotated = remember(chip, warnsPermission, colors, wraps) {
        // On a row of its own a no-break space holds each "·" to the word
        // before it, so a wrapped line never starts with one.
        val joint = if (wraps) "\u00A0· " else " · "
        buildAnnotatedString {
            var led = false
            if (chip.model.isNotEmpty()) {
                withStyle(SpanStyle(color = colors.inkMuted)) { append(chip.model) }
                led = true
            }
            if (chip.detail.isNotEmpty()) {
                withStyle(SpanStyle(color = colors.inkMuted)) {
                    if (led) append(joint)
                    append(chip.detail)
                }
                led = true
            }
            chip.unusual.forEachIndexed { index, word ->
                val isWarning = warnsPermission && index == 0
                withStyle(SpanStyle(color = if (isWarning) colors.removed else colors.ink)) {
                    if (led) append(joint)
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
    val shape = if (wraps) RoundedCornerShape(15.dp) else AmuxShapes.Pill
    ComposerTapTarget(
        onClick = onClick,
        enabled = enabled,
        contentDescription = settingsDescription,
        shape = shape,
        modifier = modifier.testTag(ChatComposerTags.MODEL_CHIP),
    ) {
        Row(
            modifier = Modifier
                .heightIn(min = ModelChipHeight)
                .background(colors.sunken, shape)
                .padding(horizontal = 11.dp, vertical = if (wraps) 6.dp else 0.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(5.dp),
        ) {
            Text(
                text = annotated,
                style = typography.caption,
                maxLines = if (wraps) Int.MAX_VALUE else 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
            Icon(
                imageVector = Icons.Outlined.ExpandMore,
                contentDescription = null,
                tint = colors.inkFaint,
                modifier = Modifier.size(IconSize.Small),
            )
        }
    }
}

/**
 * A small drawn control in the composer's button row: it takes only its own
 * room in the row, so the row stays as tall as its discs, while its touch
 * target reaches 48 dp around it. The press ripple draws on the control's
 * own shape rather than the larger target.
 */
@Composable
private fun ComposerTapTarget(
    onClick: () -> Unit,
    enabled: Boolean,
    contentDescription: String?,
    shape: Shape,
    modifier: Modifier = Modifier,
    content: @Composable () -> Unit,
) {
    val interaction = remember { MutableInteractionSource() }
    Box(
        modifier = modifier
            .spillTarget()
            .clickable(
                interactionSource = interaction,
                indication = null,
                enabled = enabled,
                role = Role.Button,
                onClick = onClick,
            )
            .then(
                if (contentDescription != null) {
                    Modifier.semantics { this.contentDescription = contentDescription }
                } else {
                    Modifier
                },
            ),
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier = Modifier
                .clip(shape)
                .indication(interaction, ripple()),
        ) {
            content()
        }
    }
}

/** The model chip's drawn height, as iOS draws it. */
private val ModelChipHeight = 30.dp

/** The font scale from which the model chip takes its own row. */
private const val LARGE_TEXT_SCALE = 1.3f

/** The composer's round attach, send and stop controls, as iOS draws them. */
private val ComposerDisc = 34.dp

@Composable
private fun ComposerPrimaryControl(
    state: ChatComposerState,
    callbacks: ChatComposerCallbacks,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography

    when {
        state.composer.mode is Composer.Resume -> {
            // Resume carries the draft as the agent's first prompt, so there
            // is nothing to resume with until something is written.
            val enabled = state.canResume && state.hasDraft
            val resumeLabel = stringResource(R.string.chat_composer_resume)
            ComposerTapTarget(
                onClick = callbacks.onResume,
                enabled = enabled,
                contentDescription = null,
                shape = AmuxShapes.Pill,
                modifier = Modifier.testTag(ChatComposerTags.RESUME_BUTTON),
            ) {
                Box(
                    modifier = Modifier
                        .height(ComposerDisc)
                        .background(if (enabled) colors.ink else colors.sunken, AmuxShapes.Pill)
                        .padding(horizontal = Space.l),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        text = resumeLabel,
                        style = typography.bodyEmphasis,
                        color = if (enabled) colors.ground else colors.inkFaint,
                        maxLines = 1,
                    )
                }
            }
        }
        !state.hasDraft && state.isWorking -> {
            val interruptLabel = stringResource(R.string.chat_composer_interrupt)
            AmuxEdgeTarget(
                footprint = ComposerDisc,
                onClick = callbacks.onInterrupt,
                contentDescription = interruptLabel,
                modifier = Modifier.testTag(ChatComposerTags.INTERRUPT_BUTTON),
            ) {
                Box(
                    modifier = Modifier
                        .size(ComposerDisc)
                        .clip(CircleShape)
                        .background(colors.ink),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        imageVector = Icons.Filled.Stop,
                        contentDescription = null,
                        tint = colors.ground,
                        modifier = Modifier.size(IconSize.Control),
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
            AmuxEdgeTarget(
                footprint = ComposerDisc,
                onClick = callbacks.onSend,
                contentDescription = sendLabel,
                enabled = enabled,
                modifier = Modifier.testTag(ChatComposerTags.SEND_BUTTON),
            ) {
                Box(
                    modifier = Modifier
                        .size(ComposerDisc)
                        .clip(CircleShape)
                        .background(if (enabled) colors.ink else colors.sunken),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        imageVector = Icons.Outlined.ArrowUpward,
                        contentDescription = null,
                        tint = if (enabled) colors.ground else colors.inkFaint,
                        modifier = Modifier.size(IconSize.Control),
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
    clock: ElapsedClock,
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
                    clock = clock,
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
                    modifier = Modifier.size(IconSize.Control),
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
            modifier = Modifier.size(IconSize.Inline),
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
    clock: ElapsedClock,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    // The one place the activity line reads the clock, so a tick redraws
    // this line and not the composer around it.
    val nowMs = clock.nowMs()
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
        verticalArrangement = Arrangement.spacedBy(7.dp),
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
            // The subject takes all the room the words and the clock leave,
            // and is cut in its middle, where a command or a path matters
            // least.
            val subject = parts.subject
            if (!subject.isNullOrEmpty()) {
                Text(
                    text = subject,
                    style = typography.monoSmall,
                    color = colors.inkMuted,
                    maxLines = 1,
                    overflow = TextOverflow.MiddleEllipsis,
                    modifier = Modifier.weight(1f),
                )
            } else {
                Spacer(modifier = Modifier.weight(1f))
            }
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
    // The animated value is read only inside the draw block, so each frame
    // redraws the rule without recomposing the composer around it.
    val progress: State<Float>? = if (reduced) {
        null
    } else {
        val transition = rememberInfiniteTransition(label = "activityHairline")
        transition.animateFloat(
            initialValue = 0f,
            targetValue = 1f,
            animationSpec = infiniteRepeatable(
                animation = tween(durationMillis = 1650, easing = LinearEasing),
                repeatMode = RepeatMode.Reverse,
            ),
            label = "activityLeg",
        )
    }

    val ink = colors.inkMuted.copy(alpha = 0.7f)
    Canvas(
        modifier = Modifier
            .fillMaxWidth()
            .height(AmuxTheme.metrics.hairline),
    ) {
        val startFrac: Float
        val endFrac: Float
        if (progress == null) {
            startFrac = 0.29f
            endFrac = 0.71f
        } else {
            val now = progress.value
            val leading = smootherstep(((now - 0.04f) / 0.74f).coerceIn(0f, 1f))
            val trailing = smootherstep(((now - 0.22f) / 0.74f).coerceIn(0f, 1f))
            startFrac = 0.87f * trailing
            endFrac = 0.13f + 0.87f * leading
        }
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

/** The composer's text field: its text, and the hook that hands each typed edit to the model. */
@Stable
internal class DraftField(
    val state: TextFieldState,
    val typing: InputTransformation,
)

/**
 * One edit as the field made it: the characters from [start] to [removedEnd]
 * of [original] became those from [start] to [insertedEnd] of the new text.
 * Only the field knows which of a run of repeated characters was typed or
 * deleted, and a paste chip beside them moves by that.
 */
@Immutable
data class FieldEdit(val original: String, val start: Int, val removedEnd: Int, val insertedEnd: Int)

/**
 * The span every change in this edit falls within, as one edit. The change
 * list is marked experimental; it is the only account of which characters an
 * edit touched, and without it a chip beside repeated characters moves to the
 * wrong side.
 */
@OptIn(ExperimentalFoundationApi::class)
private fun TextFieldBuffer.fieldEdit(): FieldEdit? {
    val count = changes.changeCount
    if (count == 0) return null
    var start = Int.MAX_VALUE
    var removedEnd = 0
    var insertedEnd = 0
    for (index in 0 until count) {
        val original = changes.getOriginalRange(index)
        val range = changes.getRange(index)
        start = minOf(start, original.min, range.min)
        removedEnd = maxOf(removedEnd, original.max)
        insertedEnd = maxOf(insertedEnd, range.max)
    }
    return FieldEdit(originalText.toString(), start, removedEnd, insertedEnd)
}

/**
 * The composer's text as the field holds it.
 *
 * The field owns its text, so typing is drawn at once and never waits on the
 * chat model. Every edit the person makes (typing, pasting, dictating into
 * the keyboard) passes through [DraftField.typing], which hands it to the
 * model through [onDraftChange], with the span the edit replaced, and counts
 * it. The model answers with its
 * draft and the number of edits it has taken, [draftEdits].
 *
 * An answer that has not yet caught up with the field's edits is ignored:
 * the field is ahead of it, and the answer to its latest edit is on its way.
 * An answer that has caught up is the truth, and when its text differs from
 * the field's, the model rewrote the draft (a long paste became a chip, a
 * command was inserted, a sent message cleared it, a kept draft or dictation
 * came back). The field takes the rewrite, keeping the caret as far from the
 * end as it was, so a paste chip lands where the caret stood. A rewrite is
 * written to the field directly, not as typing, so it is never handed back.
 *
 * The answer is followed by its count as well as its text: a paste that
 * becomes a chip leaves the model's text as it was before the paste, and
 * only the count says the model has answered it.
 */
@Composable
internal fun rememberDraftField(
    draft: String,
    draftEdits: Int,
    onDraftChange: (String, FieldEdit?) -> Unit,
): DraftField {
    val currentOnDraftChange by rememberUpdatedState(onDraftChange)
    // The edits this field has handed to the model, starting from the
    // model's own count so a field drawn again over a kept draft agrees.
    val handed = remember { intArrayOf(draftEdits) }
    val field = remember {
        DraftField(
            state = TextFieldState(initialText = draft, initialSelection = TextRange(draft.length)),
            typing = InputTransformation {
                if (asCharSequence().contentEquals(originalText)) return@InputTransformation
                handed[0] += 1
                currentOnDraftChange(asCharSequence().toString(), fieldEdit())
            },
        )
    }

    LaunchedEffect(draft, draftEdits) {
        if (draftEdits < handed[0]) return@LaunchedEffect
        handed[0] = draftEdits
        val text = field.state.text
        if (text.contentEquals(draft)) return@LaunchedEffect
        val fromEnd = text.length - field.state.selection.end
        field.state.edit {
            replace(0, length, draft)
            selection = TextRange((draft.length - fromEnd).coerceIn(0, draft.length))
        }
    }
    return field
}
