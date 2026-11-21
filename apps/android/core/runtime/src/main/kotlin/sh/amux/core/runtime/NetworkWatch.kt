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
 * Pure filter and debouncer that suppresses the initial default-network
 * registration burst, ignores RSSI / link-bandwidth jitter on an unchanged
 * network + transport set, and coalesces genuine handovers within [debounceMs].
 */
class NetworkChangeDebouncer(
    private val scope: CoroutineScope,
    private val debounceMs: Long = DEFAULT_DEBOUNCE_MS,
) {
    private val lock = Any()
    private var generation = 0
    private var baselineNetworkId: Long? = null
    private var baselineTransports: Int? = null
    private var baselineLinkSignature: String? = null
    private var pendingJob: Job? = null
    private var onChange: (() -> Unit)? = null

    fun start(initiallyOffline: Boolean = false, onChange: () -> Unit) {
        synchronized(lock) {
            stopLocked()
            this.onChange = onChange
            if (initiallyOffline) {
                baselineNetworkId = NO_NETWORK
                baselineTransports = 0
                baselineLinkSignature = ""
            }
        }
    }

    fun stop() {
        synchronized(lock) {
            stopLocked()
        }
    }

    fun onAvailable(networkId: Long) {
        synchronized(lock) {
            if (onChange == null) return
            val existing = baselineNetworkId
            if (existing == null) {
                // First callback after registration seeds the active network.
                baselineNetworkId = networkId
                baselineTransports = null
                baselineLinkSignature = null
                return
            }
            if (existing == networkId) {
                return
            }
            baselineNetworkId = networkId
            baselineTransports = null
            baselineLinkSignature = null
            scheduleLocked()
        }
    }

    fun onCapabilitiesChanged(networkId: Long, transports: Int) {
        synchronized(lock) {
            if (onChange == null) return
            val existing = baselineNetworkId
            if (existing == null) {
                baselineNetworkId = networkId
                baselineTransports = transports
                baselineLinkSignature = null
                return
            }
            if (existing == networkId) {
                val knownTransports = baselineTransports
                if (knownTransports == null) {
                    // Initial capabilities snapshot for the network we just seeded.
                    baselineTransports = transports
                    return
                }
                if (knownTransports == transports) {
                    // Same network and same transport set (e.g. RSSI / bandwidth jitter).
                    return
                }
                baselineTransports = transports
                scheduleLocked()
                return
            }
            baselineNetworkId = networkId
            baselineTransports = transports
            baselineLinkSignature = null
            scheduleLocked()
        }
    }

    fun onLinkPropertiesChanged(networkId: Long, linkSignature: String) {
        synchronized(lock) {
            if (onChange == null) return
            val existing = baselineNetworkId
            if (existing == null) {
                baselineNetworkId = networkId
                baselineLinkSignature = linkSignature
                return
            }
            if (existing == networkId) {
                val known = baselineLinkSignature
                if (known == null) {
                    baselineLinkSignature = linkSignature
                    return
                }
                if (known == linkSignature) {
                    return
                }
                baselineLinkSignature = linkSignature
                scheduleLocked()
                return
            }
            baselineNetworkId = networkId
            baselineTransports = null
            baselineLinkSignature = linkSignature
            scheduleLocked()
        }
    }

    fun onLost(networkId: Long) {
        synchronized(lock) {
            if (onChange == null) return
            if (baselineNetworkId != networkId) return
            baselineNetworkId = NO_NETWORK
            baselineTransports = 0
            baselineLinkSignature = ""
            scheduleLocked()
        }
    }

    private fun scheduleLocked() {
        if (pendingJob?.isActive == true) return
        val mine = generation
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

    private fun stopLocked() {
        generation += 1
        baselineNetworkId = null
        baselineTransports = null
        baselineLinkSignature = null
        onChange = null
        pendingJob?.cancel()
        pendingJob = null
    }

    companion object {
        const val DEFAULT_DEBOUNCE_MS: Long = 250L
        private const val NO_NETWORK: Long = -1L
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
        debouncer.start(initiallyOffline = manager.activeNetwork == null, onChange = onChange)
        val registered = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                debouncer.onAvailable(network.networkHandle)
            }

            override fun onLost(network: Network) {
                debouncer.onLost(network.networkHandle)
            }

            override fun onCapabilitiesChanged(
                network: Network,
                networkCapabilities: NetworkCapabilities,
            ) {
                debouncer.onCapabilitiesChanged(
                    networkId = network.networkHandle,
                    transports = transportMask(networkCapabilities),
                )
            }

            override fun onLinkPropertiesChanged(
                network: Network,
                linkProperties: LinkProperties,
            ) {
                val iface = linkProperties.interfaceName.orEmpty()
                val addrs = linkProperties.linkAddresses
                    .mapNotNull { it.address?.hostAddress?.substringBefore('%') }
                    .sorted()
                    .joinToString(",")
                debouncer.onLinkPropertiesChanged(
                    networkId = network.networkHandle,
                    linkSignature = "$iface|$addrs",
                )
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

    companion object {
        private val TRACKED_TRANSPORTS = intArrayOf(
            NetworkCapabilities.TRANSPORT_CELLULAR,
            NetworkCapabilities.TRANSPORT_WIFI,
            NetworkCapabilities.TRANSPORT_BLUETOOTH,
            NetworkCapabilities.TRANSPORT_ETHERNET,
            NetworkCapabilities.TRANSPORT_VPN,
            NetworkCapabilities.TRANSPORT_WIFI_AWARE,
            NetworkCapabilities.TRANSPORT_LOWPAN,
            NetworkCapabilities.TRANSPORT_USB,
            NetworkCapabilities.TRANSPORT_SATELLITE,
        )

        internal fun transportMask(capabilities: NetworkCapabilities): Int {
            var mask = 0
            for (transport in TRACKED_TRANSPORTS) {
                if (capabilities.hasTransport(transport)) {
                    mask = mask or (1 shl transport)
                }
            }
            return mask
        }
    }
}
