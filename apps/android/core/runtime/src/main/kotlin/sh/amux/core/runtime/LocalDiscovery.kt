package sh.amux.core.runtime

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.net.wifi.WifiManager
import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress
import java.nio.charset.StandardCharsets
import java.util.concurrent.Executor
import java.util.concurrent.Executors
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import sh.amux.values.Found

/** Local-network discovery seam for [RuntimeCoordinator]. */
interface DiscoverySource {
    val permission: StateFlow<LocalNetworkPermission>
    fun start()
    fun stop()
}

/** Pure TXT-record claim parsed from an `_amux._udp.` advertisement. */
data class DiscoveryClaim(
    val host: HostId,
    val scope: String,
    val name: String?,
    val pairingOpen: Boolean,
)

/**
 * Pure state tracker that turns resolved NSD sightings into the deduplicated
 * `List<Found>` handed to [Runtime.discovered]. Separated from [NsdManager] so
 * TXT parsing, address formatting, filtering, and removal are unit-tested on JVM.
 */
class NsdDiscoveryTracker(
    private val only: Set<HostId>? = null,
    private val handOver: (List<Found>) -> Unit,
) {
    private val foundByService = LinkedHashMap<String, Found>()

    @Synchronized
    fun onServiceUpdated(
        serviceName: String,
        attributes: Map<String, ByteArray?>,
        addresses: List<InetAddress>,
        port: Int,
    ) {
        val claim = parseClaim(attributes)
        if (claim == null || (only != null && claim.host !in only)) {
            if (foundByService.remove(serviceName) != null) {
                emit()
            }
            return
        }
        val formattedAddrs = addresses
            .mapNotNull { formatAddress(it, port) }
            .distinct()
        if (formattedAddrs.isEmpty()) {
            if (foundByService.remove(serviceName) != null) {
                emit()
            }
            return
        }
        val displayName = claim.name ?: serviceName
        val next = Found(
            hostId = claim.host.bytes,
            name = displayName,
            addrs = formattedAddrs,
            scope = claim.scope,
            pairingOpen = claim.pairingOpen,
        )
        if (foundByService[serviceName] != next) {
            foundByService[serviceName] = next
            emit()
        }
    }

    @Synchronized
    fun onServiceLost(serviceName: String) {
        if (foundByService.remove(serviceName) != null) {
            emit()
        }
    }

    /** Clears tracked entries without withdrawing them from the runtime (used on background). */
    @Synchronized
    fun forget() {
        foundByService.clear()
    }

    @Synchronized
    fun current(): List<Found> = foundByService.values.toList()

    private fun emit() {
        handOver(foundByService.values.toList())
    }

    companion object {
        const val SERVICE_TYPE: String = "_amux._udp."

        fun parseClaim(attributes: Map<String, ByteArray?>): DiscoveryClaim? {
            val hidText = txtString(attributes, "hid") ?: return null
            val host = HostId.parse(hidText) ?: return null
            val name = txtString(attributes, "name")?.takeIf { it.isNotEmpty() }
            val scope = txtString(attributes, "scope").orEmpty()
            val pairingOpen = txtString(attributes, "pair") == "1"
            return DiscoveryClaim(
                host = host,
                scope = scope,
                name = name,
                pairingOpen = pairingOpen,
            )
        }

        fun formatAddress(address: InetAddress, port: Int): String? {
            if (port !in 1..65535) return null
            return when (address) {
                is Inet4Address -> {
                    val host = address.hostAddress?.substringBefore('%') ?: return null
                    "$host:$port"
                }
                is Inet6Address -> {
                    if (address.isLinkLocalAddress) return null
                    val host = address.hostAddress?.substringBefore('%') ?: return null
                    "[$host]:$port"
                }
                else -> null
            }
        }

        private fun txtString(attributes: Map<String, ByteArray?>, key: String): String? {
            val raw = attributes[key] ?: return null
            return String(raw, StandardCharsets.UTF_8)
        }
    }
}

/**
 * Android local-network discovery backed by API 34+ [NsdManager.ServiceInfoCallback]
 * and [WifiManager.MulticastLock].
 */
class AndroidLocalDiscovery(
    context: Context,
    only: Set<HostId>? = null,
    private val dispatch: (() -> Unit) -> Unit = { it() },
    private val handOver: (List<Found>) -> Unit,
) : DiscoverySource {
    private val appContext = context.applicationContext
    private val nsdManager: NsdManager? =
        appContext.getSystemService(Context.NSD_SERVICE) as? NsdManager
    private val wifiManager: WifiManager? =
        appContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager

    private val _permission = MutableStateFlow(LocalNetworkPermission.Granted)
    override val permission: StateFlow<LocalNetworkPermission> = _permission.asStateFlow()

    private val lock = Any()
    private val executor: Executor = Executors.newSingleThreadExecutor { r ->
        Thread(r, "amux-nsd").apply { isDaemon = true }
    }
    private val tracker = NsdDiscoveryTracker(only = only) { list ->
        dispatch { handOver(list) }
    }

    private var browsing = false
    private var browseGeneration = 0
    private var multicastLock: WifiManager.MulticastLock? = null
    private var discoveryListener: NsdManager.DiscoveryListener? = null
    private val infoCallbacks = mutableMapOf<String, NsdManager.ServiceInfoCallback>()

    override fun start() {
        val manager = nsdManager ?: return
        synchronized(lock) {
            if (browsing) return
            browsing = true
            browseGeneration += 1
            val generation = browseGeneration

            acquireMulticastLock()

            val listener = object : NsdManager.DiscoveryListener {
                override fun onDiscoveryStarted(regType: String) {
                    _permission.value = LocalNetworkPermission.Granted
                }

                override fun onServiceFound(serviceInfo: NsdServiceInfo) {
                    registerInfoCallback(serviceInfo, generation)
                }

                override fun onServiceLost(serviceInfo: NsdServiceInfo) {
                    unregisterInfoCallback(serviceInfo.serviceName, generation)
                }

                override fun onDiscoveryStopped(serviceType: String) = Unit

                override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                    stop()
                }

                override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) = Unit
            }
            discoveryListener = listener
            runCatching {
                manager.discoverServices(
                    NsdDiscoveryTracker.SERVICE_TYPE,
                    NsdManager.PROTOCOL_DNS_SD,
                    listener,
                )
            }.onFailure {
                browsing = false
                discoveryListener = null
                releaseMulticastLock()
            }
        }
    }

    override fun stop() {
        val manager = nsdManager
        val listenerToStop: NsdManager.DiscoveryListener?
        val callbacksToStop: List<NsdManager.ServiceInfoCallback>
        synchronized(lock) {
            if (!browsing) return
            browsing = false
            browseGeneration += 1
            listenerToStop = discoveryListener
            discoveryListener = null
            callbacksToStop = infoCallbacks.values.toList()
            infoCallbacks.clear()
            tracker.forget()
            releaseMulticastLock()
        }
        if (manager != null) {
            for (callback in callbacksToStop) {
                runCatching { manager.unregisterServiceInfoCallback(callback) }
            }
            if (listenerToStop != null) {
                runCatching { manager.stopServiceDiscovery(listenerToStop) }
            }
        }
    }

    private fun registerInfoCallback(serviceInfo: NsdServiceInfo, generation: Int) {
        val manager = nsdManager ?: return
        val name = serviceInfo.serviceName
        val previous: NsdManager.ServiceInfoCallback?
        val callback = object : NsdManager.ServiceInfoCallback {
            override fun onServiceInfoCallbackRegistrationFailed(errorCode: Int) {
                synchronized(lock) {
                    if (generation == browseGeneration) {
                        infoCallbacks.remove(name)
                    }
                }
            }

            override fun onServiceUpdated(updated: NsdServiceInfo) {
                synchronized(lock) {
                    if (generation != browseGeneration || !browsing) return
                }
                tracker.onServiceUpdated(
                    serviceName = updated.serviceName,
                    attributes = updated.attributes,
                    addresses = updated.hostAddresses,
                    port = updated.port,
                )
            }

            override fun onServiceLost() {
                synchronized(lock) {
                    if (generation != browseGeneration || !browsing) return
                }
                tracker.onServiceLost(name)
            }

            override fun onServiceInfoCallbackUnregistered() = Unit
        }

        synchronized(lock) {
            if (generation != browseGeneration || !browsing) return
            previous = infoCallbacks.put(name, callback)
        }
        if (previous != null) {
            runCatching { manager.unregisterServiceInfoCallback(previous) }
        }
        runCatching {
            manager.registerServiceInfoCallback(serviceInfo, executor, callback)
        }.onFailure {
            synchronized(lock) {
                if (infoCallbacks[name] === callback) {
                    infoCallbacks.remove(name)
                }
            }
        }
    }

    private fun unregisterInfoCallback(serviceName: String, generation: Int) {
        val manager = nsdManager
        val removed = synchronized(lock) {
            if (generation != browseGeneration) return
            infoCallbacks.remove(serviceName)
        }
        if (manager != null && removed != null) {
            runCatching { manager.unregisterServiceInfoCallback(removed) }
        }
        tracker.onServiceLost(serviceName)
    }

    private fun acquireMulticastLock() {
        val manager = wifiManager ?: return
        runCatching {
            val lock = multicastLock ?: manager.createMulticastLock("amux.discovery").apply {
                setReferenceCounted(false)
                multicastLock = this
            }
            if (!lock.isHeld) {
                lock.acquire()
            }
        }
    }

    private fun releaseMulticastLock() {
        runCatching {
            val lock = multicastLock
            if (lock != null && lock.isHeld) {
                lock.release()
            }
        }
    }
}
