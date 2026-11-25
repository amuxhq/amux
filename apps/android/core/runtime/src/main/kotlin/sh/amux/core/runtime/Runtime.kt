package sh.amux.core.runtime

import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock
import kotlinx.coroutines.CancellationException
import kotlinx.serialization.encodeToString
import sh.amux.values.Bearer
import sh.amux.values.Found
import sh.amux.values.MachineSettings
import sh.amux.values.ProfileView
import sh.amux.values.SettingsChange
import sh.amux.values.StartConfig
import sh.amux.values.UsageEvent

/**
 * The shared embedded Rust runtime hosted in this app's process: one
 * installation with one profile per account.
 *
 * Every blocking or stepping call runs on [CoreLoop]'s dedicated core thread;
 * reads are synchronized against [stop] so no read executes after the native
 * handle is torn down; acts suspend until their ticket answer arrives after
 * the step's deltas.
 */
class Runtime private constructor(
    private val handle: Long,
    internal val loop: CoreLoop,
) : AutoCloseable {
    private val lock = ReentrantLock()
    private var running = true

    // Held strongly: a profile nobody references still owns a native handle,
    // and only closing it here, before the runtime stops, releases it.
    private val openedProfiles = LinkedHashSet<Profile>()

    /**
     * Stops the runtime and flushes every profile's store, closing every
     * profile opened from it first. Safe to call more than once.
     */
    fun stop() {
        val toClose: List<Profile> = lock.withLock {
            if (!running) return
            running = false
            val snapshot = openedProfiles.toList()
            openedProfiles.clear()
            snapshot
        }
        for (profile in toClose) {
            profile.close()
        }
        loop.stop()
    }

    override fun close() {
        stop()
    }

    private inline fun <T> withLive(stopped: T, body: (Long) -> T): T = lock.withLock {
        if (running) body(handle) else stopped
    }

    internal fun registerProfile(profile: Profile) {
        lock.withLock {
            if (running) {
                openedProfiles.add(profile)
            }
        }
    }

    internal fun unregisterProfile(profile: Profile) {
        lock.withLock {
            openedProfiles.remove(profile)
        }
    }

    // --- Profile registry --------------------------------------------------

    /** Creates a new unbound profile. */
    suspend fun createProfile(): Result<ProfileView> =
        act { live -> NativeBridge.runtimeCreateProfile(live) }

    /** Deletes a profile (its key, trust store, and replica rows). Close it first. */
    suspend fun deleteProfile(profileId: String): Result<Unit> =
        actUnit { live -> NativeBridge.runtimeDeleteProfile(live, profileId) }

    /**
     * Binds a profile to an account with the refresh token from a sign-in.
     * Pass `profileId = null` to let the registry pick the account's own
     * profile, an empty unbound profile, or a fresh profile.
     */
    suspend fun bind(
        profileId: String?,
        cloudUrl: String,
        clientId: String,
        refreshToken: String,
    ): Result<ProfileView> = act { live ->
        NativeBridge.runtimeBind(live, profileId, cloudUrl, clientId, refreshToken)
    }

    /** Signs a profile out while keeping it bound to its account locally. */
    suspend fun signOut(profileId: String): Result<ProfileView> =
        act { live -> NativeBridge.runtimeSignOut(live, profileId) }

    /** Every profile whose trust store holds [host], oldest first. */
    suspend fun trusting(host: HostId): Result<List<String>> {
        val hostJson = AmuxJson.json.encodeToString(host.bytes)
        return act { live -> NativeBridge.runtimeTrustingProfiles(live, hostJson) }
    }

    /** Opens pairing mode on [profileId] and returns its QR link (debug-tools builds only). */
    suspend fun offerPairing(profileId: String): Result<String> =
        act { live -> NativeBridge.runtimeOfferPairing(live, profileId) }

    /**
     * Keeps sources open for every listed agent of [profileId]; pass `null`
     * when the app is in the background so only explicitly opened chats keep a
     * source.
     */
    fun listSources(profileId: String?) {
        withLive(Unit) { live -> NativeBridge.runtimeListSources(live, profileId) }
    }

    /** Whether every listed agent of [profileId] keeps a source open now. */
    fun listsSources(profileId: String): Boolean =
        withLive(false) { live -> NativeBridge.runtimeSourcePolicyListed(live, profileId) }

    /** Hands over the whole set of locally discovered machines to every profile. */
    fun discovered(found: List<Found>) {
        val foundJson = AmuxJson.json.encodeToString(found)
        withLive(Unit) { live -> NativeBridge.runtimeDiscovered(live, foundJson) }
    }

    /** Borrows a fresh bearer token for [profileId]'s account service. */
    suspend fun accessToken(profileId: String): Result<Bearer> =
        act { live -> NativeBridge.runtimeAccessToken(live, profileId) }

    /** Returns [profileId]'s host id bytes. */
    suspend fun hostId(profileId: String): Result<List<Int>> =
        act { live -> NativeBridge.runtimeHostId(live, profileId) }

    /** Registers or withdraws a profile's public push key in memory. */
    suspend fun wantNotifications(profileId: String, key: List<Int>?): Result<Unit> {
        val keyJson = if (key == null) "null" else AmuxJson.json.encodeToString(key)
        return actUnit { live -> NativeBridge.runtimeWantNotifications(live, profileId, keyJson) }
    }

    /** Refreshes [profileId]'s relay credential immediately after an entitlement change. */
    suspend fun refreshEntitlement(profileId: String): Result<Unit> =
        actUnit { live -> NativeBridge.runtimeRefreshEntitlement(live, profileId) }

    // --- Settings ----------------------------------------------------------

    suspend fun settings(): Result<MachineSettings> =
        act { live -> NativeBridge.runtimeSettings(live) }

    suspend fun updateSettings(change: SettingsChange): Result<MachineSettings> {
        val changeJson = AmuxJson.json.encodeToString(change)
        return act { live -> NativeBridge.runtimeUpdateSettings(live, changeJson) }
    }

    suspend fun webSignInLink(profileId: String, path: String): Result<String> =
        act { live -> NativeBridge.runtimeWebSignInLink(live, profileId, path) }

    // --- App lifecycle -----------------------------------------------------

    /**
     * Feeds a lifecycle input on the core thread and suspends until the runtime
     * finishes carrying it out (including SQLite WAL checkpoints on [Lifecycle.Suspending]).
     */
    suspend fun feed(input: Lifecycle): Result<Unit> {
        val lifecycleJson = AmuxJson.json.encodeToString(input.wireValue)
        return try {
            loop.runOnCore {
                val live = withLive(0L) { it }
                if (live == 0L) {
                    Result.failure(RuntimeFailure(Bridge.STOPPED_REASON))
                } else {
                    try {
                        NativeBridge.runtimeFeed(live, lifecycleJson)
                        Result.success(Unit)
                    } catch (e: BridgeException) {
                        Result.failure(RuntimeFailure(e.message ?: Bridge.STOPPED_REASON))
                    }
                }
            }
        } catch (c: CancellationException) {
            throw c
        } catch (f: RuntimeFailure) {
            Result.failure(f)
        } catch (t: Throwable) {
            Result.failure(RuntimeFailure(t.message ?: Bridge.STOPPED_REASON))
        }
    }

    // --- Product analytics -------------------------------------------------

    fun setTelemetry(on: Boolean) {
        withLive(Unit) { live -> NativeBridge.runtimeSetTelemetry(live, on) }
    }

    fun setAutoReports(on: Boolean) {
        withLive(Unit) { live -> NativeBridge.runtimeSetAutoReports(live, on) }
    }

    fun record(event: UsageEvent, profileId: String?) {
        val eventJson = AmuxJson.json.encodeToString(event)
        withLive(Unit) { live -> NativeBridge.runtimeRecord(live, profileId, eventJson) }
    }

    // --- Profiles ----------------------------------------------------------

    /**
     * Opens [profileId]'s fleet on the core thread without holding the
     * caller's thread, registering [onEvent] before any later step so no
     * delta is missed. A caller cancelled meanwhile has the profile closed.
     */
    @Throws(RuntimeFailure::class)
    suspend fun openProfile(profileId: String, onEvent: (FleetEvent) -> Unit): Profile {
        return try {
            loop.runOnCore(dispose = Profile::close) {
                val live = withLive(0L) { it }
                if (live == 0L) {
                    throw RuntimeFailure(Bridge.STOPPED_REASON)
                }
                val profileHandle = try {
                    NativeBridge.profileOpen(live, profileId)
                } catch (e: BridgeException) {
                    throw RuntimeFailure(e.message ?: "the profile did not open")
                }
                if (profileHandle == 0L) {
                    throw RuntimeFailure("the profile did not open")
                }
                val opened = Profile(
                    id = profileId,
                    handle = profileHandle,
                    runtime = this,
                    loop = loop,
                )
                registerProfile(opened)
                loop.watchFleet(opened.fleetId, onEvent)
                opened
            }
        } catch (c: CancellationException) {
            throw c
        } catch (f: RuntimeFailure) {
            throw f
        } catch (t: Throwable) {
            throw RuntimeFailure(t.message ?: "the profile did not open")
        }
    }

    // --- Internal act helpers ----------------------------------------------

    private suspend inline fun <reified T> act(
        noinline start: (Long) -> Long,
    ): Result<T> {
        val raw = try {
            loop.awaitAct(start)
        } catch (c: CancellationException) {
            throw c
        } catch (t: Throwable) {
            return Result.failure(RuntimeFailure(t.failureDescription))
        }
        return Bridge.decodeAnswer(raw)
    }

    private suspend fun actUnit(start: (Long) -> Long): Result<Unit> {
        val raw = try {
            loop.awaitAct(start)
        } catch (c: CancellationException) {
            throw c
        } catch (t: Throwable) {
            return Result.failure(RuntimeFailure(t.failureDescription))
        }
        return Bridge.decodeUnitAnswer(raw)
    }

    companion object {
        /**
         * Starts the embedded runtime on [CoreLoop]'s core thread. Blocks until
         * every profile's store is open and the initial step has run.
         *
         * [onStopped] hears, on [dispatch], why the runtime stopped when it
         * stops by itself rather than through [stop].
         */
        @Throws(RuntimeFailure::class)
        fun start(
            config: StartConfig,
            dispatch: (() -> Unit) -> Unit = { it() },
            onProfilesMoved: (List<ProfileView>) -> Unit = {},
            onStopped: (String) -> Unit = {},
        ): Runtime = start(
            config = config,
            loop = CoreLoop(
                dispatch = dispatch,
                onProfilesMoved = onProfilesMoved,
                onStopped = onStopped,
            ),
        )

        /** Starts a runtime on [loop], which tests build over a scripted bridge. */
        @Throws(RuntimeFailure::class)
        internal fun start(config: StartConfig, loop: CoreLoop): Runtime {
            return try {
                val handle = loop.start(config)
                if (handle == 0L) {
                    loop.stop()
                    throw RuntimeFailure("the runtime did not start")
                }
                Runtime(handle = handle, loop = loop)
            } catch (f: RuntimeFailure) {
                loop.stop()
                throw f
            } catch (e: BridgeException) {
                loop.stop()
                throw RuntimeFailure(e.message ?: "the runtime did not start")
            } catch (t: Throwable) {
                loop.stop()
                throw RuntimeFailure(t.message ?: "the runtime did not start")
            }
        }
    }
}
