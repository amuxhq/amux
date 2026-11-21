package sh.amux.shell

import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.AccountRegistryState
import sh.amux.core.runtime.ChatAsk
import sh.amux.core.runtime.ChatModel
import sh.amux.core.runtime.ChatPaging
import sh.amux.core.runtime.ChatUiState
import sh.amux.core.runtime.DeletionState
import sh.amux.core.runtime.DeletionStore
import sh.amux.core.runtime.Entitlement
import sh.amux.core.runtime.EntitlementSource
import sh.amux.core.runtime.FleetState
import sh.amux.core.runtime.FrozenFrame
import sh.amux.core.runtime.Grant
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.HostsState
import sh.amux.core.runtime.HostsStore
import sh.amux.core.runtime.ListingState
import sh.amux.core.runtime.NewAgentState
import sh.amux.core.runtime.OfferState
import sh.amux.core.runtime.PairingInvitation
import sh.amux.core.runtime.PairingState
import sh.amux.core.runtime.PairingStore
import sh.amux.core.runtime.PaywallState
import sh.amux.core.runtime.PaywallStore
import sh.amux.core.runtime.Plan
import sh.amux.core.runtime.QuestionDraft as RuntimeQuestionDraft
import sh.amux.core.runtime.ReportMark
import sh.amux.core.runtime.ReportSending
import sh.amux.core.runtime.ReportState
import sh.amux.core.runtime.SignInIntent
import sh.amux.core.runtime.SignInOffers
import sh.amux.core.runtime.SignInOffersState
import sh.amux.core.runtime.SignInPrompt
import sh.amux.core.runtime.SignInState
import sh.amux.core.runtime.SignInStore
import sh.amux.core.runtime.online
import sh.amux.design.AppearanceMode
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
import sh.amux.features.DeleteAccountPhaseUi
import sh.amux.features.DeleteAccountSheetState
import sh.amux.features.DiagnosticsExportState
import sh.amux.features.HomeScope
import sh.amux.features.HostPageState
import sh.amux.features.HostsTabState
import sh.amux.features.NewAgentListingUi
import sh.amux.features.NewAgentOfferUi
import sh.amux.features.NewAgentScreenState
import sh.amux.features.PairingPhaseUi
import sh.amux.features.PairingSheetState
import sh.amux.features.PaywallGrantSource
import sh.amux.features.PaywallPhaseUi
import sh.amux.features.PaywallPlanPeriod
import sh.amux.features.PaywallPlanUi
import sh.amux.features.PaywallScreenState
import sh.amux.features.PaywallUnconfirmedReason
import sh.amux.features.QuestionDraft as UiQuestionDraft
import sh.amux.features.RemoveAccountState
import sh.amux.features.ReportDeliveryUi
import sh.amux.features.ReportMarkUi
import sh.amux.features.ReportUiState
import sh.amux.features.SettingsAccountItem
import sh.amux.features.SettingsAppearance
import sh.amux.features.SettingsEntitlementBadge
import sh.amux.features.SettingsNotificationStatus
import sh.amux.features.SettingsScreenState
import sh.amux.features.SettledScreenState
import sh.amux.features.SignInOfferFailureUi
import sh.amux.features.SignInOfferPromptUi
import sh.amux.features.SignInOfferSheetState
import sh.amux.features.SignInResultUi
import sh.amux.features.label
import sh.amux.values.AccountBinding
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
import sh.amux.values.RelayLink
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
                key = agent,
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
            key = agent,
        )
    }

    fun toChatScreenState(
        model: ChatModel,
        chat: ChatUiState,
        subject: ChatSubject,
        family: FamilyHeader?,
        nowMs: Long,
        openedAttachment: ChatOpenedAttachmentUi? = null,
        dictationEnabled: Boolean = false,
    ): ChatScreenState {
        val activitySubject = when (val kind = chat.frame?.composer?.activity?.kind) {
            is ActivityKind.Running -> model.rowOrNull(kind.key)?.let(ChatWords::subject)
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
            openedAttachment = openedAttachment,
            dictationEnabled = dictationEnabled,
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

    fun toNewAgentScreenState(
        state: NewAgentState,
        connecting: Set<HostId>,
    ): NewAgentScreenState = NewAgentScreenState(
        hosts = state.hosts,
        selectedHostId = state.machine?.bytes,
        connectingHostIds = connecting.map { it.bytes }.toSet(),
        provider = state.provider.wire,
        availableProviders = state.availableProviders.map { it.wire },
        offer = when (state.offers[state.provider]) {
            is OfferState.Offered -> NewAgentOfferUi.Offered
            OfferState.Unavailable -> NewAgentOfferUi.Unavailable
            OfferState.Asking, null -> NewAgentOfferUi.Asking
        },
        settings = state.settings,
        listing = when (state.listing) {
            ListingState.None -> NewAgentListingUi.None
            ListingState.Asking -> NewAgentListingUi.Asking
            ListingState.Ready -> NewAgentListingUi.Ready
            ListingState.Unavailable -> NewAgentListingUi.Unavailable
        },
        repositories = state.repositories,
        recent = state.recent,
        history = state.history,
        directory = state.directory,
        query = state.query,
        typedPath = state.typed,
        browsing = state.browsing,
        newWorktree = state.newWorktree,
        name = state.name,
        nameProblem = state.nameProblem,
        starting = state.starting,
        failure = state.failure,
    )

    fun toHostPageState(
        hostId: HostId,
        hosts: HostsState,
        hostsStore: HostsStore,
        offers: SignInOffersState,
    ): HostPageState {
        val host = hosts.host(hostId)
        val fingerprint = hosts.roster?.peers?.firstOrNull { it.hostId == hostId.bytes }?.fingerprint
        val canOffer = hosts.account?.binding == AccountBinding.SignedIn &&
            host != null &&
            host.signedIn == false &&
            host.online
        return HostPageState(
            host = host,
            fingerprint = fingerprint,
            isConnecting = hosts.isConnecting(hostId),
            foundButUnreachable = hostId in hosts.foundButUnreachable,
            wentOfflineAtMs = hostsStore.wentOfflineAtMs(hostId),
            nowMs = hostsStore.nowMs,
            canOfferSignIn = canOffer,
            offeringSignIn = offers.answering == hostId.bytes,
            accountPro = hosts.account?.pro,
        )
    }

    fun toSettledScreenState(
        fleet: FleetState,
        query: String,
    ): SettledScreenState = SettledScreenState(
        rows = fleet.closed?.rows.orEmpty(),
        query = query,
        nowMs = fleet.orderedAtMs,
    )

    fun toSettingsAppearance(mode: AppearanceMode): SettingsAppearance = when (mode) {
        AppearanceMode.Light -> SettingsAppearance.Light
        AppearanceMode.Dark -> SettingsAppearance.Dark
        AppearanceMode.System -> SettingsAppearance.System
    }

    fun toAppearanceMode(appearance: SettingsAppearance): AppearanceMode = when (appearance) {
        SettingsAppearance.Light -> AppearanceMode.Light
        SettingsAppearance.Dark -> AppearanceMode.Dark
        SettingsAppearance.System -> AppearanceMode.System
    }

    fun toSettingsScreenState(
        registry: AccountRegistryState,
        hosts: HostsState,
        fleet: FleetState,
        signIn: SignInState,
        appearance: AppearanceMode,
        notificationStatus: SettingsNotificationStatus,
        shareUsage: Boolean,
        autoReports: Boolean,
        diagnosticsExport: DiagnosticsExportState,
        deviceName: String,
        fingerprintCopied: Boolean,
        appVersion: String,
    ): SettingsScreenState {
        val returningId = (signIn.pending as? SignInIntent.Returning)?.account?.id
        val items = registry.accounts.map { entry ->
            val selected = entry.id == registry.selected
            val expired = selected && fleet.relay == RelayLink.SignInAgain
            val badge = when (val ent = entry.entitlement) {
                is Entitlement.Active -> when (ent.grant) {
                    Grant.Granted -> SettingsEntitlementBadge.Included
                    is Grant.Purchased -> SettingsEntitlementBadge.Pro
                }
                Entitlement.None, is Entitlement.Lapsed -> {
                    if (selected && hosts.account?.pro == true) {
                        SettingsEntitlementBadge.Pro
                    } else {
                        SettingsEntitlementBadge.Free
                    }
                }
            }
            val summary = when (val ent = entry.entitlement) {
                is Entitlement.Active -> ent.grant.named
                Entitlement.None, is Entitlement.Lapsed -> null
            }
            SettingsAccountItem(
                id = entry.id.value,
                name = entry.name,
                email = entry.account.email,
                line = entry.line,
                signedIn = entry.signedIn,
                selected = selected,
                expired = expired,
                entitlementBadge = badge,
                entitlementSummary = summary,
                signingIn = signIn.working && returningId == entry.id,
            )
        }
        return SettingsScreenState(
            accounts = items,
            addingAccount = signIn.working && signIn.pending is SignInIntent.Adding,
            appearance = toSettingsAppearance(appearance),
            notificationStatus = notificationStatus,
            pushDeliveryComingSoon = true,
            shareUsage = shareUsage,
            autoReports = autoReports,
            diagnosticsExport = diagnosticsExport,
            deviceName = deviceName,
            deviceFingerprint = hosts.roster?.identity?.fingerprint.orEmpty(),
            fingerprintCopied = fingerprintCopied,
            appVersion = appVersion,
            showContactSupport = true,
        )
    }

    fun toPaywallScreenState(
        paywall: PaywallState,
        accountEmail: String?,
        storeUnavailable: Boolean = false,
    ): PaywallScreenState {
        val plans = paywall.plans.map { plan ->
            PaywallPlanUi(
                id = plan.id,
                period = when (plan.period) {
                    Plan.Period.Monthly -> PaywallPlanPeriod.Monthly
                    Plan.Period.Yearly -> PaywallPlanPeriod.Yearly
                },
                price = plan.price,
                saving = plan.saving,
            )
        }
        val chosen = when (paywall.chosen) {
            Plan.Period.Monthly -> PaywallPlanPeriod.Monthly
            Plan.Period.Yearly -> PaywallPlanPeriod.Yearly
        }
        val phase: PaywallPhaseUi = when (val raw = paywall.phase) {
            is PaywallStore.Phase.Entitled -> {
                val source = when (val grant = raw.grant) {
                    Grant.Granted -> PaywallGrantSource.Granted
                    is Grant.Purchased -> when (grant.source) {
                        EntitlementSource.PlayStore -> PaywallGrantSource.PlayStore
                        EntitlementSource.AppStore -> PaywallGrantSource.AppStore
                        EntitlementSource.Web -> PaywallGrantSource.Web
                    }
                }
                val renewalText = (paywall.entitlement as? Entitlement.Active)?.renewsAtMs?.let { ms ->
                    val date = Instant.ofEpochMilli(ms)
                        .atZone(ZoneId.systemDefault())
                        .toLocalDate()
                        .format(DateTimeFormatter.ofLocalizedDate(FormatStyle.MEDIUM))
                    "Renews $date"
                }
                PaywallPhaseUi.Entitled(source = source, renewalOrExpiryText = renewalText)
            }
            PaywallStore.Phase.Buying -> PaywallPhaseUi.Buying
            PaywallStore.Phase.AwaitingApproval -> PaywallPhaseUi.AwaitingApproval
            PaywallStore.Phase.Confirming -> PaywallPhaseUi.Confirming
            is PaywallStore.Phase.Unconfirmed -> PaywallPhaseUi.Unconfirmed(
                when (val why = raw.why) {
                    PaywallStore.Unconfirmed.Unreachable -> PaywallUnconfirmedReason.Unreachable
                    is PaywallStore.Unconfirmed.Refused -> PaywallUnconfirmedReason.Refused(why.reason)
                    PaywallStore.Unconfirmed.SwitchingOn -> PaywallUnconfirmedReason.SwitchingOn
                },
            )
            is PaywallStore.Phase.Failed -> {
                if (storeUnavailable && plans.isEmpty()) {
                    PaywallPhaseUi.StoreUnavailable(raw.reason)
                } else {
                    PaywallPhaseUi.Failed(raw.reason)
                }
            }
            PaywallStore.Phase.Ready -> {
                if (storeUnavailable && plans.isEmpty()) {
                    PaywallPhaseUi.StoreUnavailable(null)
                } else {
                    PaywallPhaseUi.Ready
                }
            }
        }
        return PaywallScreenState(
            plans = plans,
            chosen = chosen,
            phase = phase,
            manageUrl = paywall.manageUrl,
            accountEmail = accountEmail,
        )
    }

    fun toPlanPeriod(period: PaywallPlanPeriod): Plan.Period = when (period) {
        PaywallPlanPeriod.Monthly -> Plan.Period.Monthly
        PaywallPlanPeriod.Yearly -> Plan.Period.Yearly
    }

    fun toSignInOfferSheetState(
        prompt: SignInPrompt,
        offers: SignInOffersState,
    ): SignInOfferSheetState {
        val uiPrompt = when (prompt) {
            is SignInPrompt.Offered -> SignInOfferPromptUi.Offered(
                hostName = prompt.offer.hostName,
                email = prompt.offer.email,
            )
            is SignInPrompt.Offering -> SignInOfferPromptUi.Offering(
                hostName = prompt.host.name,
                email = prompt.email,
            )
        }
        val uiFailure = when (val failure = offers.failure) {
            is SignInOffers.Failure.Unreachable -> SignInOfferFailureUi.Unreachable(failure.hostName)
            is SignInOffers.Failure.Refused -> SignInOfferFailureUi.Refused(failure.reason)
            null -> null
        }
        return SignInOfferSheetState(
            prompt = uiPrompt,
            working = offers.answering != null,
            failure = uiFailure,
        )
    }

    fun toSignInResultUi(phase: SignInStore.Phase): SignInResultUi? = when (phase) {
        is SignInStore.Phase.Mismatched -> SignInResultUi.Mismatched(
            expectedEmail = phase.wanted.email,
            signedInEmail = phase.got.email,
        )
        is SignInStore.Phase.Failed -> SignInResultUi.Failed(reason = phase.reason)
        SignInStore.Phase.Ready, SignInStore.Phase.HandingOff -> null
    }

    fun toRemoveAccountState(
        accountId: AccountId?,
        registry: AccountRegistryState,
    ): RemoveAccountState? {
        val id = accountId ?: return null
        val entry = registry.accounts.firstOrNull { it.id == id }
        return RemoveAccountState(
            accountId = id.value,
            email = entry?.account?.email ?: id.value,
            displayName = entry?.account?.displayName,
        )
    }

    fun toDeleteAccountSheetState(
        deletion: DeletionState,
        registry: AccountRegistryState,
    ): DeleteAccountSheetState? {
        val id = deletion.account ?: return null
        val phase: DeleteAccountPhaseUi = when (val raw = deletion.phase) {
            DeletionStore.Phase.Asking -> DeleteAccountPhaseUi.Asking
            DeletionStore.Phase.Working -> DeleteAccountPhaseUi.Deleting
            is DeletionStore.Phase.Blocked -> DeleteAccountPhaseUi.Blocked(
                providerPlace = raw.source.place,
                manageUrl = raw.manageUrl,
            )
            is DeletionStore.Phase.Failed -> DeleteAccountPhaseUi.Failed(raw.reason)
            DeletionStore.Phase.Deleted -> return null
        }
        val entry = registry.accounts.firstOrNull { it.id == id }
        return DeleteAccountSheetState(
            accountId = id.value,
            email = entry?.account?.email ?: id.value,
            typedEmail = deletion.typed,
            phase = phase,
        )
    }

    fun toReportUiState(
        report: ReportState,
        signedIn: Boolean,
    ): ReportUiState? {
        val capture = report.capture ?: return null
        val frame = capture.frame
        val width = frame.widthDp.takeIf { it > 0.0 } ?: 1.0
        val height = frame.heightDp.takeIf { it > 0.0 } ?: 1.0
        val aspectRatio = (width / height).toFloat().takeIf { it.isFinite() && it > 0f }
            ?: ReportUiState.DEFAULT_FRAME_ASPECT_RATIO
        val marks = report.draft.marks.map { mark ->
            val left = (mark.x / width).toFloat().coerceIn(0f, 1f)
            val top = (mark.y / height).toFloat().coerceIn(0f, 1f)
            val right = ((mark.x + mark.width) / width).toFloat().coerceIn(left, 1f)
            val bottom = ((mark.y + mark.height) / height).toFloat().coerceIn(top, 1f)
            ReportMarkUi(
                left = left,
                top = top,
                right = right,
                bottom = bottom,
                note = mark.note,
                redact = mark.redact,
            )
        }
        val delivery = when (val sending = report.sending) {
            ReportSending.Ready -> ReportDeliveryUi.Ready
            ReportSending.Sending -> ReportDeliveryUi.Sending
            is ReportSending.Failed -> ReportDeliveryUi.Failed(sending.reason)
            is ReportSending.Sent -> ReportDeliveryUi.Sent(sending.receipt.id)
        }
        return ReportUiState(
            imageBytes = frame.png.takeIf { it.isNotEmpty() },
            frameAspectRatio = aspectRatio,
            note = report.draft.note,
            marks = marks,
            signedIn = signedIn,
            diagnosticDumpAttached = capture.dump != null,
            locked = report.sealed != null,
            delivery = delivery,
        )
    }

    fun toReportMark(mark: ReportMarkUi, frame: FrozenFrame): ReportMark {
        val width = frame.widthDp.takeIf { it > 0.0 } ?: 1.0
        val height = frame.heightDp.takeIf { it > 0.0 } ?: 1.0
        return ReportMark(
            x = mark.left * width,
            y = mark.top * height,
            width = mark.width * width,
            height = mark.height * height,
            note = mark.note,
            redact = mark.redact,
        )
    }
}
