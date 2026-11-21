package sh.amux.core.runtime

import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import sh.amux.values.ChatDelta
import sh.amux.values.FleetDelta
import sh.amux.values.OnboardingState
import sh.amux.values.PairingStatusView
import sh.amux.values.ProfileView
import sh.amux.values.SignInOfferView
import sh.amux.values.StartConfig

/** What moved in an open profile during a runtime step. */
sealed interface FleetEvent {
    data class Fleet(val delta: FleetDelta) : FleetEvent
    data class Chat(val delta: ChatDelta) : FleetEvent
    data class SignInOffers(val offers: List<SignInOfferView>) : FleetEvent
    data class Onboarding(val state: OnboardingState) : FleetEvent
    data class Pairing(val status: PairingStatusView) : FleetEvent
}

/**
 * Owns the dedicated core executor and wake-fd watcher thread for one
 * [NativeBridge] runtime instance.
 *
 * Every blocking or stepping bridge call (`runtimeStart`, `runtimeStop`,
 * `profileOpen`, `sessionOpen`, `runtimeFeed`, `runtimeStep`) runs on the
 * single core thread. A dedicated wake thread blocks in
 * [NativeBridge.runtimeWaitWake] and submits `step()` onto the core executor
 * whenever the wake descriptor becomes readable, waiting for each step to
 * drain the descriptor before polling again. Stepped UI deltas and act answers
 * are delivered together in one ordered [dispatch] call so continuations
 * awaiting an act always observe the post-delta UI state.
 */
class CoreLoop(
    private val dispatch: (() -> Unit) -> Unit = { it() },
    private val onProfilesMoved: (List<ProfileView>) -> Unit = {},
    private val onError: (Throwable) -> Unit = { error ->
        System.err.println("CoreLoop watcher error: $error")
        error.printStackTrace(System.err)
    },
) : AutoCloseable {
    private val coreThreadRef = ThreadLocal.withInitial { false }
    private val executor = Executors.newSingleThreadExecutor { runnable ->
        Thread(
            {
                coreThreadRef.set(true)
                runnable.run()
            },
            "amux-core",
        ).apply { isDaemon = true }
    }

    private val stopping = AtomicBoolean(false)
    private val fleetListeners = ConcurrentHashMap<Long, (FleetEvent) -> Unit>()
    private val waitingTickets = ConcurrentHashMap<Long, CompletableFuture<String>>()

    @Volatile
    private var runtimeHandle: Long = 0L

    @Volatile
    private var watcherThread: Thread? = null

    val handle: Long
        get() = runtimeHandle

    /**
     * Starts the embedded runtime on the core thread, launches the wake-fd
     * watcher, and performs the initial step so the initial profile list is
     * delivered immediately.
     */
    fun start(config: StartConfig): Long {
        val configJson = AmuxJson.json.encodeToString(config)
        return blocking {
            check(runtimeHandle == 0L) { "CoreLoop has already been started" }
            val started = NativeBridge.runtimeStart(configJson)
            runtimeHandle = started
            startWatcher(started)
            stepOnCore()
            started
        }
    }

    /**
     * Runs [block] on the single core thread and waits for its result.
     * Re-entrant when already executing on the core thread.
     */
    fun <T> blocking(block: () -> T): T {
        if (coreThreadRef.get() == true) {
            return block()
        }
        return try {
            executor.submit(block).get()
        } catch (e: ExecutionException) {
            throw e.cause ?: e
        }
    }

    /**
     * Starts an asynchronous bridge act on the core thread and returns a
     * future that resolves to the raw JSON answer once delivered by a step.
     */
    fun submitAct(start: (Long) -> Long): CompletableFuture<String> {
        val future = CompletableFuture<String>()
        if (stopping.get()) {
            future.complete(STOPPED_ANSWER)
            return future
        }
        try {
            executor.execute {
                val live = runtimeHandle
                if (live == 0L || stopping.get()) {
                    future.complete(STOPPED_ANSWER)
                    return@execute
                }
                val ticket = try {
                    start(live)
                } catch (t: Throwable) {
                    future.completeExceptionally(t)
                    return@execute
                }
                if (ticket == 0L) {
                    future.complete(STOPPED_ANSWER)
                    return@execute
                }
                waitingTickets[ticket] = future
                if (stopping.get()) {
                    waitingTickets.remove(ticket)?.complete(STOPPED_ANSWER)
                }
            }
        } catch (_: RejectedExecutionException) {
            future.complete(STOPPED_ANSWER)
        }
        return future
    }

    fun watchFleet(fleetId: Long, listener: (FleetEvent) -> Unit) {
        fleetListeners[fleetId] = listener
    }

    fun unwatchFleet(fleetId: Long) {
        fleetListeners.remove(fleetId)
    }

    /**
     * Stops the wake watcher, stops the native runtime on the core thread,
     * resolves any pending act tickets as stopped, and shuts down the core
     * executor. Safe to call more than once and safe to invoke from a core
     * callback without self-deadlocking on the watcher thread.
     */
    fun stop() {
        if (!stopping.compareAndSet(false, true)) {
            return
        }
        val live = runtimeHandle
        if (live != 0L) {
            NativeBridge.runtimeInterruptWake(live)
        }
        val watcher = watcherThread
        watcherThread = null
        val onCoreThread = coreThreadRef.get() == true
        if (!onCoreThread && watcher != null && watcher !== Thread.currentThread()) {
            try {
                watcher.join(WATCHER_JOIN_MS)
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
            }
        }
        try {
            blocking {
                val handleToStop = runtimeHandle
                runtimeHandle = 0L
                if (handleToStop != 0L) {
                    NativeBridge.runtimeStop(handleToStop)
                }
                failPendingTickets()
            }
        } finally {
            fleetListeners.clear()
            failPendingTickets()
            executor.shutdown()
        }
    }

    override fun close() {
        stop()
    }

    private fun startWatcher(handle: Long) {
        val thread = Thread(
            {
                try {
                    var consecutiveErrors = 0
                    while (!stopping.get()) {
                        val ready = try {
                            NativeBridge.runtimeWaitWake(handle, -1)
                        } catch (t: Throwable) {
                            if (!stopping.get()) {
                                reportError(t)
                            }
                            break
                        }
                        if (ready < 0 || stopping.get()) {
                            break
                        }
                        if (ready > 0) {
                            try {
                                executor.submit { stepOnCore() }.get()
                                consecutiveErrors = 0
                            } catch (e: RejectedExecutionException) {
                                if (!stopping.get()) {
                                    reportError(e)
                                }
                                break
                            } catch (e: ExecutionException) {
                                val cause = e.cause ?: e
                                if (!stopping.get()) {
                                    reportError(cause)
                                }
                                consecutiveErrors += 1
                                if (cause is Error || cause is BridgeException || consecutiveErrors >= MAX_CONSECUTIVE_WATCHER_ERRORS) {
                                    break
                                }
                            } catch (e: InterruptedException) {
                                if (!stopping.get()) {
                                    Thread.currentThread().interrupt()
                                    reportError(e)
                                }
                                break
                            } catch (t: Throwable) {
                                if (!stopping.get()) {
                                    reportError(t)
                                }
                                break
                            }
                        }
                    }
                } finally {
                    if (stopping.compareAndSet(false, true)) {
                        failPendingTickets()
                    }
                }
            },
            "amux-wake",
        ).apply { isDaemon = true }
        watcherThread = thread
        thread.start()
    }

    private fun reportError(error: Throwable) {
        try {
            onError(error)
        } catch (_: Throwable) {
            // Never allow an error observer to break watcher cleanup.
        }
    }

    private fun stepOnCore() {
        val live = runtimeHandle
        if (live == 0L) {
            return
        }
        val raw = NativeBridge.runtimeStep(live) ?: return
        val stepped = AmuxJson.json.decodeFromString<Stepped>(raw)

        val hasWork = stepped.profiles != null ||
            stepped.fleets.isNotEmpty() ||
            stepped.chats.isNotEmpty() ||
            stepped.signInOffers.isNotEmpty() ||
            stepped.onboarding.isNotEmpty() ||
            stepped.pairing.isNotEmpty() ||
            stepped.answers.isNotEmpty()

        if (hasWork) {
            dispatch {
                try {
                    for (moved in stepped.fleets) {
                        fleetListeners[moved.fleet]?.invoke(FleetEvent.Fleet(moved.delta))
                    }
                    for (moved in stepped.chats) {
                        fleetListeners[moved.fleet]?.invoke(FleetEvent.Chat(moved.delta))
                    }
                    for (moved in stepped.signInOffers) {
                        fleetListeners[moved.fleet]?.invoke(FleetEvent.SignInOffers(moved.offers))
                    }
                    for (moved in stepped.onboarding) {
                        fleetListeners[moved.fleet]?.invoke(FleetEvent.Onboarding(moved.onboarding))
                    }
                    for (moved in stepped.pairing) {
                        fleetListeners[moved.fleet]?.invoke(FleetEvent.Pairing(moved.status))
                    }
                    stepped.profiles?.let(onProfilesMoved)
                } catch (t: Throwable) {
                    for (answer in stepped.answers) {
                        waitingTickets.remove(answer.ticket)?.completeExceptionally(t)
                    }
                    throw t
                }
                for (answer in stepped.answers) {
                    waitingTickets.remove(answer.ticket)?.complete(answer.json)
                }
            }
        }
    }

    private fun failPendingTickets() {
        val iterator = waitingTickets.entries.iterator()
        while (iterator.hasNext()) {
            val entry = iterator.next()
            iterator.remove()
            entry.value.complete(STOPPED_ANSWER)
        }
    }

    @Serializable
    private data class Stepped(
        @SerialName("profiles") val profiles: List<ProfileView>? = null,
        @SerialName("fleets") val fleets: List<FleetMoved> = emptyList(),
        @SerialName("chats") val chats: List<ChatMoved> = emptyList(),
        @SerialName("sign_in_offers") val signInOffers: List<SignInOffersMoved> = emptyList(),
        @SerialName("onboarding") val onboarding: List<OnboardingMoved> = emptyList(),
        @SerialName("pairing") val pairing: List<PairingMoved> = emptyList(),
        @SerialName("answers") val answers: List<StepAnswer> = emptyList(),
    )

    @Serializable
    private data class FleetMoved(
        @SerialName("fleet") val fleet: Long,
        @SerialName("delta") val delta: FleetDelta,
    )

    @Serializable
    private data class ChatMoved(
        @SerialName("fleet") val fleet: Long,
        @SerialName("delta") val delta: ChatDelta,
    )

    @Serializable
    private data class SignInOffersMoved(
        @SerialName("fleet") val fleet: Long,
        @SerialName("offers") val offers: List<SignInOfferView>,
    )

    @Serializable
    private data class OnboardingMoved(
        @SerialName("fleet") val fleet: Long,
        @SerialName("onboarding") val onboarding: OnboardingState,
    )

    @Serializable
    private data class PairingMoved(
        @SerialName("fleet") val fleet: Long,
        @SerialName("status") val status: PairingStatusView,
    )

    @Serializable
    private data class StepAnswer(
        @SerialName("ticket") val ticket: Long,
        @SerialName("json") val json: String,
    )

    companion object {
        const val STOPPED_ANSWER: String = """{"Err":"the runtime has stopped"}"""
        private const val WATCHER_JOIN_MS: Long = 5_000L
        private const val MAX_CONSECUTIVE_WATCHER_ERRORS: Int = 3
    }
}
