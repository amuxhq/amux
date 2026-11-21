package sh.amux.shell

import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import sh.amux.core.runtime.AccountRegistryState
import sh.amux.core.runtime.ChatAsk
import sh.amux.core.runtime.ChatModel
import sh.amux.core.runtime.ChatPaging
import sh.amux.core.runtime.ChatUiState
import sh.amux.core.runtime.FleetState
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.HostsState
import sh.amux.core.runtime.HostsStore
import sh.amux.core.runtime.PairingInvitation
import sh.amux.core.runtime.PairingState
import sh.amux.core.runtime.PairingStore
import sh.amux.core.runtime.QuestionDraft as RuntimeQuestionDraft
import sh.amux.features.AccountUi
import sh.amux.features.AgentsHomeState
import sh.amux.features.ChatOpenedAttachmentUi
import sh.amux.features.ChatPagingUi
import sh.amux.features.ChatRequestedOverlay
import sh.amux.features.ChatRowCellUi
import sh.amux.features.ChatRowSource
import sh.amux.features.ChatScreenState
import sh.amux.features.ChatSubject
import sh.amux.features.ComposerAttachmentUi
import sh.amux.features.HomeScope
import sh.amux.features.HostsTabState
import sh.amux.features.PairingPhaseUi
import sh.amux.features.PairingSheetState
import sh.amux.features.QuestionDraft as UiQuestionDraft
import sh.amux.features.label
import sh.amux.values.ActivityKind
import sh.amux.values.AgentKey
import sh.amux.values.AttachmentView
import sh.amux.values.BlobRef
import sh.amux.values.ChatFrame
import sh.amux.values.DraftAttachment
import sh.amux.values.FamilyHeader
import sh.amux.values.Filing
import sh.amux.values.FleetRow
import sh.amux.values.HostView
import sh.amux.values.Reach
import sh.amux.values.RouteKind
import sh.amux.values.Row
import sh.amux.values.Run
import sh.amux.values.RunCounts
import sh.amux.words.ChatWords
import sh.amux.words.name

/**
 * Live [ChatRowSource] backed by [ChatModel]. Each row cell subscribes solely
 * to its own `RowCell.state` `StateFlow` so changing one row recomposes only
 * that item in the transcript `LazyColumn`.
 */
internal class ModelChatRowSource(
    private val model: ChatModel,
) : ChatRowSource {
    @Composable
    override fun observeCell(id: String): ChatRowCellUi {
        val cell = model.cell(id)
        val snapshot by cell.state.collectAsState()
        return ChatRowCellUi(
            id = snapshot.id,
            row = snapshot.row,
            revision = snapshot.revision,
            attachmentRevision = snapshot.attachmentRevision,
            openedAgent = snapshot.openedAgent,
            groupState = snapshot.groupState,
        )
    }

    override fun shows(row: Row): Boolean = model.shows(row)

    override fun rowBelow(id: String): Row? = model.rowBelow(id)

    override fun countsOf(run: Run): RunCounts? = model.countsOf(run)

    override fun imageBytes(blob: BlobRef): ByteArray? = model.bytesOf(blob)
}

/**
 * Pure mappers from `core:runtime` store snapshots to `features` immutable UI states.
 */
object StateMappers {
    fun toChatSubject(
        agent: AgentKey,
        fleetRow: FleetRow?,
        frame: ChatFrame?,
        hosts: HostsState,
    ): ChatSubject {
        val hostId = HostId.fromBytes(agent.host)
        val connecting = hostId != null && hostId in hosts.connecting
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
                workspaceGoes = card.workspaceGoes,
                native = card.native,
            )
        }
        val hostView = hostId?.let { hosts.host(it) }
        return ChatSubject(
            name = frame?.name?.takeIf { it.isNotEmpty() } ?: agent.toString(),
            host = hostView?.name.orEmpty(),
            directory = "",
            branch = null,
            reach = hostView?.reach ?: Reach.Online(RouteKind.Direct),
            connecting = connecting,
            filing = Filing.Open,
        )
    }

    fun toChatScreenState(
        model: ChatModel,
        chat: ChatUiState,
        subject: ChatSubject,
        family: FamilyHeader?,
        nowMs: Long,
    ): ChatScreenState {
        val activitySubject = when (val kind = chat.frame?.composer?.activity?.kind) {
            is ActivityKind.Running -> model.cell(kind.key).row?.let(ChatWords::subject)
            else -> null
        }

        val composerAttachments = chat.attachments.mapIndexed { index, attachment ->
            when (attachment) {
                is DraftAttachment.Image -> ComposerAttachmentUi(
                    id = "img:$index:${attachment.value.hash.hashCode()}",
                    view = AttachmentView.Image(attachment.value),
                    thumbnailBytes = model.bytesOf(attachment.value),
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
                is DraftAttachment.Review -> ComposerAttachmentUi(
                    id = "review:$index:${attachment.comments.size}",
                    view = AttachmentView.Review(
                        comments = attachment.comments.size,
                        patch = attachment.diff.patch,
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

        val openedAttachmentUi = chat.openedAttachment?.let { file ->
            val bytes = runCatching { file.readBytes() }.getOrNull()
            val ext = file.extension.lowercase()
            val isImage = ext in setOf("png", "jpg", "jpeg", "gif", "webp")
            ChatOpenedAttachmentUi(
                name = file.name,
                bytes = if (isImage) bytes else null,
                isImage = isImage,
            )
        }

        val activeAskKey = chat.ask?.key
        val keptQuestion = activeAskKey?.let { model.questionDraft(it) }?.toUi()
        val keptForm = activeAskKey?.let { model.formDraft(it) }

        return ChatScreenState(
            subject = subject,
            family = family,
            keys = chat.keys,
            empty = chat.empty,
            frame = chat.frame,
            ask = chat.ask,
            overview = chat.overview,
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
            activitySubject = activitySubject,
            landingInFeed = chat.landingInFeed,
            landingInQueue = chat.landingInQueue,
            keptQuestionDraft = keptQuestion,
            keptFormValues = keptForm,
            requestedOverlay = requestedOverlay,
            openedAttachment = openedAttachmentUi,
            dictationEnabled = false,
            dictationActive = chat.dictation.active,
            dictationSentence = chat.dictation.sentence,
            nowMs = nowMs,
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

    fun toAccountsUi(registry: AccountRegistryState): List<AccountUi> =
        registry.accounts.map { entry ->
            AccountUi(
                id = entry.id.value,
                name = entry.name,
                email = entry.account.email,
                line = entry.line,
                signedIn = entry.signedIn,
                selected = entry.id == registry.selected,
            )
        }

    fun toAgentsHomeState(
        fleet: FleetState,
        hosts: HostsState,
        registry: AccountRegistryState,
        offerNotifications: Boolean = false,
        signingIn: Boolean = false,
        scope: HomeScope = HomeScope.Fleet,
    ): AgentsHomeState {
        val accountUis = toAccountsUi(registry)
        val selectedUi = accountUis.firstOrNull { it.selected }
        return AgentsHomeState(
            loaded = fleet.loaded,
            rows = fleet.rows,
            sections = fleet.sections,
            needYou = fleet.needYou,
            working = fleet.working,
            machines = fleet.machines,
            discoveredHosts = hosts.discovered,
            filter = fleet.filter,
            orderedAtMs = fleet.orderedAtMs,
            accounts = accountUis,
            selectedAccount = selectedUi,
            accountPro = hosts.account?.pro,
            relay = fleet.relay,
            relayTrouble = fleet.relayTrouble,
            heldAwayHost = fleet.heldAway,
            unreachableHost = fleet.unreachableHost,
            offerNotifications = offerNotifications,
            signingIn = signingIn,
            scope = scope,
        )
    }

    fun toHostsTabState(
        hosts: HostsState,
        hostsStore: HostsStore,
    ): HostsTabState {
        val wentOffline = buildMap {
            for (host in hosts.hosts) {
                val id = sh.amux.core.runtime.HostId.fromBytes(host.hostId) ?: continue
                hostsStore.wentOfflineAtMs(id)?.let { put(host.hostId, it) }
            }
        }
        return HostsTabState(
            hosts = hosts.hosts,
            discovered = hosts.discovered,
            accountPro = hosts.account?.pro,
            awayHostName = hosts.awayHost?.name,
            connectingHostIds = hosts.connecting.map { it.bytes }.toSet(),
            foundButUnreachableIds = hosts.foundButUnreachable.map { it.bytes }.toSet(),
            wentOfflineAtMs = wentOffline,
            nowMs = hostsStore.nowMs,
        )
    }

    /**
     * An addressless QR invitation (e.g. from an IPv6-only LAN) can still dial
     * directly without an account once local discovery has found the host.
     */
    fun invitationNeedsAccount(
        invitation: PairingInvitation,
        discovered: List<HostView>,
    ): Boolean {
        if (!invitation.needsAnAccount) return false
        return discovered.none { candidate ->
            candidate.hostId == invitation.host.bytes && candidate.addrs.isNotEmpty()
        }
    }

    fun toPairingSheetState(
        pairing: PairingState,
        modal: PairingModal,
        signedIn: Boolean,
        signingIn: Boolean,
        nowMs: Long,
        discovered: List<HostView> = emptyList(),
    ): PairingSheetState {
        val viaLink = modal is PairingModal.Link
        val alreadyConfirmedOrConfirming = pairing.phase is PairingStore.Phase.Confirming ||
            pairing.phase is PairingStore.Phase.Trusted
        val phase: PairingPhaseUi = when {
            modal is PairingModal.Link &&
                !alreadyConfirmedOrConfirming &&
                invitationNeedsAccount(modal.invitation, discovered) &&
                !signedIn -> {
                PairingPhaseUi.NeedsAccount(signingIn = signingIn)
            }
            else -> when (val raw = pairing.phase) {
                PairingStore.Phase.Entering -> {
                    if (viaLink) PairingPhaseUi.Checking(viaLink = true) else PairingPhaseUi.Entering
                }
                PairingStore.Phase.Checking -> PairingPhaseUi.Checking(viaLink = viaLink)
                is PairingStore.Phase.Confirming -> PairingPhaseUi.Confirming(
                    pending = raw.pending,
                    platformName = pairing.machine?.platform?.label,
                )
                is PairingStore.Phase.Trusted -> PairingPhaseUi.Trusted(raw.name)
                PairingStore.Phase.Refused -> PairingPhaseUi.Refused(
                    message = pairing.told,
                    viaLink = viaLink,
                )
                PairingStore.Phase.NeedsSubscription -> PairingPhaseUi.NeedsSubscription
            }
        }
        return PairingSheetState(
            digits = pairing.digits,
            phase = phase,
            machine = pairing.machine,
            offerWindow = PairingStore.OFFER_WINDOW,
            nowMs = nowMs,
        )
    }
}
