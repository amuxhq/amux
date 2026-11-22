package sh.amux.core.runtime

import java.util.UUID
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.HostView
import sh.amux.values.Identity
import sh.amux.values.Offline
import sh.amux.values.PairedPeer
import sh.amux.values.Reach
import sh.amux.values.Roster
import sh.amux.values.RouteKind

@OptIn(ExperimentalCoroutinesApi::class)
class HostsStoreTest {
    private val host1 = HostId(UUID.fromString("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"))
    private val host2 = HostId(UUID.fromString("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"))
    private val cand1 = HostId(UUID.fromString("cccccccc-cccc-cccc-cccc-cccccccccccc"))
    private val cand2 = HostId(UUID.fromString("dddddddd-dddd-dddd-dddd-dddddddddddd"))
    private val phone = HostId(UUID.fromString("eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee"))

    private fun hostView(
        id: HostId,
        name: String,
        local: Boolean = false,
        trusted: Boolean = true,
        candidate: Boolean = false,
        reach: Reach = Reach.Online(RouteKind.Direct),
        addrs: List<String> = emptyList(),
        pairingOpen: Boolean = false,
        lastDialError: String? = null,
        link: Long = 0L,
    ): HostView = HostView(
        hostId = id.bytes,
        link = link,
        sameNetwork = false,
        name = name,
        local = local,
        trusted = trusted,
        candidate = candidate,
        reach = reach,
        addrs = addrs,
        relayOnly = false,
        pairingOpen = pairingOpen,
        current = true,
        providers = emptyList(),
        lastDialError = lastDialError,
    )

    @Test
    fun groupsPairedAndDiscoveredHostsAndTracksDepartures() = runTest {
        var now = 10_000L
        val store = HostsStore(scope = this, clock = { now })

        val views = listOf(
            hostView(phone, "Pixel", local = true, trusted = true),
            hostView(host2, "zebra", reach = Reach.Offline(Offline.Plain)),
            hostView(host1, "alpha", reach = Reach.Online(RouteKind.Relay)),
            hostView(
                cand1,
                "nearby-mac",
                trusted = false,
                candidate = true,
                reach = Reach.Offline(Offline.Plain),
                addrs = listOf("192.168.1.5:4100"),
                pairingOpen = true,
            ),
            hostView(
                cand2,
                "relay-box",
                trusted = false,
                candidate = true,
                reach = Reach.Offline(Offline.Plain),
                addrs = emptyList(),
                pairingOpen = false,
            ),
        )
        store.show(views)

        val s = store.snapshot
        assertEquals("Pixel", s.local?.name)
        // Trusted hosts sorted online-first then alphabetically.
        assertEquals(listOf("alpha", "zebra"), s.hosts.map { it.name })
        assertEquals(listOf("alpha"), s.hosts(HostGroup.ThroughTheRelay).map { it.name })
        assertEquals(listOf("zebra"), s.hosts(HostGroup.Offline).map { it.name })
        assertEquals(listOf("nearby-mac"), s.candidates(HostGroup.OnThisNetwork).map { it.name })
        assertEquals(listOf("relay-box"), s.candidates(HostGroup.ThroughTheRelay).map { it.name })

        // Host2 is offline without a dial error right after foreground -> connecting.
        assertTrue(s.isConnecting(host2))
        assertFalse(s.isConnecting(host1))

        // Host1 departs -> departure time recorded.
        now = 25_000L
        store.show(
            listOf(
                hostView(phone, "Pixel", local = true),
                hostView(host1, "alpha", reach = Reach.Offline(Offline.Plain), lastDialError = "refused"),
            ),
        )
        assertEquals(25_000L, store.wentOfflineAtMs(host1))
        assertFalse(store.snapshot.isConnecting(host1))

        // Patience elapses -> connecting clears automatically.
        advanceTimeBy(HostsStore.CONNECTING_PATIENCE_MS + 1L)
        assertTrue(store.snapshot.connecting.isEmpty())
    }

    @Test
    fun windowToOfferRespectsDeclineAndResetsWhenWindowCloses() = runTest {
        val store = HostsStore(scope = this)
        val openCandidate = hostView(
            cand1,
            "laptop",
            trusted = false,
            candidate = true,
            reach = Reach.Offline(Offline.Plain),
            addrs = listOf("10.0.0.4:5000"),
            pairingOpen = true,
        )
        val closedCandidate = openCandidate.copy(pairingOpen = false)

        store.show(listOf(openCandidate))
        assertEquals(cand1, store.snapshot.windowToOffer?.id)

        store.declineWindow(cand1)
        assertNull(store.snapshot.windowToOffer)

        // Window closes -> decline forgotten.
        store.show(listOf(closedCandidate))
        assertNull(store.snapshot.windowToOffer)

        // Window reopens -> offered again.
        store.show(listOf(openCandidate))
        assertEquals(cand1, store.snapshot.windowToOffer?.id)
        store.stopWaiting()
    }

    @Test
    fun rosterBehindDetectsMissingTrustedPeersAndFoundButUnreachable() = runTest {
        val store = HostsStore(scope = this)
        store.show(
            listOf(
                hostView(
                    host1,
                    "desk",
                    reach = Reach.Offline(Offline.Plain),
                    addrs = listOf("10.0.0.9:5000"),
                ),
            ),
        )
        assertTrue(store.snapshot.rosterBehind)
        assertEquals(setOf(host1), store.snapshot.foundButUnreachable)

        store.showRoster(
            Roster(
                identity = Identity(hostId = phone.bytes, name = "Pixel", fingerprint = "abcd"),
                peers = listOf(PairedPeer(hostId = host1.bytes, name = "desk", fingerprint = "1234")),
            ),
        )
        assertFalse(store.snapshot.rosterBehind)
        store.stopWaiting()
    }

    /**
     * Coming forward, a machine the last listing read out of reach with no
     * failed dial is connecting at once: the next listing may not come until
     * its dial ends, as for a machine stopped while the app was away.
     */
    @Test
    fun comingForwardWaitsOnTheMachinesListedLast() {
        val hosts = HostsStore()
        val listed = listOf(
            hostView(host1, "studio", reach = Reach.Offline(Offline.Plain)),
            hostView(host2, "mini", reach = Reach.Offline(Offline.Plain), lastDialError = "connection refused"),
            hostView(cand1, "nova", reach = Reach.Online(RouteKind.Direct)),
        )
        hosts.show(listed)
        hosts.stopWaiting()
        assertEquals(emptySet<HostId>(), hosts.snapshot.connecting)

        hosts.cameForward()
        assertEquals("a failed dial is known already", setOf(host1), hosts.snapshot.connecting)
        hosts.show(listed)
        assertEquals("the same listing again changes nothing", setOf(host1), hosts.snapshot.connecting)
    }

    /**
     * A machine read linked as the app comes forward lost that link while the
     * app was away, though a listing may say it is linked until the runtime
     * hears it go: whichever comes first, the machine is connecting after
     * both, and a link read after it went settles it.
     */
    @Test
    fun aLinkFromBeforeComingForwardSettlesNothing() {
        val linked = listOf(hostView(host1, "studio", reach = Reach.Online(RouteKind.Direct)))
        val unlinked = listOf(hostView(host1, "studio", reach = Reach.Offline(Offline.Plain)))

        val first = HostsStore()
        first.show(linked)
        first.show(unlinked)
        first.cameForward()
        assertEquals("the link went first", setOf(host1), first.snapshot.connecting)

        val later = HostsStore()
        later.show(linked)
        later.cameForward()
        later.show(linked)
        assertEquals("drawn linked while it is read so", emptySet<HostId>(), later.snapshot.connecting)
        later.show(unlinked)
        assertEquals("the app came forward first", setOf(host1), later.snapshot.connecting)

        later.show(linked)
        assertEquals("linked again since", emptySet<HostId>(), later.snapshot.connecting)
        later.show(unlinked)
        assertEquals("a machine that answered and went is gone", emptySet<HostId>(), later.snapshot.connecting)
    }

    /**
     * The listing that says a link from before coming forward went can be
     * skipped, the runtime listing the machine linked again by the next one
     * the phone reads: a newer link settles it all the same, and when that
     * link goes the machine is gone, not connecting.
     */
    @Test
    fun aNewerLinkSettlesAMachineWhoseUnlinkedReadWasSkipped() {
        val hosts = HostsStore()
        hosts.show(listOf(hostView(host1, "studio", reach = Reach.Online(RouteKind.Direct), link = 1L)))
        hosts.cameForward()
        hosts.show(listOf(hostView(host1, "studio", reach = Reach.Online(RouteKind.Direct), link = 1L)))
        assertEquals("drawn linked while it is read so", emptySet<HostId>(), hosts.snapshot.connecting)

        hosts.show(listOf(hostView(host1, "studio", reach = Reach.Online(RouteKind.Direct), link = 2L)))
        assertEquals(emptySet<HostId>(), hosts.snapshot.connecting)
        hosts.show(listOf(hostView(host1, "studio", reach = Reach.Offline(Offline.Plain), link = 2L)))
        assertEquals("a machine that answered since and went is gone", emptySet<HostId>(), hosts.snapshot.connecting)
    }
}
