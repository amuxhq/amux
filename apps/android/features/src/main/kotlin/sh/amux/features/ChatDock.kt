package sh.amux.features

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import java.time.Instant
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.values.CardState
import sh.amux.values.Composer
import sh.amux.values.ComposerView
import sh.amux.values.Filing
import sh.amux.values.PhaseView
import sh.amux.words.ChatWords

/**
 * Everything that stands under the transcript: a held settle, Claude's own
 * view switched away, and a notice of something that did not happen, then
 * either the open ask in the composer's place or the composer with its dock.
 *
 * The first three stay while an ask is docked, since they are exactly what
 * a person acting on the ask needs to see. The ask takes what height is
 * left under them; past it the ask's content scrolls over its pinned
 * buttons.
 */
@Composable
internal fun ChatBottomDock(
    state: ChatScreenState,
    callbacks: ChatScreenCallbacks,
    clock: ElapsedClock,
    onOpenOverview: () -> Unit,
    onOpenSettings: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val activeAsk = state.ask
    val frame = state.frame
    // Kept here rather than in the dock, which leaves the screen while an
    // ask is docked: an ask coming and going leaves the dock as it was.
    var dockOpen by rememberSaveable(state.subject.key) { mutableStateOf(state.dockOpen) }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = Space.m, end = Space.m, top = Space.xs, bottom = Space.s + Space.xxs),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        if (state.subject.closeHeld) {
            HeldStopBanner(
                host = state.subject.host,
                onCancelStop = callbacks.onCancelSettle,
            )
        }

        if (frame?.composer?.switchedAway == true) {
            SwitchedAwayBanner(onSwitchBack = callbacks.onSwitchBack)
        }

        val notice = state.notice
        if (!notice.isNullOrEmpty()) {
            // Every notice is something that did not happen, so it reads in
            // the failure ink rather than the accent kept for asks.
            Text(
                text = notice,
                style = AmuxTheme.typography.detail,
                color = colors.removed,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 6.dp)
                    .testTag(ChatComposerTags.NOTICE),
            )
        }

        if (activeAsk != null && activeAsk.state !is CardState.Dismissed) {
            AskCardView(
                card = activeAsk,
                onAction = callbacks.onAskAction,
                keptQuestionDraft = state.keptQuestionDraft,
                onQuestionDraftChange = { draft -> callbacks.onQuestionDraftChange(activeAsk.key, draft) },
                keptFormValues = state.keptFormValues,
                onFormValuesChange = { values -> callbacks.onFormValuesChange(activeAsk.key, values) },
                validateForm = callbacks.validateForm,
                onInterrupt = callbacks.onInterrupt,
                onCopyCode = callbacks.onCopyCode,
            )
            return@Column
        }

        if (activeAsk != null) {
            AskCardView(
                card = activeAsk,
                onAction = callbacks.onAskAction,
                keptQuestionDraft = state.keptQuestionDraft,
                onQuestionDraftChange = { draft -> callbacks.onQuestionDraftChange(activeAsk.key, draft) },
                keptFormValues = state.keptFormValues,
                onFormValuesChange = { values -> callbacks.onFormValuesChange(activeAsk.key, values) },
                validateForm = callbacks.validateForm,
                onInterrupt = callbacks.onInterrupt,
                onCopyCode = callbacks.onCopyCode,
            )
        }

        // The clock is read only while a limit is reached, the one time the
        // placeholder counts down; otherwise a tick leaves the dock alone.
        val usage = state.overview?.usageNearLimit
        val usageLimitMessage = if (usage != null && usage.blocked) {
            ChatWords.limitReached(usage, now = Instant.ofEpochMilli(clock.nowMs()))
        } else {
            null
        }

        val composerView = frame?.composer ?: ComposerView(
            mode = Composer.Send,
            switchedAway = false,
            activity = null,
        )
        val nativeMode = composerView.mode is Composer.Native

        ChatComposer(
            state = ChatComposerState(
                composer = composerView,
                draft = state.draft,
                draftEdits = state.draftEdits,
                host = state.subject.host,
                reach = state.subject.reach,
                connecting = state.subject.connecting,
                phase = frame?.phase ?: PhaseView.Idle,
                kind = frame?.kind,
                settled = state.subject.filing == Filing.Closed,
                canSend = state.canSend,
                canResume = state.canResume,
                activitySubject = state.activitySubject,
                liveSubagents = state.liveSubagents,
                signIn = frame?.signIn,
                queued = frame?.queue.orEmpty(),
                underway = state.landingInQueue,
                refused = frame?.refused.orEmpty(),
                // A provider subagent's chat has no dock: its parent's model
                // runs it, and the parent's chat holds what it started.
                tasks = if (nativeMode) null else state.overview?.tasks,
                children = if (nativeMode) emptyList() else state.family?.children.orEmpty(),
                backgroundJobs = if (nativeMode) 0 else state.overview?.jobs?.size ?: 0,
                dockOpen = dockOpen,
                slashMatches = state.slashMatches,
                attachments = state.attachments,
                uploadingCount = state.uploadingCount,
                controls = frame?.controls,
                context = frame?.context,
                settingsAvailable = state.settings != null,
                usageLimitMessage = usageLimitMessage,
                dictationEnabled = state.dictationEnabled,
                dictationActive = state.dictationActive,
                dictationSentence = state.dictationSentence,
                dictationDenied = state.dictationDenied,
            ),
            callbacks = ChatComposerCallbacks(
                onDraftChange = callbacks.onDraftChange,
                onSend = callbacks.onSend,
                onResume = callbacks.onResume,
                onInterrupt = callbacks.onInterrupt,
                onAttach = callbacks.onAttach,
                onRemoveAttachment = callbacks.onRemoveAttachment,
                onPickCommand = callbacks.onPickCommand,
                onSendNow = callbacks.onSendNow,
                onWithdraw = callbacks.onWithdraw,
                onResend = callbacks.onResend,
                onDiscard = callbacks.onDiscard,
                onDismissRefused = callbacks.onDismissRefused,
                onSwitchBack = callbacks.onSwitchBack,
                onStopNative = callbacks.onStopJob,
                onOpenSettings = onOpenSettings,
                onOpenOverview = onOpenOverview,
                onOpenAgent = callbacks.onOpenAgent,
                onDockOpenChange = { dockOpen = it },
                onToggleDictation = callbacks.onToggleDictation,
                onOpenDictationSettings = callbacks.onOpenDictationSettings,
            ),
            clock = clock,
        )
    }
}
