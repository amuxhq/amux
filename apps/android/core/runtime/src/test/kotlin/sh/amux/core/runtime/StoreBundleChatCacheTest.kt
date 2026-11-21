package sh.amux.core.runtime

import java.util.UUID
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.ActOutcome
import sh.amux.values.AgentAct
import sh.amux.values.AgentKey
import sh.amux.values.ChatDelta
import sh.amux.values.ChatFrame
import sh.amux.values.Comparison
import sh.amux.values.Composer
import sh.amux.values.ComposerView
import sh.amux.values.Connection
import sh.amux.values.ControlsSummary
import sh.amux.values.Draft
import sh.amux.values.FormValue
import sh.amux.values.FrozenReview
import sh.amux.values.InputState
import sh.amux.values.KeysMoved
import sh.amux.values.Kind
import sh.amux.values.Overview
import sh.amux.values.PageOutcome
import sh.amux.values.PhaseView
import sh.amux.values.QuestionResponse
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Segment
import sh.amux.values.SendOutcome
import sh.amux.values.SettingChange

@OptIn(ExperimentalCoroutinesApi::class)
class StoreBundleChatCacheTest {
    private val host = HostId(UUID.fromString("11111111-1111-1111-1111-111111111111"))

    private inner class FakeOpenChat(
        override val id: Long,
        val agent: AgentKey,
        var caughtUp: Boolean = true,
        val initialDraft: Draft? = null,
    ) : OpenChat {
        var closed = false
        val keptDrafts = mutableListOf<Draft>()

        override fun close() {
            closed = true
        }

        override fun opening(): ChatDelta = ChatDelta(
            chat = id,
            rows = emptyList(),
            frame = makeFrame(agent, caughtUp),
            keys = KeysMoved.Reload(emptyList()),
        )

        override fun follow(following: Boolean) {}
        override fun rowsFor(keys: List<String>): List<Row> = emptyList()
        override suspend fun send(draft: Draft): Result<SendOutcome> =
            Result.success(SendOutcome(listOf(1), InputState.Sent))
        override suspend fun answer(ask: String, choice: Int, note: String?): ActOutcome = ActOutcome.Done
        override suspend fun answerQuestions(ask: String, responses: List<QuestionResponse>): ActOutcome = ActOutcome.Done
        override suspend fun replyInstead(ask: String, text: String, soFar: List<QuestionResponse>): ActOutcome = ActOutcome.Done
        override suspend fun answerForm(ask: String, values: List<FormValue>): ActOutcome = ActOutcome.Done
        override fun toggleRun(member: String) {}
        override fun toggleGroup(member: String) {}
        override fun foldTurns(on: Boolean) {}
        override fun toggleTurn(end: String) {}
        override suspend fun withdraw(inputId: List<Int>): ActOutcome = ActOutcome.Done
        override fun draftOf(inputId: List<Int>): Draft? = null
        override suspend fun sendNow(inputId: List<Int>): ActOutcome = ActOutcome.Done
        override suspend fun resend(inputId: List<Int>): SendOutcome? = null
        override fun discard(inputId: List<Int>) {}
        override suspend fun interrupt(): ActOutcome = ActOutcome.Done
        override suspend fun stopJob(key: String): ActOutcome = ActOutcome.Done
        override suspend fun changeSetting(setting: SettingChange): ActOutcome = ActOutcome.Done
        override suspend fun resume(draft: Draft): ActOutcome = ActOutcome.Done
        override suspend fun pageOlder(rows: Int): PageOutcome = PageOutcome.Arrived(0)
        override suspend fun putBlob(data: ByteArray, name: String, mime: String): Result<sh.amux.values.BlobRef> =
            Result.failure(RuntimeFailure("unused"))
        override fun blob(hash: List<Int>): ByteArray? = null
        override suspend fun review(comparison: Comparison): Result<FrozenReview> =
            Result.failure(RuntimeFailure("unused"))
        override suspend fun openOverview(comparison: Comparison): Result<Overview> =
            Result.success(Overview(emptyList(), emptyList()))
        override fun keptDraft(): Draft? = initialDraft
        override fun keep(draft: Draft) {
            keptDrafts.add(draft)
        }
    }

    private fun makeFrame(agent: AgentKey, caughtUp: Boolean): ChatFrame = ChatFrame(
        agent = agent,
        name = "agent",
        kind = Kind.ClaudeSdk,
        phase = PhaseView.Idle,
        composer = ComposerView(mode = Composer.Send, switchedAway = false),
        controls = ControlsSummary(),
        connection = Connection.Live,
        caughtUp = caughtUp,
        hasOlder = false,
        arrivalsHeld = false,
        queue = emptyList(),
        underway = emptyList(),
        refused = emptyList(),
    )

    @Test
    fun cachesShownChatsRoutesDeltasRetainsForFiveMinutesAndEvictsAfterExpiry() = runTest(StandardTestDispatcher()) {
        var nowMs = 1_000L
        var nextId = 1L
        val opened = mutableListOf<FakeOpenChat>()

        val bundle = StoreBundle(
            account = AccountId("ada"),
            scope = this,
            clock = { nowMs },
            sessionRetentionMs = StoreBundle.DEFAULT_SESSION_RETENTION_MS,
            opener = { key ->
                FakeOpenChat(id = nextId++, agent = key, initialDraft = draftOf("remembered")).also {
                    opened.add(it)
                }
            },
        )

        val parentKey = AgentKey(host, UUID.randomUUID())
        val childKey = AgentKey(host, UUID.randomUUID())

        val parentLease = bundle.acquireChat(parentKey)
        val parentModel = parentLease.model
        val duplicateParent = bundle.acquireChat(parentKey)
        assertSame(parentModel, duplicateParent.model)
        duplicateParent.release()
        assertEquals("remembered", parentModel.draft)
        assertEquals(1, opened.size)
        assertFalse(opened[0].closed)

        // Delta delivered through FleetEvent.Chat reaches the cached ChatModel.
        bundle.moved(
            FleetEvent.Chat(
                ChatDelta(
                    chat = opened[0].id,
                    rows = emptyList(),
                    keys = KeysMoved.Edges(above = listOf("r1"), below = emptyList(), dropped = 0),
                ),
            ),
        )
        assertEquals(listOf("r1"), parentModel.keys)

        // Opening a child keeps the parent open too.
        bundle.acquireChat(childKey)
        assertFalse(opened[0].closed)
        assertEquals(2, opened.size)

        // Leaving parent retains it during the 5-minute window.
        parentLease.release()
        nowMs += 4 * 60_000L
        advanceTimeBy(4 * 60_000L)
        runCurrent()
        assertFalse("still retained within 5-minute window", opened[0].closed)

        // Coming back before expiry leases the same ChatModel without opening, and cancels eviction.
        val again = bundle.leaseCached(parentKey)
        assertSame(parentModel, again?.model)
        nowMs += 2 * 60_000L
        advanceTimeBy(2 * 60_000L)
        runCurrent()
        assertFalse("re-shown chat is never evicted", opened[0].closed)
        assertEquals(2, opened.size)

        // Leaving again and passing 5 minutes closes and evicts it cleanly.
        again!!.release()
        nowMs += StoreBundle.DEFAULT_SESSION_RETENTION_MS
        advanceTimeBy(StoreBundle.DEFAULT_SESSION_RETENTION_MS)
        runCurrent()
        assertTrue(opened[0].closed)
        assertEquals("an evicted chat is not held", null, bundle.leaseCached(parentKey))
        assertFalse("still-shown child remains open", opened[1].closed)

        // Detaching profile closes all remaining chats cleanly.
        bundle.attach(null)
        assertTrue(opened[1].closed)
    }

    @Test
    fun deleteStopsDraftSavingAndClosesCachedChatAndWarmSurvivesUntilCaughtUp() = runTest(StandardTestDispatcher()) {
        var nowMs = 10_000L
        var nextId = 1L
        val opened = mutableMapOf<AgentKey, FakeOpenChat>()
        val recordedActs = mutableListOf<Pair<AgentAct, AgentKey>>()

        val bundle = StoreBundle(
            account = AccountId("ada"),
            scope = this,
            clock = { nowMs },
            sessionRetentionMs = StoreBundle.DEFAULT_SESSION_RETENTION_MS,
            opener = { key ->
                FakeOpenChat(id = nextId++, agent = key, caughtUp = false).also { opened[key] = it }
            },
            acting = { act, key ->
                recordedActs.add(act to key)
                Result.success(Unit)
            },
        )

        val agentA = AgentKey(host, UUID.randomUUID())
        val modelA = bundle.acquireChat(agentA).model
        modelA.setDraft("before delete")
        assertEquals(1, opened[agentA]!!.keptDrafts.size)

        // Held stop & cancel stop route cleanly through StoreBundle.
        assertTrue(bundle.close(agentA).isSuccess)
        assertTrue(bundle.cancelClose(agentA).isSuccess)
        assertEquals(listOf(AgentAct.Close, AgentAct.CancelClose), recordedActs.map { it.first })

        // Deleting agent stops draft persistence and closes its cached chat.
        assertTrue(bundle.delete(agentA).isSuccess)
        assertTrue(opened[agentA]!!.closed)
        modelA.setDraft("after delete")
        assertEquals("deleted agent must never save drafts", 1, opened[agentA]!!.keptDrafts.size)

        // Warming a chat keeps it alive across keepSessions() until caughtUp lands.
        val agentB = AgentKey(host, UUID.randomUUID())
        var warmResult = false
        val warmJob = launch {
            warmResult = bundle.warm(agentB, timeoutMs = 5_000L)
        }
        runCurrent()
        assertFalse(opened[agentB]!!.closed)
        bundle.keepSessions()
        assertFalse("warming chat survives keepSessions()", opened[agentB]!!.closed)

        // Deliver caught-up frame -> warm completes and evicts unshown chat.
        val chatB = opened[agentB]!!
        bundle.moved(
            FleetEvent.Chat(
                ChatDelta(
                    chat = chatB.id,
                    rows = emptyList(),
                    frame = makeFrame(agentB, caughtUp = true),
                ),
            ),
        )
        warmJob.join()
        assertTrue(warmResult)
        assertTrue("unshown warmed chat closes once warm finishes", chatB.closed)
    }

    @Test
    fun overlappingRouteLeasesKeepChatShownAcrossTransitionAndEvictOnlyAfterLastLeaves() =
        runTest(StandardTestDispatcher()) {
            var nowMs = 5_000L
            var nextId = 1L
            val opened = mutableListOf<FakeOpenChat>()

            val bundle = StoreBundle(
                account = AccountId("ada"),
                scope = this,
                clock = { nowMs },
                sessionRetentionMs = StoreBundle.DEFAULT_SESSION_RETENTION_MS,
                opener = { key ->
                    FakeOpenChat(id = nextId++, agent = key).also { opened.add(it) }
                },
            )

            val agent = AgentKey(host, UUID.randomUUID())

            // Route A (Conversation) acquires lease, then Route B (Changes) acquires lease
            // during route transition before Route A disposes.
            val chatLease = bundle.acquireChat(agent)
            val reviewLease = bundle.acquireChat(agent)
            assertSame(chatLease.model, reviewLease.model)
            assertEquals(1, opened.size)

            // Route A disposes; Route B still holds the agent open.
            chatLease.release()
            chatLease.release() // idempotent

            // Advancing far past sessionRetentionMs must NEVER evict while Route B is active.
            nowMs += StoreBundle.DEFAULT_SESSION_RETENTION_MS * 3
            advanceTimeBy(StoreBundle.DEFAULT_SESSION_RETENTION_MS * 3)
            runCurrent()
            assertFalse("chat must stay open while review route holds a lease", opened[0].closed)

            // Route B releases -> retention timer starts and evicts after 5 minutes.
            reviewLease.release()
            runCurrent()
            assertFalse("still retained right after last viewer leaves", opened[0].closed)

            nowMs += StoreBundle.DEFAULT_SESSION_RETENTION_MS
            advanceTimeBy(StoreBundle.DEFAULT_SESSION_RETENTION_MS)
            runCurrent()
            assertTrue("evicted after retention window elapses", opened[0].closed)

            // Detaching profile bumps profileVersion so routes know to re-evaluate lifecycle.
            val versionBefore = bundle.profileVersion.value
            bundle.attach(null)
            assertEquals(versionBefore + 1, bundle.profileVersion.value)
        }
}
