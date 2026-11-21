package sh.amux.core.runtime

import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import sh.amux.values.AccountView
import sh.amux.values.AgentAct
import sh.amux.values.AgentKey
import sh.amux.values.ChatDelta
import sh.amux.values.DeleteWarning
import sh.amux.values.FleetDelta
import sh.amux.values.FleetFilter
import sh.amux.values.FleetView
import sh.amux.values.HostView
import sh.amux.values.PairingWindowView
import sh.amux.values.PairRequest
import sh.amux.values.PendingPair
import sh.amux.values.SignInOfferOutcome
import sh.amux.values.SignInOfferView

/**
 * A balanced viewer lease on an open [ChatModel] in [StoreBundle].
 *
 * Call [release] when the UI surface displaying [agent] leaves composition;
 * releasing is idempotent and never disturbs other routes displaying the same
 * agent (such as transitioning between Conversation and Changes).
 */
class ChatLease internal constructor(
    val agent: AgentKey,
    val model: ChatModel,
    private val onRelease: () -> Unit,
) {
    private var released = false

    fun release() {
        if (released) return
        released = true
        onRelease()
    }
}

/**
 * Everything one account's screens draw over the [Profile] serving it.
 *
 * The fleet opens whole on [attach], then moves by incremental [FleetEvent]
 * deltas. Actions go through [Profile] and update the observable stores.
 *
 * [scope] must run on the main thread, as every caller does: the bundle's
 * maps and the stores it feeds are read and written there without locks.
 */
class StoreBundle(
    val account: AccountId,
    private val scope: CoroutineScope,
    val clock: () -> Long = System::currentTimeMillis,
    val sessionRetentionMs: Long = DEFAULT_SESSION_RETENTION_MS,
    private val chatBridge: ChatBridge = NativeChatBridge,
    private val attachmentDir: File = File(System.getProperty("java.io.tmpdir", "/tmp"), "amux-attachments"),
    private val opener: ((AgentKey) -> OpenChat)? = null,
    private val acting: (suspend (AgentAct, AgentKey) -> Result<Unit>)? = null,
    private val warning: (suspend (AgentKey) -> Result<DeleteWarning?>)? = null,
    private val beginPairOp: (suspend (PairRequest) -> Result<PendingPair>)? = null,
    private val abandonPairOp: (suspend (PendingPair) -> Result<Unit>)? = null,
    private val answerSignInOfferOp: (suspend (SignInOfferView, Boolean) -> Result<SignInOfferOutcome>)? = null,
) {
    val fleet: FleetStore = FleetStore(clock = clock)
    val hosts: HostsStore = HostsStore(scope = scope, clock = clock)
    val pairing: PairingStore = PairingStore(clock = clock)
    val offers: SignInOffers = SignInOffers()

    private val _applied = MutableStateFlow(0)
    val applied: StateFlow<Int> = _applied.asStateFlow()

    private val _profileReady = MutableStateFlow(false)
    val profileReady: StateFlow<Boolean> = _profileReady.asStateFlow()

    private val _profileVersion = MutableStateFlow(0)
    val profileVersion: StateFlow<Int> = _profileVersion.asStateFlow()

    var profile: Profile? = null
        private set

    var saw: ((hosts: Int, attention: Int) -> Unit)? = null
    var trusts: (() -> Unit)? = null

    private data class CachedChat(
        val chat: OpenChat,
        val model: ChatModel,
    )

    private var listedView: FleetView? = null
    private var listedHosts: List<HostView>? = null
    private var hostsShown = false
    private val chats = mutableMapOf<Long, Pair<OpenChat, (ChatDelta) -> Unit>>()
    private val models = mutableMapOf<AgentKey, CachedChat>()
    private val shownCounts = mutableMapOf<AgentKey, Int>()
    private val warming = mutableSetOf<AgentKey>()
    private val viewedAtMs = mutableMapOf<AgentKey, Long>()
    private val expiryJobs = mutableMapOf<AgentKey, Job>()

    init {
        fleet.actions = object : FleetActions {
            override fun filterFleet(filter: FleetFilter) {
                profile?.filter(filter)
            }

            override fun toggleFamily(head: AgentKey) {
                this@StoreBundle.toggleFamily(head)
            }

            override suspend fun pin(agent: AgentKey): Result<Unit> =
                this@StoreBundle.pin(agent)

            override suspend fun settle(agent: AgentKey): Result<Unit> =
                this@StoreBundle.close(agent)

            override suspend fun unsettle(agent: AgentKey): Result<Unit> =
                this@StoreBundle.unfile(agent)

            override suspend fun rename(name: String, agent: AgentKey): Result<Unit> =
                this@StoreBundle.rename(name, agent)

            override suspend fun delete(agent: AgentKey, discardingChanges: Boolean): Result<Unit> =
                this@StoreBundle.delete(agent, discardingChanges)

            override suspend fun cancelStop(agent: AgentKey): Result<Unit> =
                this@StoreBundle.cancelClose(agent)

            override suspend fun previewDelete(agent: AgentKey): Result<DeleteWarning?> =
                this@StoreBundle.deleteWarning(agent)
        }

        hosts.actions = object : HostActions {
            override fun unpair(host: HostId) {
                revoke(host)
            }

            override fun offerSignIn(host: HostView) {
                offerAccount(host)
            }

            override suspend fun offerPairing(ttlSeconds: Long): Result<PairingWindowView> {
                val activeProfile = profile
                    ?: return Result.failure(RuntimeFailure("nothing is running"))
                return activeProfile.openPairing(ttlSeconds)
            }
        }
    }

    /** Attaches a live [Profile] and loads its initial fleet snapshot, or detaches when `null`. */
    fun attach(profile: Profile?) {
        this.profile = profile
        _profileReady.value = profile != null
        if (profile == null) {
            hostsShown = false
            closeChats()
        } else {
            apply(profile.fleetOpening())
        }
        _profileVersion.value += 1
    }

    /** Applies one event stepped out of the runtime for this profile. */
    fun moved(event: FleetEvent) {
        when (event) {
            is FleetEvent.Fleet -> apply(event.delta)
            is FleetEvent.Chat -> chats[event.delta.chat]?.second?.invoke(event.delta)
            is FleetEvent.SignInOffers -> offers.listed(event.offers)
            is FleetEvent.Onboarding,
            is FleetEvent.Pairing,
            -> Unit
        }
    }

    private fun apply(delta: FleetDelta) {
        if (delta.view != null) {
            listedView = delta.view
        }
        if (delta.hosts != null) {
            listedHosts = delta.hosts
        }
        val view = listedView ?: return
        val hostViews = listedHosts ?: return

        val shouldShowHosts = delta.hosts != null || !hostsShown
        fleet.show(view, hostViews)
        if (shouldShowHosts) {
            hostsShown = true
            hosts.show(hostViews)
            if (hosts.snapshot.hosts.isNotEmpty()) {
                trusts?.invoke()
            }
        }
        saw?.invoke(fleet.snapshot.machines.count { it.trusted }, fleet.snapshot.needYou)
        _applied.value += 1
        keepSessions()
        if (shouldShowHosts && hosts.snapshot.rosterBehind) {
            scope.launch { refreshRoster() }
        }
    }

    fun showAccount(account: AccountView) {
        hosts.showAccount(account)
        fleet.relay(account.relay)
    }

    suspend fun refreshRoster() {
        val activeProfile = profile ?: return
        activeProfile.roster().onSuccess { hosts.showRoster(it) }
    }

    fun filterFleet(filter: FleetFilter) {
        fleet.setFilter(filter)
    }

    fun toggleFamily(head: AgentKey) {
        profile?.toggleFamily(head)
    }

    // --- Chats -------------------------------------------------------------

    @Throws(RuntimeFailure::class)
    private suspend fun openChat(agent: AgentKey): OpenChat {
        if (opener != null) return opener.invoke(agent)
        val activeProfile = profile
            ?: throw RuntimeFailure("nothing is running for this account")
        val opened = activeProfile.openChat(agent)
        // The profile can be detached while the chat opens on the core thread.
        if (profile !== activeProfile) {
            opened.close()
            throw RuntimeFailure("nothing is running for this account")
        }
        return opened
    }

    private fun closeChat(chat: OpenChat) {
        chats.remove(chat.id)
        chat.close()
    }

    /**
     * A lease on [agent]'s chat if it is already open here, taken without
     * reaching the core thread, so a route coming back to a chat draws it at
     * once. Otherwise `null`, and [acquireChat] opens it.
     */
    fun leaseCached(agent: AgentKey): ChatLease? {
        val model = models[agent]?.model ?: return null
        return lease(agent, model)
    }

    /**
     * Opens [agent]'s chat if it is not open yet and takes a lease keeping it
     * open until released. The open runs on the core thread; the caller's
     * thread draws meanwhile.
     */
    @Throws(RuntimeFailure::class)
    suspend fun acquireChat(agent: AgentKey): ChatLease = lease(agent, cachedModel(agent))

    private fun lease(agent: AgentKey, model: ChatModel): ChatLease {
        shownCounts[agent] = (shownCounts[agent] ?: 0) + 1
        expiryJobs.remove(agent)?.cancel()
        return ChatLease(agent = agent, model = model) {
            if (models[agent]?.model === model) {
                leave(agent)
            }
        }
    }

    @Throws(RuntimeFailure::class)
    private suspend fun cachedModel(agent: AgentKey): ChatModel {
        models[agent]?.let { return it.model }
        val opened = openChat(agent)
        // Two routes can open one chat at once; the first to land keeps it.
        models[agent]?.let { cached ->
            opened.close()
            return cached.model
        }
        chats[opened.id] = opened to { delta -> models[agent]?.model?.apply(delta) }
        val model = ChatModel(
            source = opened,
            scope = scope,
            bridge = chatBridge,
            attachmentDir = attachmentDir,
        )
        models[agent] = CachedChat(chat = opened, model = model)
        return model
    }

    /**
     * Releases one active viewer for [agent]. When the last active viewer
     * leaves, the session stays cached for [sessionRetentionMs] so returning
     * is instant, then closes cleanly.
     */
    private fun leave(agent: AgentKey) {
        if (agent !in models) {
            shownCounts.remove(agent)
            return
        }
        val remaining = (shownCounts[agent] ?: 0) - 1
        if (remaining > 0) {
            shownCounts[agent] = remaining
            return
        }
        shownCounts.remove(agent)
        viewedAtMs[agent] = clock()
        expiryJobs.remove(agent)?.cancel()
        if (sessionRetentionMs <= 0L) {
            keepSessions()
        } else {
            expiryJobs[agent] = scope.launch {
                delay(sessionRetentionMs)
                expiryJobs.remove(agent)
                keepSessions()
            }
        }
    }

    /**
     * Reconciles cached chat sessions: keeps every chat currently shown, every
     * chat viewed within [sessionRetentionMs], and every chat currently being
     * warmed for a push notification; closes all others cleanly.
     */
    fun keepSessions() {
        val now = clock()
        val toClose = mutableListOf<AgentKey>()
        for (agent in models.keys) {
            if ((shownCounts[agent] ?: 0) > 0 || agent in warming) continue
            val lastViewed = viewedAtMs[agent]
            if (lastViewed != null && now - lastViewed < sessionRetentionMs) continue
            toClose.add(agent)
        }
        for (agent in toClose) {
            evictCachedChat(agent)
        }
    }

    /**
     * Brings [agent]'s chat current for a push wakeup, keeping its session open
     * across fleet updates until caught up or [timeoutMs] elapses.
     */
    suspend fun warm(
        agent: AgentKey,
        timeoutMs: Long = DEFAULT_WARM_TIMEOUT_MS,
    ): Boolean {
        val model = try {
            cachedModel(agent)
        } catch (_: RuntimeFailure) {
            return false
        }
        warming.add(agent)
        val caughtUp = try {
            withTimeoutOrNull(timeoutMs) {
                model.state.first { it.frame?.caughtUp == true }
                true
            } ?: (model.currentFrame?.caughtUp == true)
        } finally {
            warming.remove(agent)
            keepSessions()
        }
        return caughtUp
    }

    private fun evictCachedChat(agent: AgentKey) {
        expiryJobs.remove(agent)?.cancel()
        shownCounts.remove(agent)
        warming.remove(agent)
        viewedAtMs.remove(agent)
        val cached = models.remove(agent) ?: return
        cached.model.close()
        closeChat(cached.chat)
    }

    /** Stops any active dictation state on open chat models (e.g. when the Activity backgrounds). */
    fun stopAllDictation() {
        for (cached in models.values) {
            cached.model.stopDictation()
        }
    }

    fun closeChats() {
        val pendingJobs = expiryJobs.values.toList()
        expiryJobs.clear()
        for (job in pendingJobs) {
            job.cancel()
        }
        val openModels = models.values.map { it.model }
        models.clear()
        shownCounts.clear()
        warming.clear()
        viewedAtMs.clear()
        for (model in openModels) {
            model.close()
        }
        val openChats = chats.values.map { it.first }
        chats.clear()
        for (chat in openChats) {
            chat.close()
        }
    }

    // --- Pairing -----------------------------------------------------------

    fun pairDigits(typed: String): Boolean {
        pairing.enter(typed)
        val request = pairing.snapshot.request ?: return false
        beginPair(request, relayOnly = pairing.snapshot.machine?.relayOnly ?: true)
        return true
    }

    fun pairLink(link: String, relayOnly: Boolean = false): Boolean {
        if (profile == null && beginPairOp == null) return false
        pairing.open()
        beginPair(PairRequest.Link(link), relayOnly = relayOnly)
        return true
    }

    private fun beginPair(request: PairRequest, relayOnly: Boolean) {
        val activeProfile = profile
        val pairAction = beginPairOp
        if (activeProfile == null && pairAction == null) return
        val attempt = pairing.checking()
        val unsubscribed = relayOnly && hosts.snapshot.account?.pro == false
        scope.launch {
            val reached = if (pairAction != null) {
                pairAction(request)
            } else {
                activeProfile!!.beginPair(request)
            }
            val accepted = pairing.reached(reached, attempt = attempt, relayOnly = unsubscribed)
            if (!accepted) {
                reached.getOrNull()?.let { stalePending ->
                    abandonPair(activeProfile, stalePending)
                }
            }
        }
    }

    fun confirmPairing(pending: PendingPair) {
        val activeProfile = profile ?: return
        val attempt = pairing.checking()
        scope.launch {
            val confirmed = activeProfile.confirmPair(pending)
            if (confirmed.isSuccess) {
                HostId.fromBytes(pending.hostId)?.let { offers.paired(it) }
            }
            pairing.paired(confirmed.map { pending.name }, attempt = attempt)
            refreshRoster()
        }
    }

    fun abandonPairing(pending: PendingPair) {
        pairing.abandoned()
        val activeProfile = profile
        if (activeProfile == null && abandonPairOp == null) return
        scope.launch {
            abandonPair(activeProfile, pending)
        }
    }

    /**
     * Cancels the current pairing modal: abandons an already-returned
     * [PendingPair] if confirming, or invalidates any in-flight `beginPair`
     * attempt so its late-arriving token is abandoned automatically.
     */
    fun cancelPairing() {
        val currentPhase = pairing.snapshot.phase
        if (currentPhase is PairingStore.Phase.Confirming) {
            abandonPairing(currentPhase.pending)
        } else {
            pairing.abandoned()
        }
    }

    private suspend fun abandonPair(activeProfile: Profile?, pending: PendingPair) {
        val abandonAction = abandonPairOp
        if (abandonAction != null) {
            abandonAction(pending)
        } else {
            activeProfile?.abandonPair(pending)
        }
    }

    // --- Sign-in offers ----------------------------------------------------

    fun answerOffer(offer: SignInOfferView, accept: Boolean) {
        val activeProfile = profile
        val answerAction = answerSignInOfferOp
        if (activeProfile == null && answerAction == null) return
        if (offers.snapshot.answering != null) return
        offers.answers(offer.hostId)
        scope.launch {
            val outcome = if (answerAction != null) {
                answerAction(offer, accept)
            } else {
                activeProfile!!.answerSignInOffer(offer.hostId, accept)
            }
            offers.answered(outcome, fromHostId = offer.hostId)
        }
    }

    fun offerAccount(host: HostView) {
        val activeProfile = profile ?: return
        val hostId = host.id ?: return
        if (offers.snapshot.answering != null) return
        offers.answers(host.hostId)
        scope.launch {
            offers.offered(activeProfile.offerSignIn(hostId), to = hostId)
        }
    }

    fun revoke(host: HostId) {
        val activeProfile = profile ?: return
        scope.launch {
            activeProfile.unpair(host)
            refreshRoster()
        }
    }

    // --- Agents ------------------------------------------------------------

    suspend fun rename(name: String, agent: AgentKey): Result<Unit> {
        val trimmed = name.trim()
        if (trimmed.isEmpty()) {
            return Result.failure(RuntimeFailure("a name cannot be empty"))
        }
        return agentAct(AgentAct.Rename(trimmed), agent)
    }

    suspend fun close(agent: AgentKey): Result<Unit> = agentAct(AgentAct.Close, agent)

    suspend fun cancelClose(agent: AgentKey): Result<Unit> = agentAct(AgentAct.CancelClose, agent)

    suspend fun pin(agent: AgentKey): Result<Unit> = agentAct(AgentAct.Pin, agent)

    suspend fun unfile(agent: AgentKey): Result<Unit> = agentAct(AgentAct.Unfile, agent)

    suspend fun delete(agent: AgentKey, discardingChanges: Boolean = false): Result<Unit> {
        val outcome = agentAct(
            if (discardingChanges) AgentAct.DeleteDiscardingChanges else AgentAct.Delete,
            agent,
        )
        if (outcome.isSuccess) {
            models[agent]?.model?.stopKeepingDraft()
            evictCachedChat(agent)
        }
        return outcome
    }

    suspend fun deleteWarning(agent: AgentKey): Result<DeleteWarning?> {
        warning?.let { return it(agent) }
        val activeProfile = profile ?: return Result.failure(RuntimeFailure("nothing is running"))
        return activeProfile.deleteWarning(agent)
    }

    private suspend fun agentAct(act: AgentAct, agent: AgentKey): Result<Unit> {
        acting?.let { return it(act, agent) }
        val activeProfile = profile ?: return Result.failure(RuntimeFailure("nothing is running"))
        return activeProfile.act(act, agent)
    }

    // --- Diagnostics -------------------------------------------------------

    suspend fun dump(reason: String): Result<File> {
        val activeProfile = profile ?: return Result.failure(RuntimeFailure("nothing is running"))
        return activeProfile.dump(reason).map { File(it) }
    }

    companion object {
        /** Retained open-chat duration after leaving a chat screen (5 minutes). */
        const val DEFAULT_SESSION_RETENTION_MS: Long = 300_000L

        /** Maximum time [warm] waits for a push-opened chat to catch up. */
        const val DEFAULT_WARM_TIMEOUT_MS: Long = 25_000L
    }
}
