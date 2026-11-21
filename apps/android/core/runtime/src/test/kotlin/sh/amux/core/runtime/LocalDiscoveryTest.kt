package sh.amux.core.runtime

import java.net.InetAddress
import java.nio.charset.StandardCharsets
import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import org.robolectric.Shadows.shadowOf
import org.robolectric.shadows.ShadowNetwork
import android.content.Context
import android.net.ConnectivityManager
import sh.amux.values.Found

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class LocalDiscoveryTest {
    private val hostId = HostId(UUID.fromString("11112222-3333-4444-5555-666677778888"))

    @Test
    fun staleGenerationDispatchRacingWithForgetDoesNotRepopulateHosts() {
        val queued = ArrayDeque<() -> Unit>()
        val delivered = mutableListOf<List<Found>>()
        val discovery = AndroidLocalDiscovery(
            context = RuntimeEnvironment.getApplication(),
            dispatch = { block -> queued.addLast(block) },
            handOver = { snapshot -> delivered.add(snapshot) },
        )

        val attrs = mapOf(
            "hid" to hostId.toString().toByteArray(StandardCharsets.UTF_8),
            "name" to "desk".toByteArray(StandardCharsets.UTF_8),
            "scope" to "".toByteArray(StandardCharsets.UTF_8),
            "pair" to "1".toByteArray(StandardCharsets.UTF_8),
        )
        val ipv4 = InetAddress.getByName("192.168.1.10")

        val gen1 = discovery.tracker.beginSession()
        // Callback from gen1 produces a snapshot queued onto the UI dispatcher.
        discovery.tracker.onServiceUpdated(
            serviceName = "desk",
            attributes = attrs,
            addresses = listOf(ipv4),
            port = 41999,
            generation = gen1,
        )
        // Before the queued gen1 dispatch executes, stop/forget advances the generation and clears hosts.
        val clearedGen = discovery.tracker.forget()
        // Simulate a callback that already held a gen1 snapshot dispatching after forget() bumped generation.
        discovery.dispatchSnapshot(
            snapshot = listOf(
                Found(
                    hostId = hostId.bytes,
                    name = "stale-desk",
                    addrs = listOf("192.168.1.10:41999"),
                    scope = "",
                    pairingOpen = true,
                ),
            ),
            browseGen = gen1,
        )
        // Also ensure a callback tagged with the latest generation still delivers once stop/start syncs it.
        discovery.start()
        discovery.stop()
        // Enqueue one stale callback from clearedGen after stop() moved to a newer generation.
        discovery.dispatchSnapshot(
            snapshot = listOf(
                Found(
                    hostId = hostId.bytes,
                    name = "stale-after-stop",
                    addrs = listOf("192.168.1.10:41999"),
                    scope = "",
                    pairingOpen = true,
                ),
            ),
            browseGen = clearedGen,
        )

        while (queued.isNotEmpty()) {
            queued.removeFirst().invoke()
        }

        // Every delivered snapshot must be empty — stale gen1 and clearedGen callbacks were dropped.
        assertTrue(
            "expected no stale hosts to be delivered after stop/forget, got $delivered",
            delivered.all { it.isEmpty() },
        )
        assertEquals(emptyList<Found>(), delivered.lastOrNull() ?: emptyList<Found>())
    }

    private val connectivity: ConnectivityManager
        get() = RuntimeEnvironment.getApplication()
            .getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager

    /** A discovery on a phone that starts with no network at all. */
    private fun discovery(): AndroidLocalDiscovery {
        shadowOf(connectivity).clearAllNetworks()
        return AndroidLocalDiscovery(context = RuntimeEnvironment.getApplication(), handOver = {})
    }

    private fun localWatch(): ConnectivityManager.NetworkCallback =
        shadowOf(connectivity).networkCallbacks.single()

    @Test
    fun aLossToldBeforeTheFirstReadingStaysLost() {
        val discovery = discovery()
        discovery.start()
        val wifi = ShadowNetwork.newInstance(41)

        // The callback reports the network gone before the reading taken as
        // the watch started lands, and that reading still lists it.
        localWatch().onLost(wifi)
        discovery.takeReading(listOf(wifi.networkHandle))

        assertEquals(LocalNetworkSight.NoLocalNetwork, discovery.sight.value)
        discovery.stop()
    }

    @Test
    fun aNetworkComingBackDoesNotLiftARefusedBrowse() {
        val discovery = discovery()
        discovery.start()
        discovery.browseRefused()

        localWatch().onAvailable(ShadowNetwork.newInstance(42))

        assertEquals(LocalNetworkSight.BrowseFailed, discovery.sight.value)
        discovery.stop()
    }

    @Test
    fun aCallbackDeliveredAfterStopChangesNothing() {
        val discovery = discovery()
        discovery.start()
        val watch = localWatch()
        discovery.takeReading(listOf(ShadowNetwork.newInstance(43).networkHandle))
        val before = discovery.sight.value
        discovery.stop()

        watch.onLost(ShadowNetwork.newInstance(43))

        assertEquals(before, discovery.sight.value)
    }
}
