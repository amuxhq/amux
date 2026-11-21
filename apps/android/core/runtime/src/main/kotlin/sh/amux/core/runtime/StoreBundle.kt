package sh.amux.core.runtime

import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
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
import sh.amux.values.SignInOfferView

/**
 * Everything one account's screens draw over the [Profile] serving it.
 *
 * The fleet opens whole on [attach], then moves by incremental [FleetEvent]
 * deltas. Actions go through [Profile] and update the observable stores.
 */
class StoreBundle(
    val account: AccountId,
    private val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default),
    val clock: () -> Long = System::currentTimeMillis,
    private val opener: ((AgentKey) -> OpenChat)? = null,
    private val acting: (suspend (AgentAct, AgentKey) -> Result<Unit>)? = null,
    private val warning: (suspend (AgentKey) -> Result<DeleteWarning?>)? = null,
    private val beginPairOp: (suspend (PairRequest) -> Result<PendingPair>)? = null,
    private val abandonPairOp: (suspend (PendingPair) -> Result<Unit>)? = null,
) {
    val fleet: FleetStore = FleetStore(clock = clock)
    val hosts: HostsStore = HostsStore(scope = scope, clock = clock)
    val pairing: PairingStore = PairingStore(clock = clock)
    val offers: SignInOffers = SignInOffers()

    private val _applied = MutableStateFlow(0)
    val applied: StateFlow<Int> = _applied.asStateFlow()

    private val _profileReady = MutableStateFlow(false)
    val profileReady: StateFlow<Boolean> = _profileReady.asStateFlow()

    var profile: Profile? = null
        private set

    var saw: ((hosts: Int, attention: Int) -> Unit)? = null
    var trusts: (() -> Unit)? = null

    private var listedView: FleetView? = null
    private var listedHosts: List<HostView>? = null
    private val chats = mutableMapOf<Long, Pair<OpenChat, (ChatDelta) -> Unit>>()

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
        if (profile != null) {
            apply(profile.fleetOpening())
        }
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

        fleet.show(view, hostViews)
        if (delta.hosts != null) {
            hosts.show(hostViews)
            if (hosts.snapshot.hosts.isNotEmpty()) {
                trusts?.invoke()
            }
        }
        saw?.invoke(fleet.snapshot.machines.count { it.trusted }, fleet.snapshot.needYou)
        _applied.value += 1
        if (delta.hosts != null && hosts.snapshot.rosterBehind) {
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
    fun openChat(agent: AgentKey, onDelta: (ChatDelta) -> Unit): OpenChat {
        val opened = if (opener != null) {
            opener.invoke(agent)
        } else {
            val activeProfile = profile
                ?: throw RuntimeFailure("nothing is running for this account")
            activeProfile.openChat(agent)
        }
        chats[opened.id] = opened to onDelta
        return opened
    }

    fun closeChat(chat: OpenChat) {
        chats.remove(chat.id)
        chat.close()
    }

    fun closeChats() {
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
        val activeProfile = profile ?: return
        if (offers.snapshot.answering != null) return
        offers.answers(offer.hostId)
        scope.launch {
            val outcome = activeProfile.answerSignInOffer(offer.hostId, accept)
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

    suspend fun delete(agent: AgentKey, discardingChanges: Boolean = false): Result<Unit> =
        agentAct(
            if (discardingChanges) AgentAct.DeleteDiscardingChanges else AgentAct.Delete,
            agent,
        )

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
}
