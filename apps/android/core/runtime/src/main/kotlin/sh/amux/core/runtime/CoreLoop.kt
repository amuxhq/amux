package sh.amux.core.runtime

import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.atomic.AtomicBoolean
import java.util.logging.Level
import java.util.logging.Logger
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull
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

/** Internal bridge seam allowing unit tests to exercise watcher failures deterministically. */
internal interface CoreLoopBridge {
    fun runtimeStart(configJson: String): Long
    fun runtimeStop(handle: Long)
    fun runtimeWaitWake(handle: Long, timeoutMs: Int): Int
    fun runtimeInterruptWake(handle: Long)
    fun runtimeStep(handle: Long): String?
}

private object NativeCoreLoopBridge : CoreLoopBridge {
    override fun runtimeStart(configJson: String): Long = NativeBridge.runtimeStart(configJson)
    override fun runtimeStop(handle: Long) = NativeBridge.runtimeStop(handle)
    override fun runtimeWaitWake(handle: Long, timeoutMs: Int): Int =
        NativeBridge.runtimeWaitWake(handle, timeoutMs)
    override fun runtimeInterruptWake(handle: Long) = NativeBridge.runtimeInterruptWake(handle)
    override fun runtimeStep(handle: Long): String? = NativeBridge.runtimeStep(handle)
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
class CoreLoop internal constructor(
    private val dispatch: (() -> Unit) -> Unit = { it() },
    private val onProfilesMoved: (List<ProfileView>) -> Unit = {},
    private val onError: (Throwable) -> Unit = ::logWatcherError,
    private val onStopped: (String) -> Unit = {},
    private val bridge: CoreLoopBridge,
) : AutoCloseable {
    /**
     * [onStopped] hears, through [dispatch], that the runtime stopped by
     * itself (the bridge failed or the runtime went away), never a [stop]
     * asked for here.
     */
    constructor(
        dispatch: (() -> Unit) -> Unit = { it() },
        onProfilesMoved: (List<ProfileView>) -> Unit = {},
        onError: (Throwable) -> Unit = ::logWatcherError,
        onStopped: (String) -> Unit = {},
    ) : this(
        dispatch = dispatch,
        onProfilesMoved = onProfilesMoved,
        onError = onError,
        onStopped = onStopped,
        bridge = NativeCoreLoopBridge,
    )

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
    private val released = AtomicBoolean(false)
    private val fleetListeners = ConcurrentHashMap<Long, (FleetEvent) -> Unit>()
    private val waitingTickets = ConcurrentHashMap<Long, CompletableFuture<String>>()

    @Volatile
    private var runtimeHandle: Long = 0L

    @Volatile
    private var watcherThread: Thread? = null

    val handle: Long
        get() = runtimeHandle

    internal val isExecutorShutdown: Boolean
        get() = executor.isShutdown

    internal fun awaitWatcherExit(timeoutMs: Long = WATCHER_JOIN_MS) {
        watcherThread?.join(timeoutMs)
    }

    /**
     * Starts the embedded runtime on the core thread, launches the wake-fd
     * watcher, and performs the initial step so the initial profile list is
     * delivered immediately.
     */
    fun start(config: StartConfig): Long {
        val configJson = AmuxJson.json.encodeToString(config)
        return blocking {
            check(runtimeHandle == 0L) { "CoreLoop has already been started" }
            val started = bridge.runtimeStart(configJson)
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
     * Runs [block] on the single core thread without blocking the caller's thread.
     *
     * A caller cancelled while [block] runs never sees its value, so a value
     * that owns a native handle passes [dispose] to close it then.
     */
    suspend fun <T> runOnCore(dispose: ((T) -> Unit)? = null, block: () -> T): T {
        if (coreThreadRef.get() == true) {
            return block()
        }
        return suspendCancellableCoroutine { cont ->
            try {
                executor.execute {
                    val value = try {
                        block()
                    } catch (t: Throwable) {
                        if (cont.isActive) cont.resumeWithException(t)
                        return@execute
                    }
                    if (dispose == null) {
                        if (cont.isActive) cont.resume(value)
                    } else {
                        // Called when the caller was cancelled before or while
                        // this resumes it, so nothing else will close the value.
                        cont.resume(value) { _, dropped, _ -> dispose(dropped) }
                    }
                }
            } catch (e: RejectedExecutionException) {
                if (cont.isActive) cont.resumeWithException(RuntimeFailure(Bridge.STOPPED_REASON))
            }
        }
    }

    /**
     * Starts an asynchronous bridge act on the core thread and suspends until
     * its JSON answer is delivered after the step's deltas.
     */
    suspend fun awaitAct(start: (Long) -> Long): String {
        val future = submitAct(start)
        return suspendCancellableCoroutine { cont ->
            future.whenComplete { value, error ->
                if (!cont.isActive) return@whenComplete
                if (error != null) {
                    val cause = (error as? java.util.concurrent.CompletionException)?.cause ?: error
                    cont.resumeWithException(cause)
                } else {
                    cont.resume(value ?: STOPPED_ANSWER)
                }
            }
        }
    }

    fun isRunning(): Boolean = runtimeHandle != 0L && !stopping.get()

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
        val firstStopper = stopping.compareAndSet(false, true)
        val live = runtimeHandle
        if (firstStopper && live != 0L) {
            runCatching { bridge.runtimeInterruptWake(live) }
        }
        val watcher = watcherThread
        val onCoreThread = coreThreadRef.get() == true
        if (!onCoreThread && watcher != null && watcher !== Thread.currentThread()) {
            try {
                watcher.join(WATCHER_JOIN_MS)
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
            }
        }
        watcherThread = null
        releaseResources()
    }

    override fun close() {
        stop()
    }

    private fun releaseResources() {
        if (!released.compareAndSet(false, true)) {
            return
        }
        try {
            blocking {
                val handleToStop = runtimeHandle
                runtimeHandle = 0L
                if (handleToStop != 0L) {
                    bridge.runtimeStop(handleToStop)
                }
                failPendingTickets()
            }
        } catch (t: Throwable) {
            if (t !is RejectedExecutionException) {
                reportError(t)
            }
            val handleToStop = runtimeHandle
            runtimeHandle = 0L
            if (handleToStop != 0L) {
                runCatching { bridge.runtimeStop(handleToStop) }
            }
        } finally {
            fleetListeners.clear()
            failPendingTickets()
            executor.shutdown()
        }
    }

    private fun startWatcher(handle: Long) {
        val thread = Thread(
            {
                var cause: Throwable? = null
                try {
                    while (!stopping.get()) {
                        val ready = bridge.runtimeWaitWake(handle, -1)
                        // A stop rings the wake descriptor, so a wait that
                        // returns while stopping is the stop, not work.
                        if (ready < 0 || stopping.get()) {
                            break
                        }
                        if (ready > 0) {
                            // A step that cannot be read is skipped inside
                            // stepOnCore; what reaches here is the bridge or
                            // the runtime failing, after which no step can run.
                            try {
                                executor.submit { stepOnCore() }.get()
                            } catch (e: ExecutionException) {
                                throw e.cause ?: e
                            }
                        }
                    }
                } catch (t: Throwable) {
                    if (t is InterruptedException) {
                        Thread.currentThread().interrupt()
                    }
                    if (!stopping.get()) {
                        cause = t
                        reportError(t)
                    }
                } finally {
                    if (stopping.compareAndSet(false, true)) {
                        releaseResources()
                        val reason = cause?.failureDescription ?: Bridge.STOPPED_REASON
                        dispatch { onStopped(reason) }
                    }
                    watcherThread = null
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

    /**
     * Steps the runtime once and hands what moved to [dispatch].
     *
     * The answers are read apart from the deltas: an answer is only a ticket
     * and a string, so it reads whatever this build makes of the rest, and
     * every act waiting on the step resumes. Deltas this build cannot read are
     * skipped, as the iPhone app skips them; the next opening or delta brings
     * the stores current again.
     */
    private fun stepOnCore() {
        val live = runtimeHandle
        if (live == 0L) {
            return
        }
        val raw = bridge.runtimeStep(live) ?: return
        val whole = try {
            AmuxJson.json.parseToJsonElement(raw).jsonObject
        } catch (e: IllegalArgumentException) {
            reportError(UnreadableStep(e))
            // Any waiting act may have been answered in this step, and no
            // later step answers it again, so each one is told now.
            failTickets(waitingTickets.keys.toList())
            return
        }
        val answers = readAnswers(whole["answers"])
        val stepped = try {
            AmuxJson.json.decodeFromJsonElement<Stepped>(whole)
        } catch (e: IllegalArgumentException) {
            reportError(UnreadableStep(e))
            null
        }

        val hasWork = answers.isNotEmpty() || (
            stepped != null && (
                stepped.profiles != null ||
                    stepped.fleets.isNotEmpty() ||
                    stepped.chats.isNotEmpty() ||
                    stepped.signInOffers.isNotEmpty() ||
                    stepped.onboarding.isNotEmpty() ||
                    stepped.pairing.isNotEmpty()
                )
            )

        if (hasWork) {
            dispatch {
                try {
                    if (stepped != null) {
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
                    }
                } catch (t: Throwable) {
                    for (answer in answers) {
                        waitingTickets.remove(answer.ticket)?.completeExceptionally(t)
                    }
                    throw t
                }
                for (answer in answers) {
                    waitingTickets.remove(answer.ticket)?.complete(answer.json)
                }
            }
        }
    }

    /**
     * Reads each answer on its own, so one this build cannot read loses only
     * itself; its act, when the ticket can still be read, is told so.
     */
    private fun readAnswers(element: JsonElement?): List<StepAnswer> {
        val listed = element as? JsonArray ?: return emptyList()
        return listed.mapNotNull { item ->
            try {
                AmuxJson.json.decodeFromJsonElement<StepAnswer>(item)
            } catch (e: IllegalArgumentException) {
                reportError(UnreadableStep(e))
                val ticket = ((item as? JsonObject)?.get("ticket") as? JsonPrimitive)?.longOrNull
                ticket?.let { StepAnswer(ticket = it, json = UNREADABLE_ANSWER) }
            }
        }
    }

    /** Answers [tickets] as unreadable, in order with the deltas around them. */
    private fun failTickets(tickets: List<Long>) {
        if (tickets.isEmpty()) return
        dispatch {
            for (ticket in tickets) {
                waitingTickets.remove(ticket)?.complete(UNREADABLE_ANSWER)
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

    /** A step the runtime handed back that this build could not read. */
    class UnreadableStep(cause: Throwable) : Exception("a runtime step could not be read: ${cause.message}", cause)

    companion object {
        const val STOPPED_ANSWER: String = """{"Err":"the runtime has stopped"}"""
        internal const val UNREADABLE_ANSWER: String =
            """{"Err":"the runtime answered in a shape this app cannot read"}"""
        private const val WATCHER_JOIN_MS: Long = 5_000L
    }
}

internal val runtimeLog: Logger = Logger.getLogger("sh.amux.core.runtime")

/** Where the bridge's own failures go: logcat on Android, standard error on the JVM. */
internal fun logWatcherError(error: Throwable) {
    runtimeLog.log(Level.WARNING, "core loop: ${error.message ?: error}", error)
}
