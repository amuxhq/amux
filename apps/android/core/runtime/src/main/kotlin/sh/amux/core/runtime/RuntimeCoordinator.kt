package sh.amux.core.runtime

import java.io.File
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import sh.amux.values.Found
import sh.amux.values.ProfileView
import sh.amux.values.StartConfig
import sh.amux.values.UsageEvent

/**
 * Starts an installation, delivering profile updates on [onProfilesMoved] and,
 * should the runtime stop by itself, why on [onStopped].
 */
fun interface RuntimeStarter {
    @Throws(RuntimeFailure::class)
    fun start(
        config: StartConfig,
        dispatch: (() -> Unit) -> Unit,
        onProfilesMoved: (List<ProfileView>) -> Unit,
        onStopped: (String) -> Unit,
    ): Runtime
}

data class CoordinatorState(
    val profiles: List<ProfileView> = emptyList(),
    val activeProfileId: String? = null,
    val failure: String? = null,
    val storeFailure: String? = null,
    val active: Boolean = true,
    val shareUsage: Boolean = true,
    val autoReports: Boolean = false,
)

/**
 * Single per-process owner of [Runtime] (and its [CoreLoop]), managing the
 * active profile's [StoreBundle], source policy, ordered lifecycle feeds,
 * local discovery forwarding, entitlement refreshes, analytics toggles, and
 * store-failure relaunch recovery.
 *
 * [scope] runs on the main thread: everything the coordinator holds is read
 * and written there. Native work that can take a while, starting and
 * stopping the runtime and opening a profile, runs on the core thread or
 * [ioDispatcher] while the screens draw what they last held.
 */
class RuntimeCoordinator(
    val registry: AccountRegistry,
    val installationDir: File,
    val deviceName: String,
    val signedOutStores: StoreBundle,
    private val scope: CoroutineScope,
    private val ioDispatcher: CoroutineDispatcher = Dispatchers.IO,
    private val dispatch: (() -> Unit) -> Unit = { it() },
    private val options: Options = Options(),
    private val networkWatch: NetworkWatcher = NoOpNetworkWatcher,
    var discovery: DiscoverySource? = null,
    var entitlementWatch: EntitlementWatch? = null,
    var pushRegistrar: PushRegistrar = NoOpPushRegistrar(),
    private val starter: RuntimeStarter = RuntimeStarter { config, disp, onProfilesMoved, onStopped ->
        Runtime.start(
            config = config,
            dispatch = disp,
            onProfilesMoved = onProfilesMoved,
            onStopped = onStopped,
        )
    },
) {
    /** Options used by tests or debug driving builds to override network endpoints. */
    data class Options(
        val discoveryScope: String = "",
        val lan: Boolean = true,
        val lanBind: String? = null,
        val relayTcp: String? = null,
        val relayQuic: String? = null,
        val relayRoot: String? = null,
    )

    private val _state = MutableStateFlow(CoordinatorState())
    val state: StateFlow<CoordinatorState> = _state.asStateFlow()

    val snapshot: CoordinatorState
        get() = _state.value

    private val _storesFlow = MutableStateFlow(registry.stores ?: signedOutStores)
    val storesFlow: StateFlow<StoreBundle> = _storesFlow.asStateFlow()

    val stores: StoreBundle
        get() = registry.stores ?: signedOutStores

    var runtime: Runtime? = null
        private set

    var profile: Profile? = null
        private set

    var storeFailureChanged: ((String?) -> Unit)? = null
    var accountChanged: (() -> Unit)? = null

    private val _fedLifecycles = mutableListOf<Lifecycle>()
    val fedLifecycles: List<Lifecycle>
        get() = synchronized(_fedLifecycles) { _fedLifecycles.toList() }

    private var found: List<Found> = emptyList()
    private var fedBundle: StoreBundle? = null
    private var shownBundle: StoreBundle = registry.stores ?: signedOutStores
    private var openingGeneration: Int = 0
    private var launchGeneration: Int = 0
    private var startingJob: Job? = null
    private var lastFedJob: Job? = null
    private var opening: Job? = null
    private var openingTarget: Pair<String, StoreBundle>? = null

    // Stopping the native runtime joins its threads, which can take seconds,
    // so it runs here; the next start waits for it, as both use one store.
    private val teardownScope = CoroutineScope(SupervisorJob() + ioDispatcher)
    private var teardown: Job? = null
    private val waiterLock = Any()
    private val startWaiters = mutableListOf<CompletableDeferred<Runtime?>>()

    init {
        registry.switching = { switched() }
        registry.changed = { pushRegistrar.updateAccounts(pushAccounts()) }
    }

    private fun switched() {
        val current = stores
        _storesFlow.value = current
        if (shownBundle !== current) {
            shownBundle = current
            accountChanged?.invoke()
        }
        place()
    }

    // --- Startup & Shutdown ------------------------------------------------

    /** Starts the embedded installation if not already running or starting. */
    fun start() {
        val expectedLaunch = synchronized(waiterLock) {
            if (runtime != null || startingJob != null) return
            launchGeneration += 1
            launchGeneration
        }
        val dir = installationDir
        val currentState = _state.value
        val config = StartConfig(
            dataDir = dir.absolutePath,
            deviceName = deviceName,
            autoReports = currentState.autoReports,
            discoveryScope = options.discoveryScope,
            lan = options.lan,
            lanBind = options.lanBind,
            logPath = File(dir, "runtime.log").absolutePath,
            relayQuic = options.relayQuic,
            relayRoot = options.relayRoot,
            relayTcp = options.relayTcp,
            tail = null,
            telemetry = currentState.shareUsage,
        )
        val job = scope.launch(start = CoroutineStart.LAZY) {
            teardown?.join()
            var startedRuntime: Runtime? = null
            try {
                val result: Result<Runtime> = withContext(ioDispatcher) {
                    try {
                        dir.mkdirs()
                        val created = starter.start(
                            config,
                            dispatch,
                            { listed -> profilesMoved(listed, expectedLaunch) },
                            { reason -> stoppedByItself(reason, expectedLaunch) },
                        )
                        startedRuntime = created
                        Result.success(created)
                    } catch (f: RuntimeFailure) {
                        Result.failure(f)
                    } catch (t: Throwable) {
                        Result.failure(RuntimeFailure(t.message ?: t.toString()))
                    }
                }
                if (onStarted(result, expectedLaunch)) {
                    startedRuntime = null
                }
            } finally {
                val orphan = startedRuntime
                if (orphan != null) {
                    withContext(NonCancellable + ioDispatcher) {
                        orphan.stop()
                    }
                }
            }
        }
        val shouldStart = synchronized(waiterLock) {
            if (expectedLaunch == launchGeneration && runtime == null) {
                startingJob = job
                true
            } else {
                false
            }
        }
        if (shouldStart) {
            job.start()
        } else {
            job.cancel()
        }
    }

    /**
     * Hands a freshly started [Runtime] to this coordinator.
     *
     * Returns `true` if ownership transferred to `this.runtime` (or no runtime was
     * created), or `false` if this launch was superseded so the caller must stop it.
     */
    private fun onStarted(result: Result<Runtime>, expectedLaunch: Int): Boolean {
        synchronized(waiterLock) {
            if (expectedLaunch != launchGeneration) {
                return false
            }
            startingJob = null
            result.getOrNull()?.let { runtime = it }
        }
        result.fold(
            onSuccess = { startedRuntime ->
                updateFailures(failure = null, storeFailure = null)
                val current = _state.value
                startedRuntime.setTelemetry(current.shareUsage)
                startedRuntime.setAutoReports(current.autoReports)
                if (current.active) {
                    feed(startedRuntime, Lifecycle.Foreground)
                    discovery?.start()
                }
                networkWatch.start {
                    dispatch { networkChanged() }
                }
                if (found.isNotEmpty()) {
                    startedRuntime.discovered(found)
                }
                if (current.profiles.isNotEmpty()) {
                    registry.show(current.profiles)
                    place()
                }
                completeWaiters(startedRuntime)
            },
            onFailure = { error ->
                val reason = error.failureDescription
                updateFailures(failure = reason, storeFailure = reason)
                completeWaiters(null)
            },
        )
        return true
    }

    /** Suspends until startup finishes, returning the running [Runtime] or `null` on failure. */
    suspend fun awaitStarted(): Runtime? {
        val waiter = synchronized(waiterLock) {
            runtime?.let { return it }
            if (startingJob == null) return null
            CompletableDeferred<Runtime?>().also { startWaiters.add(it) }
        }
        return waiter.await()
    }

    private fun completeWaiters(value: Runtime?) {
        val pending = synchronized(waiterLock) {
            val copy = startWaiters.toList()
            startWaiters.clear()
            copy
        }
        for (waiter in pending) {
            waiter.complete(value)
        }
    }

    /**
     * Stops the installation, closing any open profile and chats first. The
     * screens let go of the runtime at once; the native stop finishes on
     * [ioDispatcher], and the returned job completes when it has.
     */
    fun stop(): Job {
        closeProfile()
        val jobToCancel: Job?
        val runtimeToStop: Runtime?
        synchronized(waiterLock) {
            launchGeneration += 1
            jobToCancel = startingJob
            startingJob = null
            runtimeToStop = runtime
            runtime = null
        }
        jobToCancel?.cancel()
        completeWaiters(null)
        networkWatch.stop()
        discovery?.stop()
        _state.update { it.copy(profiles = emptyList(), activeProfileId = null) }
        val previous = teardown
        val job = teardownScope.launch {
            previous?.join()
            // A start cancelled here may still hand back a runtime, which its
            // own job then stops; waiting for that job keeps a later start
            // from opening the installation while that runtime still holds it.
            jobToCancel?.join()
            runtimeToStop?.stop()
        }
        teardown = job
        return job
    }

    /** Recovers from a store failure by clearing the error, stopping, and starting afresh. */
    fun relaunch() {
        updateFailures(failure = null, storeFailure = null)
        stop()
        start()
    }

    /**
     * The runtime stopped without being asked to: its bridge failed or it
     * went away. Every act would fail from here, so the screens show the
     * failure and offer to relaunch rather than draw a fleet that no longer
     * moves.
     */
    private fun stoppedByItself(reason: String, expectedLaunch: Int) {
        if (expectedLaunch != launchGeneration) return
        stop()
        updateFailures(failure = reason, storeFailure = reason)
    }

    private fun updateFailures(failure: String?, storeFailure: String?) {
        val previousStoreFailure = _state.value.storeFailure
        _state.update { it.copy(failure = failure, storeFailure = storeFailure) }
        if (previousStoreFailure != storeFailure) {
            storeFailureChanged?.invoke(storeFailure)
        }
    }

    // --- Profiles & Fleet Placement ----------------------------------------

    private fun profilesMoved(listed: List<ProfileView>, expectedLaunch: Int) {
        if (expectedLaunch != launchGeneration) return
        val previousIds = _state.value.profiles.map { it.id }.toSet()
        val added = listed.map { it.id }.toSet() - previousIds
        _state.update { it.copy(profiles = listed) }
        val activeRuntime = runtime ?: return
        if (added.isNotEmpty() && found.isNotEmpty()) {
            activeRuntime.discovered(found)
        }
        registry.show(listed)
        place()
        pushRegistrar.updateAccounts(pushAccounts())
    }

    private fun place() {
        val activeRuntime = runtime ?: return
        val profileId = registry.profile ?: return
        val activeStores = stores
        _storesFlow.value = activeStores

        val target = profileId to activeStores
        val placed = profile?.id == profileId && fedBundle === activeStores
        if (!placed && openingTarget != target) {
            closeProfile()
            openingGeneration += 1
            val expectedOpening = openingGeneration
            openingTarget = target
            opening = scope.launch {
                val opened = try {
                    activeRuntime.openProfile(profileId) { event ->
                        if (openingGeneration == expectedOpening) {
                            activeStores.moved(event)
                        }
                    }
                } catch (f: RuntimeFailure) {
                    if (openingGeneration == expectedOpening) {
                        openingTarget = null
                        _state.update { it.copy(failure = f.description) }
                    }
                    return@launch
                }
                if (openingGeneration != expectedOpening || runtime !== activeRuntime) {
                    opened.close()
                    return@launch
                }
                openingTarget = null
                profile = opened
                fedBundle = activeStores
                _state.update { it.copy(activeProfileId = profileId) }
                activeStores.hosts.explainedNetwork(true)
                activeStores.attach(opened)
                applySourcePolicy()
            }
        }
        _state.value.profiles.firstOrNull { it.id == profileId }?.let { view ->
            activeStores.showAccount(view.account)
        }
        applySourcePolicy()
    }

    private fun applySourcePolicy() {
        val activeRuntime = runtime ?: return
        val isActive = _state.value.active
        activeRuntime.listSources(if (isActive) registry.profile else null)
        profile?.setForeground(isActive)
    }

    private fun closeProfile() {
        openingGeneration += 1
        // A cancelled open closes the profile it opened on the core thread.
        opening?.cancel()
        opening = null
        openingTarget = null
        fedBundle?.closeChats()
        fedBundle?.attach(null)
        fedBundle = null
        profile?.close()
        profile = null
        _state.update { it.copy(activeProfileId = null) }
    }

    // --- Ordered Lifecycle Feeds -------------------------------------------

    /**
     * Feeds a lifecycle moment strictly in decision order so a foreground
     * event decided after a suspend can never be overtaken by that suspend.
     */
    private fun feed(activeRuntime: Runtime, input: Lifecycle): Job {
        synchronized(_fedLifecycles) {
            _fedLifecycles.add(input)
        }
        val previous = lastFedJob
        val job = scope.launch {
            previous?.join()
            activeRuntime.feed(input)
        }
        lastFedJob = job
        return job
    }

    fun setActive(active: Boolean) {
        if (_state.value.active == active) return
        _state.update { it.copy(active = active) }
        applySourcePolicy()
        if (active) {
            discovery?.start()
            stores.hosts.cameForward()
            runtime?.let { feed(it, Lifecycle.Foreground) }
            entitlementWatch?.let { watch ->
                scope.launch { watch.look(force = false) }
            }
            start()
        } else {
            discovery?.stop()
            runtime?.let { feed(it, Lifecycle.Background) }
        }
    }

    /** Flushes pending analytics and checkpoints every profile's SQLite WAL. */
    suspend fun suspending() {
        val activeRuntime = runtime ?: return
        feed(activeRuntime, Lifecycle.Suspending).join()
    }

    /** Moves links onto the new network after a connectivity handover. */
    fun networkChanged() {
        val activeRuntime = runtime ?: return
        feed(activeRuntime, Lifecycle.NetworkChanged)
    }

    /** Hands discovered LAN peers to every profile and replays them to future profiles. */
    fun discovered(hosts: List<Found>) {
        found = hosts
        runtime?.discovered(hosts)
    }

    // --- Analytics & Reports Settings --------------------------------------

    fun setShareUsage(on: Boolean) {
        if (_state.value.shareUsage == on) return
        _state.update { it.copy(shareUsage = on) }
        runtime?.setTelemetry(on)
    }

    fun setAutoReports(on: Boolean) {
        if (_state.value.autoReports == on) return
        _state.update { it.copy(autoReports = on) }
        runtime?.setAutoReports(on)
    }

    fun record(event: UsageEvent) {
        runtime?.record(event, profileId = registry.profile)
    }

    // --- Account Operations ------------------------------------------------

    suspend fun bind(
        account: AccountId,
        cloudUrl: String,
        clientId: String,
        refreshToken: String,
    ): Result<Unit> {
        val activeRuntime = awaitStarted()
            ?: return Result.failure(RuntimeFailure("this phone’s runtime did not start"))
        val knownProfile = registry.accounts.firstOrNull { it.id == account }?.profile
        val target = knownProfile ?: if (registry.selected == null) registry.unbound else null
        return activeRuntime.bind(target, cloudUrl, clientId, refreshToken).map { view ->
            registry.select(AccountId(view.subject))
        }
    }

    suspend fun signOut(account: AccountId) {
        val activeRuntime = awaitStarted() ?: return
        val entry = registry.accounts.firstOrNull { it.id == account } ?: return
        pushRegistrar.withdraw(account)
        activeRuntime.signOut(entry.profile)
    }

    /**
     * Removes an account from this phone by deleting its profile. Guarantees
     * at least one profile remains by creating a fresh unbound profile first
     * when removing the last account.
     */
    suspend fun remove(account: AccountId) {
        val activeRuntime = awaitStarted() ?: return
        val entry = registry.accounts.firstOrNull { it.id == account } ?: return
        pushRegistrar.withdraw(account)
        if (_state.value.profiles.size == 1) {
            activeRuntime.createProfile()
        }
        registry.leave(account)
        activeRuntime.deleteProfile(entry.profile)
    }

    /** Puts an account trusting [host] on screen, answering whether its profile is open. */
    suspend fun bringForward(host: HostId): Boolean {
        val trustingProfiles = trusting(host)
        val onScreen = registry.profile
        if (onScreen != null && onScreen in trustingProfiles) {
            opening?.join()
            return profile?.id == onScreen
        }
        val matchingAccounts = registry.accounts.filter { it.profile in trustingProfiles }
        val chosen = matchingAccounts.firstOrNull { it.signedIn } ?: matchingAccounts.firstOrNull()
            ?: return false
        registry.select(chosen.id)
        opening?.join()
        return profile?.id == chosen.profile
    }

    private suspend fun trusting(host: HostId): List<String> {
        val activeRuntime = awaitStarted() ?: return emptyList()
        return activeRuntime.trusting(host).getOrDefault(emptyList())
    }

    suspend fun bearer(account: AccountId): String? {
        val activeRuntime = runtime ?: return null
        val entry = registry.accounts.firstOrNull { it.id == account } ?: return null
        return activeRuntime.accessToken(entry.profile).getOrNull()?.bearer
    }

    suspend fun refreshEntitlement(account: AccountId): Boolean {
        val activeRuntime = runtime ?: return false
        val entry = registry.accounts.firstOrNull { it.id == account } ?: return false
        return activeRuntime.refreshEntitlement(entry.profile).isSuccess
    }

    suspend fun pushHost(profileId: String): HostId? {
        val activeRuntime = runtime ?: return null
        val bytes = activeRuntime.hostId(profileId).getOrNull() ?: return null
        return HostId.fromBytes(bytes)
    }

    suspend fun wantNotifications(profileId: String, key: List<Int>?): Boolean {
        val activeRuntime = runtime ?: return false
        return activeRuntime.wantNotifications(profileId, key).isSuccess
    }

    fun pushAccounts(): List<PushAccount> {
        val currentProfiles = _state.value.profiles
        return registry.accounts.map { entry ->
            val pro = currentProfiles.firstOrNull { it.id == entry.profile }?.account?.pro
            PushAccount(
                account = entry.id,
                profile = entry.profile,
                wanted = PushAccount.wanted(
                    signedIn = entry.signedIn,
                    pro = pro,
                    entitlement = entry.entitlement,
                ),
            )
        }
    }
}
