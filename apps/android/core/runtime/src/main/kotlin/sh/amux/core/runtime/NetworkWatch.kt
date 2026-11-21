package sh.amux.core.runtime

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
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
 * Pure debouncer that suppresses the initial network-registration callback burst
 * and coalesces handover bursts within [debounceMs] into a single notification.
 */
class NetworkChangeDebouncer(
    private val scope: CoroutineScope,
    private val debounceMs: Long = DEFAULT_DEBOUNCE_MS,
) {
    private val lock = Any()
    private var generation = 0
    private var seeded = false
    private var pendingJob: Job? = null
    private var onChange: (() -> Unit)? = null

    fun start(onChange: () -> Unit) {
        synchronized(lock) {
            stopLocked()
            this.onChange = onChange
        }
    }

    fun stop() {
        synchronized(lock) {
            stopLocked()
        }
    }

    fun onEvent() {
        val mine: Int
        synchronized(lock) {
            if (onChange == null) return
            if (!seeded) {
                seeded = true
                return
            }
            if (pendingJob?.isActive == true) return
            mine = generation
            pendingJob = scope.launch {
                delay(debounceMs)
                val callback = synchronized(lock) {
                    if (mine != generation) {
                        null
                    } else {
                        pendingJob = null
                        onChange
                    }
                }
                callback?.invoke()
            }
        }
    }

    private fun stopLocked() {
        generation += 1
        seeded = false
        onChange = null
        pendingJob?.cancel()
        pendingJob = null
    }

    companion object {
        const val DEFAULT_DEBOUNCE_MS: Long = 250L
    }
}

/** Android [ConnectivityManager.NetworkCallback] implementation of [NetworkWatcher]. */
class AndroidNetworkWatch(
    context: Context,
    scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default),
    debounceMs: Long = NetworkChangeDebouncer.DEFAULT_DEBOUNCE_MS,
) : NetworkWatcher {
    private val connectivityManager: ConnectivityManager? =
        context.applicationContext.getSystemService(Context.CONNECTIVITY_SERVICE)
            as? ConnectivityManager
    private val debouncer = NetworkChangeDebouncer(scope = scope, debounceMs = debounceMs)
    private val lock = Any()
    private var callback: ConnectivityManager.NetworkCallback? = null

    override fun start(onChange: () -> Unit) {
        val manager = connectivityManager ?: return
        stop()
        debouncer.start(onChange)
        val registered = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                debouncer.onEvent()
            }

            override fun onLost(network: Network) {
                debouncer.onEvent()
            }

            override fun onCapabilitiesChanged(
                network: Network,
                networkCapabilities: NetworkCapabilities,
            ) {
                debouncer.onEvent()
            }

            override fun onLinkPropertiesChanged(
                network: Network,
                linkProperties: LinkProperties,
            ) {
                debouncer.onEvent()
            }
        }
        synchronized(lock) {
            callback = registered
        }
        runCatching {
            manager.registerDefaultNetworkCallback(registered)
        }.onFailure {
            synchronized(lock) {
                if (callback === registered) callback = null
            }
            debouncer.stop()
        }
    }

    override fun stop() {
        val manager = connectivityManager
        val current = synchronized(lock) {
            val existing = callback
            callback = null
            existing
        }
        debouncer.stop()
        if (manager != null && current != null) {
            runCatching { manager.unregisterNetworkCallback(current) }
        }
    }
}
