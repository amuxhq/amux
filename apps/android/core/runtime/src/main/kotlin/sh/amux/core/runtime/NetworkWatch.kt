package sh.amux.core.runtime

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** Observes system network handovers and triggers [Lifecycle.NetworkChanged]. */
interface NetworkWatcher {
    fun start(onChange: () -> Unit)
    fun stop()
}

/** No-op [NetworkWatcher] for JVM tests or environments without connectivity monitoring. */
object NoOpNetworkWatcher : NetworkWatcher {
    override fun start(onChange: () -> Unit) = Unit
    override fun stop() = Unit
}

/**
 * Tells of every change of the default network after the one the watch
 * started on: Wi-Fi to cellular, another Wi-Fi network, the network going or
 * coming back. A handover arrives as a burst of callbacks, so they are
 * coalesced: the first change of a burst is told [debounceMs] later, once.
 *
 * Callbacks arrive on the system's thread; [onChange] runs on [scope].
 */
class NetworkChangeDebouncer(
    private val scope: CoroutineScope,
    private val debounceMs: Long = DEFAULT_DEBOUNCE_MS,
) {
    private val lock = Any()
    private var generation = 0
    private var current: Long? = null
    private var pending: Job? = null
    private var onChange: (() -> Unit)? = null

    /** Starts watching from [network], the default network now, or `null` with none. */
    fun start(network: Long?, onChange: () -> Unit) {
        synchronized(lock) {
            stopLocked()
            current = network
            this.onChange = onChange
        }
    }

    fun stop() {
        synchronized(lock) { stopLocked() }
    }

    /** The default network is now [network]; registering repeats the one already current. */
    fun available(network: Long) {
        synchronized(lock) {
            if (onChange == null || current == network) return
            current = network
            scheduleLocked()
        }
    }

    /** [network] stopped being the default network. */
    fun lost(network: Long) {
        synchronized(lock) {
            if (onChange == null || current != network) return
            current = null
            scheduleLocked()
        }
    }

    private fun scheduleLocked() {
        if (pending?.isActive == true) return
        val mine = generation
        pending = scope.launch {
            delay(debounceMs)
            val callback = synchronized(lock) {
                if (mine != generation) return@launch
                pending = null
                onChange
            }
            callback?.invoke()
        }
    }

    private fun stopLocked() {
        generation += 1
        current = null
        onChange = null
        pending?.cancel()
        pending = null
    }

    companion object {
        const val DEFAULT_DEBOUNCE_MS: Long = 250L
    }
}

/** [NetworkWatcher] over the system's default-network callback. */
class AndroidNetworkWatch(
    context: Context,
    scope: CoroutineScope,
    debounceMs: Long = NetworkChangeDebouncer.DEFAULT_DEBOUNCE_MS,
) : NetworkWatcher {
    private val connectivityManager: ConnectivityManager? =
        context.applicationContext.getSystemService(Context.CONNECTIVITY_SERVICE)
            as? ConnectivityManager
    private val debouncer = NetworkChangeDebouncer(scope = scope, debounceMs = debounceMs)
    private var callback: ConnectivityManager.NetworkCallback? = null

    override fun start(onChange: () -> Unit) {
        val manager = connectivityManager ?: return
        stop()
        debouncer.start(network = manager.activeNetwork?.networkHandle, onChange = onChange)
        val registered = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) = debouncer.available(network.networkHandle)

            override fun onLost(network: Network) = debouncer.lost(network.networkHandle)
        }
        try {
            manager.registerDefaultNetworkCallback(registered)
            callback = registered
        } catch (_: RuntimeException) {
            // The system refuses a callback past its per-app limit; the
            // runtime then learns of a handover when its links fail instead.
            debouncer.stop()
        }
    }

    override fun stop() {
        debouncer.stop()
        val current = callback ?: return
        callback = null
        try {
            connectivityManager?.unregisterNetworkCallback(current)
        } catch (_: IllegalArgumentException) {
            // Already unregistered.
        }
    }
}
