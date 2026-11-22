package sh.amux.words

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.AgentKey
import sh.amux.values.Attention
import sh.amux.values.ExitCause
import sh.amux.values.Filing
import sh.amux.values.FleetCard
import sh.amux.values.FleetRow
import sh.amux.values.Kind
import sh.amux.values.Offline
import sh.amux.values.Reach
import sh.amux.values.RouteKind
import sh.amux.values.SecondLine
import java.time.Instant

class RowStateTest {
    private val hostBytes = List(16) { it }
    private val agentBytes = List(16) { it + 16 }

    private fun row(
        attention: Attention,
        kind: Kind = Kind.ClaudeSdk,
        reach: Reach = Reach.Online(RouteKind.Direct),
        exitCause: ExitCause? = null,
        filing: Filing = Filing.Open,
        native: Boolean = false,
        topLevel: Boolean = true,
        name: String = "refactor-auth",
        phaseSinceMs: Long = 0L,
    ): FleetRow = FleetRow(
        card = FleetCard(
            agent = AgentKey(host = hostBytes, agent = agentBytes),
            name = name,
            handle = name,
            kind = kind,
            provider = if (kind == Kind.Codex) "codex" else "claude",
            providerName = if (kind == Kind.Codex) "Codex" else "Claude",
            attachable = false,
            attention = attention,
            cwd = "~/src/amux",
            phaseSinceMs = phaseSinceMs,
            host = "Studio.local",
            hostReach = reach,
            children = 0,
            familyAttention = attention,
            members = 1,
            descendants = 0,
            backgroundJobs = 0,
            closeHeld = false,
            filing = filing,
            topLevel = topLevel,
            native = native,
            branch = null,
            exitCause = exitCause,
            repository = null,
        ),
        depth = 0,
        expanded = false,
        secondLine = SecondLine.Blank,
        loud = null,
    )

    @Test
    fun eachAttentionSpeaksInWords() {
        assertEquals(RowState.Working, RowState.of(row(Attention.Working)))
        assertEquals("Working", RowState.of(row(Attention.Working)).word)
        assertEquals("Starting", RowState.of(row(Attention.Starting)).word)
        assertEquals("Idle", RowState.of(row(Attention.Idle)).word)
        assertEquals("Finished", RowState.of(row(Attention.Exited, exitCause = ExitCause.Finished)).word)
        assertNull(RowState.of(row(Attention.Exited, exitCause = ExitCause.Finished)).elaboration)
        assertEquals("Stopped", RowState.of(row(Attention.Exited, exitCause = ExitCause.Ended)).word)
        assertEquals("Stopped", RowState.of(row(Attention.Exited, exitCause = ExitCause.Failed("code 1"))).word)
        assertNull(RowState.of(row(Attention.Exited, exitCause = ExitCause.Failed("code 1"))).elaboration)
    }

    @Test
    fun needingYouSaysSoAndIsTheOnlyMark() {
        val state = RowState.of(row(Attention.NeedsYou))
        assertEquals(RowState.NeedsYou, state)
        assertEquals("Needs you", state.word)
        assertTrue(state.needsYou)
        assertEquals("Needs you", state.spoken)
    }

    @Test
    fun aKindThisBuildCannotOpenOutranksEverything() {
        val state = RowState.of(row(Attention.NeedsYou, kind = Kind.Unspecified, reach = Reach.Offline(Offline.Plain)))
        assertEquals(RowState.Unsupported, state)
        assertEquals("update amux to open it", state.elaboration)
    }

    @Test
    fun anOfflineHostOutranksARequest() {
        val state = RowState.of(row(Attention.NeedsYou, reach = Reach.Offline(Offline.Plain)))
        assertEquals(RowState.HostOffline("Studio"), state)
        assertEquals("Studio offline", state.word)
        assertTrue(state.namesTheHost)
        assertEquals(RowState.HostOffline("Studio"), RowState.of(row(Attention.Working, reach = Reach.Offline(Offline.Revoked))))
        assertEquals(RowState.HostOffline("Studio"), RowState.of(row(Attention.Working, reach = Reach.Offline(Offline.SignedOut))))
    }

    @Test
    fun anEndingOutranksAnOfflineHost() {
        assertEquals(
            RowState.Exited(ExitCause.Finished),
            RowState.of(row(Attention.Exited, reach = Reach.Offline(Offline.Plain), exitCause = ExitCause.Finished)),
        )
    }

    @Test
    fun anAwayHostIsNotLive() {
        val state = RowState.of(row(Attention.Working, reach = Reach.Away))
        assertEquals(RowState.HostAway("Studio"), state)
        assertEquals("not live", state.elaboration)
        assertEquals("Studio is away, not live", state.spoken)
    }

    @Test
    fun aSettledAgentIsSettledUnlessItFailed() {
        assertEquals(RowState.Settled, RowState.of(row(Attention.Exited, exitCause = ExitCause.Ended, filing = Filing.Closed)))
        assertEquals(RowState.Settled, RowState.of(row(Attention.Exited, exitCause = ExitCause.Finished, filing = Filing.Closed)))
        assertEquals("Settled", RowState.of(row(Attention.Exited, exitCause = ExitCause.Ended, filing = Filing.Closed)).spoken)
        val failed = RowState.of(row(Attention.Exited, exitCause = ExitCause.Failed("code 1"), filing = Filing.Closed))
        assertEquals(RowState.Exited(ExitCause.Failed("code 1")), failed)
        assertNull(failed.spoken)
        assertEquals(RowState.Exited(ExitCause.Ended), RowState.of(row(Attention.Exited, exitCause = ExitCause.Ended)))
    }

    @Test
    fun aHostReachedAnyWayLetsTheAgentSpeak() {
        assertEquals(RowState.Working, RowState.of(row(Attention.Working, reach = Reach.Online(RouteKind.Direct))))
        assertEquals(RowState.Working, RowState.of(row(Attention.Working, reach = Reach.Online(RouteKind.Relay))))
    }

    @Test
    fun everyStateHasADistinctName() {
        val states = listOf(
            RowState.NeedsYou,
            RowState.Working,
            RowState.Starting,
            RowState.HostOffline("a"),
            RowState.HostAway("a"),
            RowState.Unsupported,
            RowState.Exited(ExitCause.Ended),
            RowState.Settled,
            RowState.Idle,
        )
        assertEquals(states.size, states.map { it.name }.toSet().size)
    }

    @Test
    fun aSubagentIsNeverPinnedOrDeletedOnItsOwnAndFleetRowExtensionsWork() {
        val settled = row(Attention.Exited, exitCause = ExitCause.Finished, filing = Filing.Closed)
        assertTrue(settled.deletable)
        val nativeSettled = settled.copy(card = settled.card.copy(native = true, topLevel = false))
        assertFalse(nativeSettled.deletable)
        assertTrue(nativeSettled.native)

        val live = row(Attention.Working)
        assertTrue(live.pinnable)
        val nativeLive = live.copy(card = live.card.copy(native = true))
        assertFalse(nativeLive.pinnable)

        val nameless = row(Attention.Idle, name = "", phaseSinceMs = 1_000L)
        assertEquals("agent", nameless.name)
        assertEquals("5m", nameless.age(Instant.ofEpochMilli(1_000L + 5L * 60_000L)))
        assertEquals("0s", nameless.age(Instant.ofEpochMilli(500L)))
    }
}
