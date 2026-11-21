package sh.amux.features

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.key
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import java.time.Instant
import sh.amux.design.Space
import sh.amux.values.CardState
import sh.amux.values.Composer
import sh.amux.values.ComposerView
import sh.amux.values.Filing
import sh.amux.values.PhaseView
import sh.amux.words.ChatWords

@Composable
internal fun ChatBottomDock(
    state: ChatScreenState,
    callbacks: ChatScreenCallbacks,
    clock: ElapsedClock,
    onOpenOverview: () -> Unit,
    onOpenSettings: () -> Unit,
) {
    val activeAsk = state.ask
    if (activeAsk != null && activeAsk.state !is CardState.Dismissed) {
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.m, vertical = Space.s)
                .heightIn(max = 520.dp),
        ) {
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
        return
    }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        if (activeAsk != null) {
            Box(modifier = Modifier.padding(horizontal = Space.m)) {
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
        }

        val nativeMode = state.frame?.composer?.mode is Composer.Native
        if (!nativeMode) {
            ChatOverviewStrip(
                overview = state.overview,
                context = state.frame?.context,
                changeTotals = state.changes,
                clock = clock,
                onOpenOverview = onOpenOverview,
                onOpenReview = if (state.changes != null) {
                    { callbacks.onOpenReview(null) }
                } else {
                    null
                },
                modifier = Modifier.padding(horizontal = Space.m),
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

        val composerView = state.frame?.composer ?: ComposerView(
            mode = Composer.Send,
            switchedAway = false,
            activity = null,
        )

        ChatComposer(
            state = ChatComposerState(
                composer = composerView,
                draft = state.draft,
                draftEdits = state.draftEdits,
                host = state.subject.host,
                reach = state.subject.reach,
                connecting = state.subject.connecting,
                phase = state.frame?.phase ?: PhaseView.Idle,
                kind = state.frame?.kind,
                settled = state.subject.filing == Filing.Closed,
                canSend = state.canSend,
                canResume = state.canResume,
                activitySubject = state.activitySubject,
                liveSubagents = state.liveSubagents,
                signIn = state.frame?.signIn,
                queued = state.frame?.queue.orEmpty(),
                underway = state.landingInQueue,
                refused = state.frame?.refused.orEmpty(),
                slashMatches = state.slashMatches,
                attachments = state.attachments,
                uploadingCount = state.uploadingCount,
                notice = state.notice,
                controls = state.frame?.controls,
                context = state.frame?.context,
                settingsAvailable = state.settings != null,
                showOverviewChip = state.hasOverviewHighlights,
                usageLimitMessage = usageLimitMessage,
                dictationEnabled = state.dictationEnabled,
                dictationActive = state.dictationActive,
                dictationSentence = state.dictationSentence,
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
                onToggleDictation = callbacks.onToggleDictation,
            ),
            clock = clock,
        )
    }
}
