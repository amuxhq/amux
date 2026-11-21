package sh.amux.core.runtime

import android.content.Context
import android.os.Handler
import android.os.Looper
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

/** Starts an installation and delivers profile updates on [onProfilesMoved]. */
fun interface RuntimeStarter {
    @Throws(RuntimeFailure::class)
    fun start(
        config: StartConfig,
        dispatch: (() -> Unit) -> Unit,
        onProfilesMoved: (List<ProfileView>) -> Unit,
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
 */
class RuntimeCoordinator(
    val registry: AccountRegistry,
    val installationDir: File,
    val deviceName: String,
    val signedOutStores: StoreBundle,
    private val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default),
    private val ioDispatcher: CoroutineDispatcher = Dispatchers.IO,
    private val dispatch: (() -> Unit) -> Unit = { it() },
    private val options: Options = Options(),
    private val networkWatch: NetworkWatcher = NoOpNetworkWatcher,
    var discovery: DiscoverySource? = null,
    var entitlementWatch: EntitlementWatch? = null,
    var pushRegistrar: PushRegistrar = NoOpPushRegistrar(),
    private val starter: RuntimeStarter = RuntimeStarter { config, disp, onProfilesMoved ->
        Runtime.start(
            config = config,
            dispatch = disp,
            onProfilesMoved = onProfilesMoved,
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
            var startedRuntime: Runtime? = null
            try {
                val result: Result<Runtime> = withContext(ioDispatcher) {
                    try {
                        dir.mkdirs()
                        val created = starter.start(config, dispatch) { listed ->
                            profilesMoved(listed, expectedLaunch)
                        }
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

    /** Stops the installation cleanly, closing any open profile and chats first. */
    fun stop() {
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
        runtimeToStop?.stop()
        _state.update { it.copy(profiles = emptyList(), activeProfileId = null) }
    }

    /** Recovers from a store failure by clearing the error, stopping, and starting afresh. */
    fun relaunch() {
        updateFailures(failure = null, storeFailure = null)
        stop()
        start()
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

        if (profile?.id != profileId || fedBundle !== activeStores) {
            closeProfile()
            openingGeneration += 1
            val expectedOpening = openingGeneration
            try {
                val opened = activeRuntime.openProfile(profileId) { event ->
                    if (openingGeneration == expectedOpening) {
                        activeStores.moved(event)
                    }
                }
                profile = opened
                fedBundle = activeStores
                _state.update { it.copy(activeProfileId = profileId) }
                discovery?.permission?.value?.let { activeStores.hosts.sawLocalNetwork(it) }
                activeStores.hosts.explainedNetwork(true)
                activeStores.attach(opened)
            } catch (f: RuntimeFailure) {
                _state.update { it.copy(failure = f.description) }
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

    suspend fun bringForward(host: HostId): Boolean {
        val trustingProfiles = trusting(host)
        val onScreen = registry.profile
        if (onScreen != null && onScreen in trustingProfiles) {
            return profile?.id == onScreen
        }
        val matchingAccounts = registry.accounts.filter { it.profile in trustingProfiles }
        val chosen = matchingAccounts.firstOrNull { it.signedIn } ?: matchingAccounts.firstOrNull()
            ?: return false
        registry.select(chosen.id)
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

    companion object {
        /** Creates a production [RuntimeCoordinator] wired to Android system services. */
        fun create(
            context: Context,
            registry: AccountRegistry,
            deviceName: String,
            signedOutStores: StoreBundle,
            scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate),
            options: Options = Options(),
            entitlementWatch: EntitlementWatch? = null,
            pushRegistrar: PushRegistrar = NoOpPushRegistrar(),
        ): RuntimeCoordinator {
            val mainHandler = Handler(Looper.getMainLooper())
            val mainDispatch: (() -> Unit) -> Unit = { block ->
                if (Looper.myLooper() == Looper.getMainLooper()) {
                    block()
                } else {
                    mainHandler.post(block)
                }
            }
            val coordinator = RuntimeCoordinator(
                registry = registry,
                installationDir = InstallationPaths.installationDir(context),
                deviceName = deviceName,
                signedOutStores = signedOutStores,
                scope = scope,
                dispatch = mainDispatch,
                options = options,
                networkWatch = AndroidNetworkWatch(context, scope = scope),
                entitlementWatch = entitlementWatch,
                pushRegistrar = pushRegistrar,
            )
            coordinator.discovery = AndroidLocalDiscovery(
                context = context,
                dispatch = mainDispatch,
                handOver = { coordinator.discovered(it) },
            )
            return coordinator
        }
    }
}
