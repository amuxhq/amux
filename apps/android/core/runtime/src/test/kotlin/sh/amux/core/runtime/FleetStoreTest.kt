package sh.amux.core.runtime

import java.util.UUID
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.AgentAct
import sh.amux.values.AgentKey
import sh.amux.values.Attention
import sh.amux.values.ChangeTotals
import sh.amux.values.DeleteWarning
import sh.amux.values.Filing
import sh.amux.values.FleetCard
import sh.amux.values.FleetFilter
import sh.amux.values.FleetOnly
import sh.amux.values.FleetRow
import sh.amux.values.FleetSection
import sh.amux.values.FleetView
import sh.amux.values.HostView
import sh.amux.values.Kind
import sh.amux.values.Offline
import sh.amux.values.Reach
import sh.amux.values.RelayLink
import sh.amux.values.RouteKind
import sh.amux.values.SecondLine
import sh.amux.values.SectionKind
import sh.amux.values.WorkspaceGoes

class FleetStoreTest {
    private val hostA = HostId(UUID.fromString("11111111-1111-1111-1111-111111111111"))
    private val hostB = HostId(UUID.fromString("22222222-2222-2222-2222-222222222222"))
    private val localHost = HostId(UUID.fromString("33333333-3333-3333-3333-333333333333"))

    private fun makeHost(
        id: HostId,
        name: String,
        local: Boolean = false,
        trusted: Boolean = true,
        reach: Reach = Reach.Online(RouteKind.Direct),
    ): HostView = HostView(
        hostId = id.bytes,
        link = 0L,
        sameNetwork = false,
        name = name,
        local = local,
        trusted = trusted,
        candidate = !trusted,
        reach = reach,
        addrs = emptyList(),
        relayOnly = false,
        pairingOpen = false,
        current = true,
        providers = emptyList(),
    )

    private fun makeRow(
        host: HostId,
        name: String,
        filing: Filing = Filing.Open,
        expanded: Boolean = false,
        members: Int = 1,
        phaseSinceMs: Long = 1_000L,
    ): FleetRow = FleetRow(
        card = FleetCard(
            agent = AgentKey(host, UUID.nameUUIDFromBytes(name.toByteArray())),
            name = name,
            handle = name,
            kind = Kind.ClaudeSdk,
            attention = Attention.Working,
            cwd = "/repo",
            phaseSinceMs = phaseSinceMs,
            host = "desk",
            hostReach = Reach.Online(RouteKind.Direct),
            children = members - 1,
            familyAttention = Attention.Working,
            members = members,
            descendants = 0,
            backgroundJobs = 0,
            closeHeld = false,
            filing = filing,
            topLevel = true,
            native = false,
        ),
        depth = 0,
        expanded = expanded,
        secondLine = SecondLine.Blank,
    )

    @Test
    fun showPopulatesSectionsCountsSubtitleAndExpandedFamilies() {
        var now = 5_000L
        val store = FleetStore(clock = { now })

        val head = makeRow(hostA, "parent", expanded = true, members = 3, phaseSinceMs = 8_000L)
        val child = makeRow(hostA, "child", expanded = false, members = 1, phaseSinceMs = 6_000L)
        val folded = makeRow(hostA, "folded-family", expanded = false, members = 4, phaseSinceMs = 4_000L)
        val settled = makeRow(hostA, "done", filing = Filing.Closed, phaseSinceMs = 2_000L)

        val liveSection = FleetSection(
            kind = SectionKind.Live,
            families = 2,
            foldedByDefault = false,
            rows = listOf(head, child, folded),
        )
        val closedSection = FleetSection(
            kind = SectionKind.Closed,
            families = 1,
            foldedByDefault = true,
            rows = listOf(settled),
        )

        store.show(
            view = FleetView(sections = listOf(liveSection, closedSection), needYou = 2, working = 3),
            hosts = listOf(
                makeHost(localHost, "Pixel", local = true),
                makeHost(hostA, "desk"),
                makeHost(hostB, "mini", reach = Reach.Away),
            ),
        )

        val state = store.snapshot
        assertTrue(state.loaded)
        assertEquals(2, state.needYou)
        assertEquals(3, state.working)
        assertEquals(setOf(head.id), state.expandedFamilies)
        // Expanded head counts 1 + child 1 + folded family (members=4) = 6 open agents; closed excluded.
        assertEquals("2 need you · 6 agents", state.subtitle)
        assertEquals(8_000L, state.orderedAtMs)
        assertEquals(2, state.machines.size)
        assertEquals("mini", state.awayHost)
        assertEquals("mini", state.heldAway)
        assertTrue(state.folded(closedSection))
        assertFalse(state.folded(liveSection))

        store.toggleSection(SectionKind.Closed)
        assertFalse(store.snapshot.folded(closedSection))

        // Clock never goes backwards, advances forward.
        store.refreshOrder(7_000L)
        assertEquals(8_000L, store.snapshot.orderedAtMs)
        now = 12_000L
        store.refreshOrder()
        assertEquals(12_000L, store.snapshot.orderedAtMs)
    }

    @Test
    fun heldAwayPersistsWhileOfflineAndClearsWhenOnline() {
        val store = FleetStore()
        val emptyView = FleetView(sections = emptyList(), needYou = 0, working = 1)

        store.show(emptyView, listOf(makeHost(hostA, "desk", reach = Reach.Away)))
        assertEquals("desk", store.snapshot.heldAway)
        assertEquals("Nothing needs you · 1 working", store.snapshot.subtitle)

        // App returns from background: host temporarily reads Offline before relay confirms Away again.
        store.show(emptyView, listOf(makeHost(hostA, "desk", reach = Reach.Offline(Offline.Plain))))
        assertEquals("desk", store.snapshot.heldAway)

        // Host comes online: heldAway clears.
        store.show(emptyView, listOf(makeHost(hostA, "desk", reach = Reach.Online(RouteKind.Direct))))
        assertNull(store.snapshot.heldAway)
    }

    @Test
    fun relayTroubleAndStoreBundleActionsForwardCleanly() = runTest {
        val recordedActs = mutableListOf<Pair<AgentAct, AgentKey>>()
        val warningResult: DeleteWarning = DeleteWarning.Goes(
            WorkspaceGoes(
                branch = "feat",
                uncommitted = ChangeTotals(files = 2, added = 10, removed = 3),
            ),
        )
        val bundle = StoreBundle(
            account = AccountId("ada"),
            scope = this,
            acting = { act, key ->
                recordedActs.add(act to key)
                Result.success(Unit)
            },
            warning = { Result.success(warningResult) },
        )
        val agent = AgentKey(hostA, UUID.randomUUID())

        bundle.fleet.relay(RelayLink.SignInAgain)
        assertEquals(
            "Sign in again to reach your computers from anywhere",
            bundle.fleet.snapshot.relayTrouble,
        )
        bundle.fleet.relay(RelayLink.VersionMismatch)
        assertEquals(
            "Update amux to reach your computers from anywhere",
            bundle.fleet.snapshot.relayTrouble,
        )
        bundle.fleet.relay(RelayLink.Connected)
        assertNull(bundle.fleet.snapshot.relayTrouble)

        bundle.fleet.setSearchWords("fix")
        bundle.fleet.setOnly(FleetOnly.NeedsYou)
        assertEquals(FleetFilter(words = "fix", only = FleetOnly.NeedsYou), bundle.fleet.snapshot.filter)

        assertTrue(bundle.fleet.pin(agent).isSuccess)
        assertTrue(bundle.fleet.settle(agent).isSuccess)
        assertTrue(bundle.fleet.unsettle(agent).isSuccess)
        assertTrue(bundle.fleet.rename("  new-name  ", agent).isSuccess)
        assertTrue(bundle.fleet.rename("   ", agent).isFailure)
        assertTrue(bundle.fleet.cancelStop(agent).isSuccess)
        assertTrue(bundle.fleet.delete(agent, discardingChanges = false).isSuccess)
        assertTrue(bundle.fleet.delete(agent, discardingChanges = true).isSuccess)
        assertEquals(Result.success(warningResult), bundle.fleet.previewDelete(agent))

        assertEquals(
            listOf(
                AgentAct.Pin,
                AgentAct.Close,
                AgentAct.Unfile,
                AgentAct.Rename("new-name"),
                AgentAct.CancelClose,
                AgentAct.Delete,
                AgentAct.DeleteDiscardingChanges,
            ),
            recordedActs.map { it.first },
        )
    }
}
