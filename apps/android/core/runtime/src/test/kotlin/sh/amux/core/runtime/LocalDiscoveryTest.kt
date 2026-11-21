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
}
