package sh.amux.shell

import sh.amux.core.runtime.DictationState

import sh.amux.core.runtime.ChatAsk
import sh.amux.core.runtime.ChatModel
import sh.amux.core.runtime.ChatPaging
import sh.amux.core.runtime.ChatUiState
import sh.amux.core.runtime.QuestionDraft as RuntimeQuestionDraft
import sh.amux.features.ChatOpenedAttachmentUi
import sh.amux.features.ChatPagingUi
import sh.amux.features.ChatRequestedOverlay
import sh.amux.features.ChatScreenState
import sh.amux.features.ChatSubject
import sh.amux.features.ComposerAttachmentUi
import sh.amux.features.QuestionDraft as UiQuestionDraft
import sh.amux.values.AgentKey
import sh.amux.values.AttachmentView
import sh.amux.values.ChatFrame
import sh.amux.values.DraftAttachment
import sh.amux.values.FamilyHeader
import sh.amux.values.Filing
import sh.amux.values.FleetRow
import sh.amux.values.FormValue
import sh.amux.values.HostView
import sh.amux.values.Reach
import sh.amux.values.RouteKind
import sh.amux.words.name

/** Pure mappers from a chat's model state and its fleet row to the chat screen's state. */
object ChatMappers {

    /**
     * Who the chat is with, from its own fleet row and, before the fleet has
     * one, from the computer it runs on: [host] and whether it is
     * [connecting].
     */
    fun toChatSubject(
        agent: AgentKey,
        fleetRow: FleetRow?,
        frame: ChatFrame?,
        host: HostView?,
        connecting: Boolean,
    ): ChatSubject {
        if (fleetRow != null) {
            val card = fleetRow.card
            return ChatSubject(
                name = frame?.name?.takeIf { it.isNotEmpty() } ?: fleetRow.name,
                host = card.host,
                directory = card.cwd,
                branch = card.branch,
                reach = card.hostReach,
                connecting = connecting,
                descendants = card.descendants,
                backgroundJobs = card.backgroundJobs,
                closeHeld = card.closeHeld,
                filing = card.filing,
                topLevel = card.topLevel,
                native = card.native,
                key = agent,
            )
        }
        val hostView = host
        return ChatSubject(
            name = frame?.name?.takeIf { it.isNotEmpty() } ?: agent.toString(),
            host = hostView?.name.orEmpty(),
            directory = "",
            branch = null,
            reach = hostView?.reach ?: Reach.Online(RouteKind.Direct),
            connecting = connecting,
            filing = Filing.Open,
            key = agent,
        )
    }

    fun toChatScreenState(
        chat: ChatUiState,
        subject: ChatSubject,
        family: FamilyHeader?,
        keptQuestionDraft: RuntimeQuestionDraft? = null,
        keptFormValues: List<FormValue>? = null,
        openedAttachment: ChatOpenedAttachmentUi? = null,
        dictationEnabled: Boolean = false,
    ): ChatScreenState {
        val composerAttachments = chat.attachments.mapIndexed { index, attachment ->
            when (attachment) {
                is DraftAttachment.Image -> ComposerAttachmentUi(
                    id = "img:$index:${attachment.value.hash.hashCode()}",
                    view = AttachmentView.Image(attachment.value),
                    thumbnailBytes = chat.draftImages[attachment.value.hash],
                )
                is DraftAttachment.File -> ComposerAttachmentUi(
                    id = "file:$index:${attachment.value.hash.hashCode()}",
                    view = AttachmentView.File(attachment.value),
                )
                is DraftAttachment.Text -> ComposerAttachmentUi(
                    id = "text:$index:${attachment.name}",
                    view = AttachmentView.Text(
                        name = attachment.name,
                        lines = ChatModel.lines(attachment.text),
                        text = attachment.text,
                    ),
                )
                is DraftAttachment.Page -> ComposerAttachmentUi(
                    id = "page:$index:${attachment.value.title}",
                    view = AttachmentView.Page(
                        title = attachment.value.title,
                        comments = attachment.value.comments.size,
                        images = attachment.value.comments.mapNotNull { it.image },
                    ),
                )
            }
        }

        val pagingUi = when (val paging = chat.paging) {
            ChatPaging.Idle -> ChatPagingUi.Idle
            ChatPaging.Fetching -> ChatPagingUi.Fetching
            ChatPaging.Unreachable -> ChatPagingUi.Unreachable
            is ChatPaging.Failed -> ChatPagingUi.Failed(paging.reason)
        }

        val requestedOverlay = when (chat.asks) {
            is ChatAsk.Settings -> ChatRequestedOverlay.Settings
            ChatAsk.Rename -> ChatRequestedOverlay.Rename
            null -> null
        }

        return ChatScreenState(
            subject = subject,
            family = family,
            keys = chat.keys,
            empty = chat.empty,
            frame = chat.frame,
            ask = chat.ask,
            overview = chat.overview,
            overviewChangesComparison = chat.overviewChangesComparison,
            overviewChangesLoading = chat.overviewChangesLoading,
            overviewChangesError = chat.overviewChangesError,
            settings = chat.settings,
            openedAgents = chat.openedAgents,
            liveSubagents = chat.liveSubagents,
            groupStates = chat.groupStates,
            expanded = chat.expanded,
            paging = pagingUi,
            loadingHint = chat.loadingHint,
            following = chat.following,
            toNewest = chat.toNewest,
            draft = chat.draft,
            draftEdits = chat.typedEdits,
            attachments = composerAttachments,
            uploadingCount = chat.uploading,
            sending = chat.sending,
            canSend = chat.canSend,
            canResume = chat.canResume,
            notice = chat.notice,
            comparison = chat.comparison,
            comparisons = chat.comparisons,
            changes = chat.changes,
            slashMatches = chat.slashMatches,
            activitySubject = chat.activitySubject,
            landingInFeed = chat.landingInFeed,
            landingInQueue = chat.landingInQueue,
            keptQuestionDraft = keptQuestionDraft?.toUi(),
            keptFormValues = keptFormValues,
            requestedOverlay = requestedOverlay,
            openedAttachment = openedAttachment,
            dictationEnabled = dictationEnabled,
            dictationActive = chat.dictation.active,
            dictationSentence = chat.dictation.sentence,
            dictationDenied = chat.dictation.phase == DictationState.Phase.Denied,
        )
    }

    fun toRuntimeQuestionDraft(ui: UiQuestionDraft): RuntimeQuestionDraft = RuntimeQuestionDraft(
        step = ui.step,
        picks = ui.picks,
        others = ui.others,
        highlighted = ui.highlighted,
        skipped = ui.skipped,
        notes = ui.notes,
        noting = ui.noting,
        reviewing = ui.reviewing,
        replying = ui.replying,
        reply = ui.replyText,
    )
    private fun RuntimeQuestionDraft.toUi(): UiQuestionDraft = UiQuestionDraft(
        step = step,
        picks = picks,
        others = others,
        highlighted = highlighted,
        skipped = skipped,
        notes = notes,
        noting = noting,
        reviewing = reviewing,
        replying = replying,
        replyText = reply,
    )

}
