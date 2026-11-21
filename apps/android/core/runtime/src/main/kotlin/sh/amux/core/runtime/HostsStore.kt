package sh.amux.core.runtime

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import sh.amux.values.AccountView
import sh.amux.values.HostView
import sh.amux.values.Offline
import sh.amux.values.PairedPeer
import sh.amux.values.PairingWindowView
import sh.amux.values.Reach
import sh.amux.values.Roster

/** Delegate wired by [StoreBundle] for host management actions. */
interface HostActions {
    fun unpair(host: HostId)
    fun offerSignIn(host: HostView)
    suspend fun offerPairing(ttlSeconds: Long = 0L): Result<PairingWindowView>
}

/** Immutable UI snapshot of known, paired, and nearby hosts. */
data class HostsState(
    val hosts: List<HostView> = emptyList(),
    val discovered: List<HostView> = emptyList(),
    val local: HostView? = null,
    val roster: Roster? = null,
    val networkExplained: Boolean = true,
    val account: AccountView? = null,
    val declinedWindows: Set<HostId> = emptySet(),
    val heldAway: Set<HostId> = emptySet(),
    val connecting: Set<HostId> = emptySet(),
) {
    val online: List<HostView>
        get() = hosts.filter { it.online }

    val offline: List<HostView>
        get() = hosts.filter { !it.online }

    val awayHost: HostView?
        get() = hosts
            .filter { host -> host.id?.let { it in heldAway } == true }
            .minByOrNull { it.name.lowercase() }

    /** Nearby host with an open pairing window not yet dismissed by the user. */
    val windowToOffer: HostView?
        get() = discovered.firstOrNull { host ->
            val id = host.id
            host.windowOpen && id != null && id !in declinedWindows
        }

    /** Trusted hosts seen by discovery that currently have no live route. */
    val foundButUnreachable: Set<HostId>
        get() {
            val seen = hosts.filter { it.addrs.isNotEmpty() }.mapNotNull { it.id }.toSet()
            return hosts
                .filter { it.group == HostGroup.Offline && it.id in seen }
                .mapNotNull { it.id }
                .toSet()
        }

    val devices: List<PairedPeer>
        get() = roster?.peers.orEmpty()

    /** True if the roster has not been loaded or is missing any trusted host. */
    val rosterBehind: Boolean
        get() {
            val currentRoster = roster ?: return true
            val known = currentRoster.peers.map { it.hostId }.toSet()
            return hosts.any { it.trusted && it.hostId !in known }
        }

    fun isConnecting(id: HostId?): Boolean = id != null && id in connecting

    fun hosts(group: HostGroup): List<HostView> = hosts.filter { it.group == group }

    fun candidates(group: HostGroup): List<HostView> = discovered.filter { it.group == group }

    fun host(id: HostId): HostView? =
        hosts.firstOrNull { it.id == id } ?: local?.takeIf { it.id == id }

    fun known(id: HostId): HostView? =
        hosts.firstOrNull { it.id == id } ?: discovered.firstOrNull { it.id == id }
}

/**
 * Observable store of paired hosts, nearby discovered candidates, local
 * identity roster, and foreground link-settling state.
 */
class HostsStore(
    private val scope: CoroutineScope? = null,
    private val clock: () -> Long = System::currentTimeMillis,
    private val connectingPatienceMs: Long = CONNECTING_PATIENCE_MS,
) {
    private val _state = MutableStateFlow(HostsState())
    val state: StateFlow<HostsState> = _state.asStateFlow()

    val snapshot: HostsState
        get() = _state.value

    internal var actions: HostActions? = null

    private var unsettled: MutableSet<HostId>? = null

    /**
     * The machines read linked as the app came forward, with the link each
     * was read on. Those links went while the app was away, but a listing
     * can still say they are up until the runtime has heard them go, so a
     * machine is settled by being read linked only on a newer link. The
     * listings in between are not all read, so one saying the old link went
     * may never be seen.
     */
    private var linkedBefore: MutableMap<HostId, Long> = mutableMapOf()
    private var patienceJob: Job? = null
    private val departures = mutableMapOf<HostId, Long>()

    init {
        cameForward()
    }

    val nowMs: Long
        get() = clock()

    /**
     * Called whenever the app enters the foreground: links were dropped while
     * backgrounded, so previously offline hosts without a dial error settle
     * gracefully as "connecting" until they answer, fail, or patience elapses.
     *
     * The machines listed last are settled again at once, so what is drawn
     * as connecting never waits on the next listing: a machine stopped while
     * the app was away changes nothing the phone hears until its dial fails,
     * and nothing else may be listed before then. Whether such a machine
     * reads connecting or away then depends neither on the order the app
     * heard its link go and came forward in, nor on anything listed after.
     */
    fun cameForward() {
        unsettled = null
        val hosts = _state.value.hosts
        linkedBefore = hosts.filter { it.online }
            .mapNotNull { host -> host.id?.let { it to host.link } }
            .toMap(mutableMapOf())
        if (hosts.isNotEmpty()) {
            val connecting = settle(hosts)
            _state.update { it.copy(connecting = connecting) }
        }
        patienceJob?.cancel()
        patienceJob = scope?.launch {
            delay(connectingPatienceMs)
            stopWaiting()
        }
    }

    /** Clears all pending "connecting" states immediately. */
    fun stopWaiting() {
        patienceJob?.cancel()
        patienceJob = null
        unsettled = mutableSetOf()
        linkedBefore = mutableMapOf()
        _state.update { it.copy(connecting = emptySet()) }
    }

    /** Applies the latest host list from a fleet opening or delta. */
    fun show(views: List<HostView>) {
        val localHost = views.firstOrNull { it.local }
        val trusted = views.filter { it.trusted && !it.local }
        val previousOnline = buildMap {
            for (host in _state.value.hosts) {
                host.id?.let { put(it, host.online) }
            }
        }
        val now = clock()
        for (host in trusted) {
            val id = host.id ?: continue
            if (host.online) {
                departures.remove(id)
            } else if (previousOnline[id] == true) {
                departures[id] = now
            }
        }
        val listedIds = trusted.mapNotNull { it.id }.toSet()
        departures.keys.retainAll(listedIds)

        val nextConnecting = settle(trusted)
        val sortedTrusted = trusted.sortedWith(
            compareBy<HostView>({ if (it.online) 0 else 1 }, { it.name.lowercase() }),
        )
        val sortedDiscovered = views
            .filter { it.candidate && !it.local }
            .sortedBy { it.name.lowercase() }

        val unreached = trusted.filter { !it.online }.mapNotNull { it.id }.toSet()
        val awayNow = trusted.filter { it.group == HostGroup.Away }.mapNotNull { it.id }.toSet()
        val openWindows = sortedDiscovered.filter { it.windowOpen }.mapNotNull { it.id }.toSet()

        _state.update { current ->
            current.copy(
                local = localHost,
                hosts = sortedTrusted,
                discovered = sortedDiscovered,
                heldAway = (current.heldAway intersect unreached) union awayNow,
                declinedWindows = current.declinedWindows intersect openWindows,
                connecting = nextConnecting,
            )
        }
    }

    /**
     * Settles every machine a listing says is linked or out of reach for a
     * known reason: a failed dial, a revoked trust, a signed-out phone or a
     * free plan. Settled stays settled until the app next comes forward, so
     * a machine whose dial failed reads away while it is retried. Returns
     * the machines to draw as connecting.
     */
    private fun settle(trusted: List<HostView>): Set<HostId> {
        val ids = trusted.mapNotNull { it.id }.toSet()
        val currentUnsettled = unsettled ?: ids.toMutableSet()
        for (host in trusted) {
            val id = host.id ?: continue
            val before = linkedBefore[id]
            if (host.online && before != null && host.link <= before) continue
            linkedBefore.remove(id)
            val waiting = host.reach == Reach.Offline(Offline.Plain) && host.lastDialError == null
            if (!waiting) {
                currentUnsettled.remove(id)
            }
        }
        currentUnsettled.retainAll(ids)
        linkedBefore.keys.retainAll(ids)
        unsettled = currentUnsettled
        // One still read linked is drawn linked, whatever settles it.
        return currentUnsettled - linkedBefore.keys
    }

    /** Remembers that the user dismissed the auto-prompt for [id]'s current open window. */
    fun declineWindow(id: HostId) {
        if (_state.value.discovered.none { it.id == id && it.windowOpen }) return
        _state.update { it.copy(declinedWindows = it.declinedWindows + id) }
    }

    fun explainedNetwork(explained: Boolean) {
        _state.update { it.copy(networkExplained = explained) }
    }

    fun showRoster(roster: Roster) {
        _state.update { it.copy(roster = roster) }
    }

    fun showAccount(account: AccountView?) {
        _state.update { it.copy(account = account) }
    }

    fun wentOfflineAtMs(id: HostId): Long? = departures[id]

    fun unpair(host: HostId) {
        actions?.unpair(host)
    }

    fun offerSignIn(host: HostView) {
        actions?.offerSignIn(host)
    }

    suspend fun offerPairing(ttlSeconds: Long = 0L): Result<PairingWindowView> =
        actions?.offerPairing(ttlSeconds)
            ?: Result.failure(RuntimeFailure("nothing is running"))

    companion object {
        const val CONNECTING_PATIENCE_MS: Long = 12_000L
    }
}
