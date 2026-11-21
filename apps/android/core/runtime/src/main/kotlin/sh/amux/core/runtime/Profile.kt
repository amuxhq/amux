package sh.amux.core.runtime

import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock
import kotlinx.coroutines.CancellationException
import kotlinx.serialization.encodeToString
import sh.amux.values.AgentAct
import sh.amux.values.AgentKey
import sh.amux.values.Bearer
import sh.amux.values.Catalogue
import sh.amux.values.DeleteWarning
import sh.amux.values.Directories
import sh.amux.values.FamilyHeader
import sh.amux.values.FleetCard
import sh.amux.values.FleetDelta
import sh.amux.values.FleetFilter
import sh.amux.values.NewAgent
import sh.amux.values.OnboardingAnswer
import sh.amux.values.OnboardingState
import sh.amux.values.PairRequest
import sh.amux.values.PairingWindowView
import sh.amux.values.PendingPair
import sh.amux.values.Provider
import sh.amux.values.ReportSent
import sh.amux.values.Roster
import sh.amux.values.SignInAnswer
import sh.amux.values.SignInOfferOutcome

/**
 * One open profile's fleet and chats on [Runtime].
 *
 * Close all chats opened from this profile before closing it, and close this
 * profile before stopping [runtime].
 */
class Profile internal constructor(
    val id: String,
    private val handle: Long,
    val runtime: Runtime,
    private val loop: CoreLoop,
) : ReportSubmitting, AutoCloseable {
    val fleetId: Long = NativeBridge.profileFleetId(handle)

    private val lock = ReentrantLock()
    private var open = true

    val listsSources: Boolean
        get() = runtime.listsSources(id)

    override fun close() {
        val wasOpen = lock.withLock {
            if (!open) return
            open = false
            NativeBridge.profileClose(handle)
            true
        }
        if (wasOpen) {
            loop.unwatchFleet(fleetId)
            runtime.unregisterProfile(this)
        }
    }

    private inline fun <T> withOpen(closed: T, body: (Long) -> T): T = lock.withLock {
        if (open) body(handle) else closed
    }

    // --- Fleet -------------------------------------------------------------

    fun fleetOpening(): FleetDelta = withOpen(FleetDelta(hosts = null, view = null)) { live ->
        val raw = NativeBridge.fleetOpening(live) ?: return@withOpen FleetDelta(hosts = null, view = null)
        Bridge.decodeNullable<FleetDelta>(raw) ?: FleetDelta(hosts = null, view = null)
    }

    fun toggleFamily(head: AgentKey) {
        val rootJson = AmuxJson.json.encodeToString(head.agent)
        withOpen(Unit) { live -> NativeBridge.fleetToggleFamily(live, rootJson) }
    }

    fun filter(filter: FleetFilter) {
        val filterJson = AmuxJson.json.encodeToString(filter)
        withOpen(Unit) { live -> NativeBridge.fleetFilter(live, filterJson) }
    }

    fun setForeground(foreground: Boolean) {
        withOpen(Unit) { live -> NativeBridge.profileSetForeground(live, foreground) }
    }

    fun card(agent: AgentKey): FleetCard? {
        val agentJson = AmuxJson.json.encodeToString(agent)
        return withOpen(null) { live ->
            NativeBridge.fleetCard(live, agentJson)?.let { Bridge.decodeNullable<FleetCard>(it) }
        }
    }

    fun family(agent: AgentKey): FamilyHeader? {
        val agentJson = AmuxJson.json.encodeToString(agent)
        return withOpen(null) { live ->
            NativeBridge.fleetFamily(live, agentJson)?.let { Bridge.decodeNullable<FamilyHeader>(it) }
        }
    }

    // --- Pairing & Roster --------------------------------------------------

    suspend fun beginPair(request: PairRequest): Result<PendingPair> {
        val requestJson = AmuxJson.json.encodeToString(request)
        return act { live -> NativeBridge.profileBeginPair(live, requestJson) }
    }

    suspend fun confirmPair(pending: PendingPair): Result<Unit> {
        val tokenJson = AmuxJson.json.encodeToString(pending.token)
        return actUnit { live -> NativeBridge.profileConfirmPair(live, tokenJson) }
    }

    suspend fun abandonPair(pending: PendingPair): Result<Unit> {
        val tokenJson = AmuxJson.json.encodeToString(pending.token)
        return actUnit { live -> NativeBridge.profileAbandonPair(live, tokenJson) }
    }

    suspend fun offerSignIn(host: HostId): Result<Unit> {
        val hostJson = AmuxJson.json.encodeToString(host.bytes)
        return actUnit { live -> NativeBridge.profileOfferSignIn(live, hostJson) }
    }

    suspend fun answerSignInOffer(hostId: List<Int>, accept: Boolean): Result<SignInOfferOutcome> {
        val answerJson = AmuxJson.json.encodeToString(SignInAnswer(hostId = hostId, accept = accept))
        return act { live -> NativeBridge.profileAnswerSignInOffer(live, answerJson) }
    }

    fun watchOnboarding() {
        withOpen(Unit) { live -> NativeBridge.profileWatchOnboarding(live) }
    }

    suspend fun answerOnboarding(answer: OnboardingAnswer): Result<OnboardingState> {
        val answerJson = AmuxJson.json.encodeToString(answer)
        return act { live -> NativeBridge.profileAnswerOnboarding(live, answerJson) }
    }

    suspend fun openPairing(ttlSeconds: Long = 0L): Result<PairingWindowView> =
        act { live -> NativeBridge.profileOpenPairing(live, ttlSeconds) }

    suspend fun closePairing(): Result<Unit> =
        actUnit { live -> NativeBridge.profileClosePairing(live) }

    suspend fun unpair(host: HostId): Result<Unit> {
        val hostJson = AmuxJson.json.encodeToString(host.bytes)
        return actUnit { live -> NativeBridge.profileUnpair(live, hostJson) }
    }

    suspend fun roster(): Result<Roster> = act { live -> NativeBridge.profileRoster(live) }

    suspend fun accessToken(): Result<Bearer> = runtime.accessToken(id)

    suspend fun refreshEntitlement(): Result<Unit> = runtime.refreshEntitlement(id)

    // --- Reports & Diagnostics ---------------------------------------------

    suspend fun dump(reason: String): Result<String> =
        act { live -> NativeBridge.profileDump(live, reason) }

    override suspend fun submitReport(upload: ReportUpload): Result<ReportSent> =
        act { live ->
            NativeBridge.profileSubmitReport(live, upload.path, upload.header, upload.frame)
        }

    // --- Agents ------------------------------------------------------------

    suspend fun createAgent(agent: NewAgent): Result<AgentKey> {
        val agentJson = AmuxJson.json.encodeToString(agent)
        return act { live -> NativeBridge.profileCreateAgent(live, agentJson) }
    }

    suspend fun hostCatalogue(host: HostId, provider: Provider): Result<Catalogue> {
        val hostJson = AmuxJson.json.encodeToString(host.bytes)
        val providerJson = AmuxJson.json.encodeToString(provider)
        return act { live -> NativeBridge.profileHostCatalogue(live, hostJson, providerJson) }
    }

    suspend fun directories(host: HostId, query: String, limit: Int): Result<Directories> {
        val hostJson = AmuxJson.json.encodeToString(host.bytes)
        return act { live -> NativeBridge.profileDirectories(live, hostJson, query, limit) }
    }

    suspend fun pinFolder(host: HostId, path: String, pinned: Boolean): Result<Unit> {
        val hostJson = AmuxJson.json.encodeToString(host.bytes)
        return actUnit { live -> NativeBridge.profilePinFolder(live, hostJson, path, pinned) }
    }

    suspend fun act(act: AgentAct, agent: AgentKey): Result<Unit> {
        val agentJson = AmuxJson.json.encodeToString(agent)
        val actJson = AmuxJson.json.encodeToString(act)
        return actUnit { live -> NativeBridge.profileAgentAct(live, agentJson, actJson) }
    }

    suspend fun deleteWarning(agent: AgentKey): Result<DeleteWarning?> {
        val agentJson = AmuxJson.json.encodeToString(agent)
        return actNullable { live -> NativeBridge.profileDeleteWarning(live, agentJson) }
    }

    // --- Chats -------------------------------------------------------------

    /**
     * Opens [agent]'s chat on the core thread without holding the caller's
     * thread. A caller cancelled meanwhile has the chat closed.
     */
    @Throws(RuntimeFailure::class)
    suspend fun openChat(agent: AgentKey, tail: Int = 0): Chat {
        val agentJson = AmuxJson.json.encodeToString(agent)
        return try {
            loop.runOnCore(dispose = Chat::close) {
                // Opening a chat can take a while, and the main thread takes
                // this lock on every fleet delta, so the open runs outside it.
                // A profile closed meanwhile is safe to call: the native side
                // refuses a closed handle, and closes a chat that opened as
                // its profile closed.
                val live = lock.withLock { if (open) handle else 0L }
                if (live == 0L) {
                    throw RuntimeFailure("the chat did not open")
                }
                val chatHandle = NativeBridge.sessionOpen(live, agentJson, tail)
                if (chatHandle == 0L) {
                    throw RuntimeFailure("the chat did not open")
                }
                Chat(handle = chatHandle, loop = loop)
            }
        } catch (c: CancellationException) {
            throw c
        } catch (f: RuntimeFailure) {
            throw f
        } catch (t: Throwable) {
            throw RuntimeFailure(t.message ?: "the chat did not open")
        }
    }

    // --- Internal act helpers ----------------------------------------------

    private suspend inline fun <reified T> act(
        noinline start: (Long) -> Long,
    ): Result<T> {
        val raw = try {
            loop.awaitAct { withOpen(0L, start) }
        } catch (c: CancellationException) {
            throw c
        } catch (t: Throwable) {
            return Result.failure(RuntimeFailure(t.failureDescription))
        }
        return Bridge.decodeAnswer(raw)
    }

    private suspend inline fun <reified T> actNullable(
        noinline start: (Long) -> Long,
    ): Result<T?> {
        val raw = try {
            loop.awaitAct { withOpen(0L, start) }
        } catch (c: CancellationException) {
            throw c
        } catch (t: Throwable) {
            return Result.failure(RuntimeFailure(t.failureDescription))
        }
        return Bridge.decodeNullableAnswer(raw)
    }

    private suspend fun actUnit(start: (Long) -> Long): Result<Unit> {
        val raw = try {
            loop.awaitAct { withOpen(0L, start) }
        } catch (c: CancellationException) {
            throw c
        } catch (t: Throwable) {
            return Result.failure(RuntimeFailure(t.failureDescription))
        }
        return Bridge.decodeUnitAnswer(raw)
    }
}
