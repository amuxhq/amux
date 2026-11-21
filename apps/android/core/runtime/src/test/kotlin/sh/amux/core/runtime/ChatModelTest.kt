package sh.amux.core.runtime

import java.nio.file.Files
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.ActOutcome
import sh.amux.values.AgentKey
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.AskMoved
import sh.amux.values.AttachmentView
import sh.amux.values.BlobRef
import sh.amux.values.CallPhase
import sh.amux.values.CardState
import sh.amux.values.ChangeTotals
import sh.amux.values.Changeability
import sh.amux.values.Changeable
import sh.amux.values.ChangedFile
import sh.amux.values.Changes
import sh.amux.values.ChatAgents
import sh.amux.values.ChatDelta
import sh.amux.values.ChatFrame
import sh.amux.values.ClientCommand
import sh.amux.values.CommandOutcome
import sh.amux.values.CommandView
import sh.amux.values.Comparison
import sh.amux.values.Composer
import sh.amux.values.ComposerView
import sh.amux.values.Connection
import sh.amux.values.ControlsSummary
import sh.amux.values.Diff
import sh.amux.values.Draft
import sh.amux.values.DraftAttachment
import sh.amux.values.DraftPart
import sh.amux.values.ExitCause
import sh.amux.values.FieldProblem
import sh.amux.values.FileStatus
import sh.amux.values.Folder
import sh.amux.values.FormField
import sh.amux.values.FormFieldKind
import sh.amux.values.FormValue
import sh.amux.values.FrozenReview
import sh.amux.values.Git
import sh.amux.values.GroupState
import sh.amux.values.GroupView
import sh.amux.values.InputState
import sh.amux.values.JobStop
import sh.amux.values.KeysMoved
import sh.amux.values.Kind
import sh.amux.values.Lands
import sh.amux.values.ModelChoice
import sh.amux.values.OpenedAgent
import sh.amux.values.Overview
import sh.amux.values.PageOutcome
import sh.amux.values.PhaseView
import sh.amux.values.QuestionResponse
import sh.amux.values.QueuedRow
import sh.amux.values.Refusal
import sh.amux.values.RefusalReason
import sh.amux.values.RefusedPrompt
import sh.amux.values.ReviewComment
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Run
import sh.amux.values.RunCounts
import sh.amux.values.Segment
import sh.amux.values.SendOutcome
import sh.amux.values.SentPrompt
import sh.amux.values.SettingChange
import sh.amux.values.SettingsView
import sh.amux.values.SubagentGroup
import sh.amux.values.Underway
import sh.amux.values.Waiting
import sh.amux.words.AgentStanding
import sh.amux.words.ChatWords
import sh.amux.words.EMPTY_GROUP_STATE
import sh.amux.words.nativeStop
import sh.amux.words.subagentGroupStanding
import sh.amux.words.subagentGroupState

@OptIn(ExperimentalCoroutinesApi::class)
class ChatModelTest {
    // Runs what a model launches at once, for tests that never wait on it.
    private val idle = CoroutineScope(Dispatchers.Unconfined)

    private data class ChatChanges(
        val keys: MutableList<String> = mutableListOf(),
        var reloaded: Boolean = false,
        var session: Boolean = false,
    )

    private class FakeChat(
        rows: List<Row>,
        var current: ChatFrame,
    ) : ChatSource {
        val ordered = rows.toMutableList()
        var card: AskCard? = null
        var pending = ChatChanges()
        val fetched = mutableListOf<List<String>>()
        val paged = mutableListOf<Int>()
        val earlier = mutableListOf<Row>()
        val sent = mutableListOf<Draft>()
        val resumed = mutableListOf<Draft>()
        val withdrawn = mutableListOf<List<Int>>()
        val sentNow = mutableListOf<List<Int>>()
        val discarded = mutableListOf<List<Int>>()
        val toggled = mutableListOf<String>()
        val groupsToggled = mutableListOf<String>()
        val answeredChoices = mutableListOf<Triple<String, Int, String?>>()
        val responded = mutableListOf<List<QuestionResponse>>()
        val replied = mutableListOf<String>()
        val submittedForms = mutableListOf<List<FormValue>>()
        val drafts = mutableMapOf<List<Int>, Draft>()
        var interrupts = 0

        /** When set, an interrupt waits for this before it answers. */
        var interruptAnswer: CompletableDeferred<ActOutcome?>? = null
        var switchBacks = 0
        val stopped = mutableListOf<String>()
        var agents = ChatAgents(emptyList(), emptyList(), EMPTY_GROUP_STATE)
        var facts: Overview? = null
        var offered: SettingsView? = null
        val changed = mutableListOf<SettingChange>()
        var working: FrozenReview? = null
        var reviews = 0
        val listings = mutableListOf<Comparison>()
        var files: Changes? = null
        var overviewGate: kotlinx.coroutines.CompletableDeferred<Unit>? = null
        var overviewFailure: RuntimeFailure? = null
        val draftEvents = mutableListOf<String>()
        var cap = Int.MAX_VALUE
        val follows = mutableListOf<Boolean>()
        var following = true
        val held = mutableListOf<Row>()
        var pageOutcomeOverride: PageOutcome? = null
        var blobFailure: RuntimeFailure? = null
        val stored = mutableListOf<String>()
        val landed = mutableMapOf<List<Int>, ByteArray>()
        val failedFetches = mutableSetOf<List<Int>>()
        val fetchedAgain = mutableListOf<List<Int>>()
        var initialKeptDraft: Draft? = null
        val keptDrafts = mutableListOf<Draft>()
        val commands = mutableListOf<Draft>()
        var commandOutcome: CommandOutcome? = CommandOutcome.Send

        private var sentKeys: List<String> = emptyList()
        private var sentFrame: ChatFrame? = null
        private var hasSentCard = false
        private var sentCard: AskCard? = null
        private var hasSentFacts = false
        private var sentFacts: Overview? = null
        private var hasSentOffered = false
        private var sentOffered: SettingsView? = null
        private var sentAgents: ChatAgents? = null

        override fun opening(): ChatDelta {
            pending = ChatChanges()
            sentKeys = ordered.map { it.id }
            sentFrame = drawn(current)
            hasSentCard = true
            sentCard = card
            hasSentFacts = true
            sentFacts = facts
            hasSentOffered = true
            sentOffered = offered
            sentAgents = agents
            return ChatDelta(
                chat = 0L,
                rows = emptyList(),
                agents = agents,
                ask = AskMoved(card = card),
                frame = current,
                keys = KeysMoved.Reload(sentKeys),
                overview = facts,
                settings = offered,
            )
        }

        fun delta(): ChatDelta? {
            val taken = pending
            pending = ChatChanges()
            val now = ordered.map { it.id }
            var keysMoved: KeysMoved? = null
            var wanted = emptySet<String>()
            val edgeResult = if (!taken.reloaded) edges(sentKeys, now) else null
            if (edgeResult != null) {
                val (above, below, dropped) = edgeResult
                if (above.isNotEmpty() || below.isNotEmpty() || dropped > 0) {
                    keysMoved = KeysMoved.Edges(above = above, below = below, dropped = dropped)
                }
                wanted = taken.keys.toSet().intersect(now.toSet()) - below.toSet() - above.toSet()
            } else if (taken.reloaded || sentKeys.isNotEmpty() || now.isNotEmpty()) {
                keysMoved = KeysMoved.Reload(now)
            }
            sentKeys = now
            val rows = ordered.filter { it.id in wanted }
            val nextFrame = if (drawn(current) == sentFrame) null else current
            sentFrame = drawn(current)
            val nextAsk = if (hasSentCard && sentCard == card) null else AskMoved(card = card)
            hasSentCard = true
            sentCard = card
            val nextOverview = if (hasSentFacts && sentFacts == facts) null else facts
            hasSentFacts = true
            sentFacts = facts
            val nextSettings = if (hasSentOffered && sentOffered == offered) null else offered
            hasSentOffered = true
            sentOffered = offered
            val nextAgents = if (sentAgents == agents) null else agents
            sentAgents = agents
            if (keysMoved == null && rows.isEmpty() && nextFrame == null && nextAsk == null &&
                nextOverview == null && nextSettings == null && nextAgents == null
            ) {
                return null
            }
            return ChatDelta(
                chat = 0L,
                rows = rows,
                agents = nextAgents,
                ask = nextAsk,
                frame = nextFrame,
                keys = keysMoved,
                overview = nextOverview,
                settings = nextSettings,
            )
        }

        fun deliver(to: ChatModel) {
            delta()?.let { to.apply(it) }
        }

        fun arrive(rows: List<Row>) {
            if (following) {
                ordered.addAll(rows)
                pending.keys.addAll(rows.map { it.id })
                trim()
            } else {
                held.addAll(rows)
                current = current.copy(arrivalsHeld = true)
                pending.session = true
            }
        }

        private fun trim() {
            while (ordered.size > cap) {
                pending.keys.add(ordered.removeAt(0).id)
            }
        }

        override fun follow(following: Boolean) {
            follows.add(following)
            this.following = following
            if (!following || held.isEmpty()) return
            current = current.copy(arrivalsHeld = false)
            pending.session = true
            if (held.size > cap) {
                val combined = (ordered + held).takeLast(cap)
                ordered.clear()
                ordered.addAll(combined)
                pending.reloaded = true
            } else {
                ordered.addAll(held)
                pending.keys.addAll(held.map { it.id })
                trim()
            }
            held.clear()
        }

        override fun rowsFor(keys: List<String>): List<Row> {
            fetched.add(keys)
            val set = keys.toSet()
            return ordered.filter { it.id in set }
        }

        override suspend fun send(draft: Draft): Result<SendOutcome> {
            sent.add(draft)
            return Result.success(SendOutcome(inputId = listOf(sent.size), state = InputState.Sent))
        }

        override suspend fun answer(ask: String, choice: Int, note: String?): ActOutcome {
            answeredChoices.add(Triple(ask, choice, note))
            return ActOutcome.Done
        }

        override suspend fun answerQuestions(ask: String, responses: List<QuestionResponse>): ActOutcome {
            responded.add(responses)
            return ActOutcome.Done
        }

        override suspend fun replyInstead(ask: String, text: String, soFar: List<QuestionResponse>): ActOutcome {
            replied.add(text)
            responded.add(soFar)
            return ActOutcome.Done
        }

        override suspend fun answerForm(ask: String, values: List<FormValue>): ActOutcome {
            submittedForms.add(values)
            return ActOutcome.Done
        }

        override fun toggleRun(member: String) {
            toggled.add(member)
            val last = ordered.firstOrNull { it.id == member }?.run?.last ?: return
            for (i in ordered.indices) {
                val run = ordered[i].run
                if (run != null && run.last == last) {
                    ordered[i] = ordered[i].copy(run = run.copy(open = !run.open))
                    pending.keys.add(ordered[i].id)
                }
            }
        }

        override fun toggleGroup(member: String) {
            groupsToggled.add(member)
            val first = ordered.firstOrNull { it.id == member }?.group?.first ?: return
            for (i in ordered.indices) {
                val grp = ordered[i].group
                if (grp != null && grp.first == first) {
                    val nowOpen = !grp.open
                    ordered[i] = ordered[i].copy(
                        group = grp.copy(open = nowOpen),
                        collapsed = !nowOpen && ordered[i].id != first,
                    )
                    pending.keys.add(ordered[i].id)
                }
            }
        }

        override fun foldTurns(on: Boolean) {}
        override fun toggleTurn(end: String) {}

        override suspend fun withdraw(inputId: List<Int>): ActOutcome {
            withdrawn.add(inputId)
            return ActOutcome.Done
        }

        override fun draftOf(inputId: List<Int>): Draft? = drafts[inputId]

        override suspend fun sendNow(inputId: List<Int>): ActOutcome {
            sentNow.add(inputId)
            return ActOutcome.Done
        }

        override suspend fun resend(inputId: List<Int>): SendOutcome? = null

        override fun discard(inputId: List<Int>) {
            draftEvents.add("discard")
            discarded.add(inputId)
            current = current.copy(
                underway = current.underway.filterNot { it.inputId == inputId },
                refused = current.refused.filterNot { it.inputId == inputId },
            )
        }

        override suspend fun interrupt(): ActOutcome? {
            interrupts += 1
            val gate = interruptAnswer ?: return ActOutcome.Done
            return gate.await()
        }

        override suspend fun stopJob(key: String): ActOutcome {
            stopped.add(key)
            return ActOutcome.Done
        }

        override suspend fun switchBack(): ActOutcome {
            switchBacks += 1
            return ActOutcome.Done
        }

        override suspend fun command(draft: Draft): CommandOutcome? {
            commands.add(draft)
            return commandOutcome
        }

        override suspend fun changeSetting(setting: SettingChange): ActOutcome {
            changed.add(setting)
            return ActOutcome.Done
        }

        override suspend fun resume(draft: Draft): ActOutcome {
            resumed.add(draft)
            return ActOutcome.Done
        }

        override suspend fun pageOlder(rows: Int): PageOutcome {
            paged.add(rows)
            pageOutcomeOverride?.let { return it }
            val count = rows.coerceAtMost(earlier.size)
            val page = earlier.subList(earlier.size - count, earlier.size).toList()
            earlier.subList(earlier.size - count, earlier.size).clear()
            ordered.addAll(0, page)
            pending.keys.addAll(page.map { it.id })
            return PageOutcome.Arrived(page.size)
        }

        override suspend fun putBlob(data: ByteArray, name: String, mime: String): Result<BlobRef> {
            stored.add(name)
            blobFailure?.let { return Result.failure(it) }
            return Result.success(BlobRef(hash = listOf(9), name = name, mime = mime, size = data.size.toLong()))
        }

        override fun blob(hash: List<Int>): ByteArray? = landed[hash]
        override fun blobFailed(hash: List<Int>): Boolean = hash in failedFetches
        override fun fetchBlob(hash: List<Int>) { fetchedAgain.add(hash) }

        override suspend fun review(comparison: Comparison): Result<FrozenReview> {
            reviews += 1
            val currentWorking = working ?: return Result.failure(RuntimeFailure("no machine"))
            return Result.success(currentWorking)
        }

        override suspend fun openOverview(comparison: Comparison): Result<Overview> {
            listings.add(comparison)
            overviewGate?.await()
            overviewFailure?.let { return Result.failure(it) }
            val updated = (facts ?: Overview(emptyList(), emptyList(), null, null, null)).copy(changes = files)
            facts = updated
            return Result.success(updated)
        }

        override fun keptDraft(): Draft? = initialKeptDraft
        override fun keep(draft: Draft) {
            draftEvents.add("keep:${draft.words}")
            keptDrafts.add(draft)
        }

        private fun drawn(frame: ChatFrame): ChatFrame {
            val activity = frame.composer.activity
            return if (activity != null && activity.sinceMs > 0L) {
                frame.copy(composer = frame.composer.copy(activity = activity.copy(elapsedMs = 0L)))
            } else {
                frame
            }
        }

        private fun edges(sent: List<String>, now: List<String>): Triple<List<String>, List<String>, Int>? {
            val newest = sent.lastOrNull() ?: return null
            val at = now.indexOf(newest)
            if (at < 0 || now.isEmpty()) return null
            val above = now.subList(at + 1, now.size).toList()
            val below: List<String>
            val dropped: Int
            val oldest = now.indexOf(sent[0])
            if (oldest >= 0) {
                below = now.subList(0, oldest).toList()
                dropped = 0
            } else {
                val kept = sent.indexOf(now[0])
                if (kept < 0) return null
                below = emptyList()
                dropped = kept
            }
            if (sent.size - dropped + above.size + below.size != now.size) return null
            return Triple(above, below, dropped)
        }
    }

    private fun proseRow(id: String, order: Long, text: String = "", run: Run? = null): Row = Row(
        id = id,
        order = order,
        atMs = 0L,
        kind = RowKind.Prose(
            text = listOf(Segment.Text(text.ifEmpty { id })),
            streaming = false,
            workingNote = false,
        ),
        collapsed = false,
        attention = false,
        decision = null,
        group = null,
        opens = null,
        parent = null,
        run = run,
        turn = null,
    )

    private fun subagentCallRow(
        id: String,
        order: Long,
        opensAgent: Int,
        group: SubagentGroup? = null,
    ): Row = Row(
        id = id,
        order = order,
        atMs = 0L,
        kind = RowKind.Subagent(
            description = "Survey $id",
            continued = false,
            state = CallPhase.Succeeded,
            answer = "",
            durationMs = null,
        ),
        collapsed = group?.let { !it.open && it.first != id } ?: false,
        attention = false,
        decision = null,
        group = group,
        opens = listOf(opensAgent),
        parent = null,
        run = null,
        turn = null,
    )

    private fun chatAgents(vararg standing: Pair<Int, PhaseView>): ChatAgents = ChatAgents(
        agents = standing.map { (id, phase) ->
            OpenedAgent(
                agent = AgentKey(host = listOf(1), agent = listOf(id)),
                name = "sub-$id",
                kind = Kind.ClaudeSdk,
                phase = phase,
            )
        },
        groups = emptyList(),
        live = EMPTY_GROUP_STATE,
    )

    private fun chatFrame(
        mode: Composer = Composer.Send,
        caughtUp: Boolean = true,
        hasOlder: Boolean = false,
        phase: PhaseView = PhaseView.Idle,
        kind: Kind = Kind.ClaudeSdk,
        switchedAway: Boolean = false,
        ended: String? = null,
    ): ChatFrame = ChatFrame(
        agent = AgentKey(host = listOf(1), agent = listOf(2)),
        name = "a",
        kind = kind,
        phase = phase,
        composer = ComposerView(mode = mode, switchedAway = switchedAway, activity = null),
        controls = ControlsSummary(),
        connection = Connection.Live,
        caughtUp = caughtUp,
        hasOlder = hasOlder,
        arrivalsHeld = false,
        queue = emptyList(),
        underway = emptyList(),
        refused = emptyList(),
        askInput = null,
        context = null,
        ended = ended,
        git = null,
        signIn = null,
    )

    private fun numbered(range: IntRange): List<Row> = range.map { proseRow("m$it", it.toLong()) }

    private fun offering(names: List<String>, skills: List<String> = emptyList()): SettingsView = SettingsView(
        models = emptyList(),
        efforts = emptyList(),
        permissions = emptyList(),
        modes = emptyList(),
        changeable = Changeability(
            model = Changeable.NotOffered,
            effort = Changeable.NotOffered,
            permission = Changeable.NotOffered,
            mode = Changeable.NotOffered,
        ),
        commands = names.map {
            CommandView(name = it, description = "", argumentHint = "", source = "", sigil = "/", anywhere = false)
        } + skills.map {
            CommandView(name = it, description = "", argumentHint = "", source = "", sigil = "$", anywhere = true)
        },
    )

    @Test
    fun openingReadsKeysLazilyAndDeltaUpdatesSingleKeyedCell() {
        val source = FakeChat(listOf(proseRow("a", 1), proseRow("b", 2), proseRow("c", 3)), chatFrame())
        val model = ChatModel(source = source, scope = idle, loadingHintAfterMs = -1L)

        assertEquals(listOf("a", "b", "c"), model.keys)
        assertTrue("no row is read before its cell is drawn", source.fetched.isEmpty())

        val cellB = model.cell("b")
        assertEquals("b", cellB.row?.id)
        assertSame(cellB, model.cell("b"))
        assertEquals(listOf(listOf("b")), source.fetched)

        val cellC = model.cell("c")
        source.fetched.clear()
        val beforeRev = model.snapshot.revision

        // Streamed revision of c and new row d at top edge.
        source.ordered[2] = proseRow("c", 3, "c, longer")
        source.ordered.add(proseRow("d", 4))
        source.pending.keys.addAll(listOf("c", "d"))
        source.deliver(model)

        assertEquals(listOf("a", "b", "c", "d"), model.keys)
        assertTrue("changed row arrived in delta without refetch", source.fetched.isEmpty())
        assertEquals(proseRow("c", 3, "c, longer").kind, cellC.row?.kind)
        assertTrue(cellC.revision > beforeRev)
        assertEquals(beforeRev, cellB.revision)
        assertEquals(0, model.revisionOf("d"))

        // Unchanged re-delivery of b keeps its revision unchanged.
        val revAfterC = model.snapshot.revision
        source.pending.keys.add("b")
        source.deliver(model)
        assertEquals(revAfterC, model.snapshot.revision)
        assertEquals(beforeRev, cellB.revision)

        // Page below the oldest edge.
        source.ordered.add(0, proseRow("z", 0))
        source.pending.keys.add("z")
        source.deliver(model)
        assertEquals(listOf("z", "a", "b", "c", "d"), model.keys)
    }

    @Test
    fun resetReloadSwapsSequenceInPlaceAndRestoresFollowing() {
        val source = FakeChat(listOf(proseRow("a", 1), proseRow("b", 2)), chatFrame())
        val model = ChatModel(source = source, scope = idle, loadingHintAfterMs = -1L)
        val cellB = model.cell("b")
        model.reading(atNewest = false)
        val beforeToNewest = model.snapshot.toNewest

        source.ordered.clear()
        source.ordered.addAll(listOf(proseRow("b", 20, "b again"), proseRow("x", 21), proseRow("y", 22)))
        source.pending.reloaded = true
        source.pending.session = true
        source.deliver(model)

        assertEquals(listOf("b", "x", "y"), model.keys)
        assertSame("kept key preserves its cell", cellB, model.cell("b"))
        assertEquals(20L, cellB.row?.order)
        assertTrue(model.isFollowing)
        assertFalse(model.newActivity)
        assertEquals(beforeToNewest + 1, model.snapshot.toNewest)
    }

    @Test
    fun followingAndArrivalsHeldAndReturnPathsWork() = runTest {
        val source = FakeChat(numbered(1..50), chatFrame())
        source.cap = 200
        val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

        assertEquals(emptyList<Boolean>(), source.follows)
        model.reading(atNewest = false)
        model.reading(atNewest = false)
        assertEquals(listOf(false), source.follows)

        val beforeToNewest = model.snapshot.toNewest
        source.arrive(listOf(proseRow("m51", 51)))
        source.deliver(model)
        assertTrue(model.arrivalsHeld)
        assertTrue(model.newActivity)
        assertEquals(50, model.keys.size)
        assertEquals(beforeToNewest, model.snapshot.toNewest)

        model.jumpToNewest()
        assertTrue(model.isFollowing)
        assertFalse(model.newActivity)
        assertEquals(beforeToNewest + 1, model.snapshot.toNewest)
        assertEquals(listOf(false, true), source.follows)
        source.deliver(model)
        assertEquals("m51", model.keys.last())

        // Sending also restores following before the send runs.
        model.reading(atNewest = false)
        model.setDraft("go on")
        val sendJob = model.send()
        assertNotNull(sendJob)
        assertTrue(model.isFollowing)
        assertEquals(listOf(false, true, false, true), source.follows)
        sendJob!!.join()
        assertEquals(1, source.sent.size)
    }

    @Test
    fun followingUnderFloodDropsOldestKeysWithoutPaging() {
        val source = FakeChat(numbered(801..1000), chatFrame(hasOlder = true))
        source.cap = 200
        val model = ChatModel(source = source, scope = idle, loadingHintAfterMs = -1L)

        for (n in 1001..1300) {
            source.arrive(listOf(proseRow("m$n", n.toLong())))
            source.deliver(model)
            model.reading(atNewest = true)
        }
        assertTrue(source.paged.isEmpty())
        assertEquals((1101..1300).map { "m$it" }, model.keys)
        assertEquals(emptyList<Boolean>(), source.follows)
    }

    @Test
    fun reachedTopPagesOnceChecksVisibilityHandlesUnreachableAndCollapsedRunSize() = runTest {
        val source = FakeChat(numbered(1001..1020), chatFrame(hasOlder = true))
        source.earlier.addAll(numbered(1..1000))
        val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

        // When a full page of held rows still sits above the visible run, skip fetch.
        assertNull(model.reachedTop(visibleFirstIndex = ChatModel.PAGE_ROWS))
        assertTrue(source.paged.isEmpty())

        val pageJob = model.reachedTop(visibleFirstIndex = 5)
        assertNotNull(pageJob)
        assertEquals(ChatPaging.Fetching, model.snapshot.paging)
        assertNull("in-flight page is not requested twice", model.reachedTop())
        pageJob!!.join()
        assertEquals(ChatPaging.Idle, model.snapshot.paging)
        assertEquals(listOf(ChatModel.PAGE_ROWS), source.paged)
        source.deliver(model)
        assertEquals("m961", model.keys.first())

        // Origin unreachable sets ChatPaging.Unreachable with clear message.
        source.pageOutcomeOverride = PageOutcome.OriginUnreachable
        model.reachedTop()!!.join()
        assertEquals(ChatPaging.Unreachable, model.snapshot.paging)

        // Collapsed open-below run asks for its step count clamped to LARGEST_PAGE.
        val bigRun = Run(
            first = "q",
            last = "r",
            steps = 300,
            live = false,
            openBelow = true,
            liveShown = false,
            unresolvedFailure = false,
            open = false,
            counts = RunCounts(0, 0, 300, 0, 0, 0, 0, 0),
        )
        val runSource = FakeChat(listOf(proseRow("r", 5, run = bigRun), proseRow("s", 6)), chatFrame(hasOlder = true))
        val runModel = ChatModel(source = runSource, scope = this, loadingHintAfterMs = -1L)
        runModel.cell("r")
        runModel.reachedTop()!!.join()
        assertEquals(listOf(300), runSource.paged)

        val hugeRun = bigRun.copy(steps = 5_000)
        runSource.ordered[0] = proseRow("r", 5, run = hugeRun)
        runSource.pending.keys.add("r")
        runSource.deliver(runModel)
        runModel.reachedTop()!!.join()
        assertEquals(ChatModel.LARGEST_PAGE, runSource.paged.last())
    }

    @Test
    fun draftPersistenceReadsOnOpenSavesInOrderAndStopsWhenAgentDeleted() {
        val source = FakeChat(listOf(proseRow("a", 1)), chatFrame())
        source.initialKeptDraft = draftOf("kept earlier")
        val model = ChatModel(source = source, scope = idle, loadingHintAfterMs = -1L)

        assertEquals("kept earlier", model.draft)
        assertTrue("reading keptDraft on open does not immediately echo a keep", source.keptDrafts.isEmpty())

        model.setDraft("kept earlier plus more")
        model.setDraft("final draft")
        assertEquals(listOf("kept earlier plus more", "final draft"), source.keptDrafts.map { it.words })

        // Agent deleted/ended via frame.ended stops further draft saves.
        source.current = source.current.copy(ended = "agent deleted")
        source.pending.session = true
        source.deliver(model)

        model.setDraft("typed while leaving deleted chat")
        assertEquals(2, source.keptDrafts.size)
    }

    @Test
    fun attachmentValidationRefusesOver64MbBeforeReadingOrUploading() = runTest {
        val source = FakeChat(emptyList(), chatFrame())
        val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

        var readCalled = false
        val overLimit = 64L * 1024L * 1024L + 1L
        val rejectedJob = model.attachCheckingSize(
            name = "huge.bin",
            size = overLimit,
            mime = "application/octet-stream",
            image = false,
        ) {
            readCalled = true
            ByteArray(1)
        }

        assertNull(rejectedJob)
        assertFalse("oversized file must be refused before reading bytes", readCalled)
        assertTrue("oversized file must not be uploaded", source.stored.isEmpty())
        assertTrue(model.attachments.isEmpty())
        val notice = model.snapshot.notice.orEmpty()
        assertTrue("notice names file: $notice", "huge.bin" in notice)
        assertTrue("notice names 64 MB limit: $notice", "64 MB" in notice)

        // Upload failure names file and reason.
        source.blobFailure = RuntimeFailure("desk stopped answering")
        val failedJob = model.attach(
            PickedAttachment(data = ByteArray(16), name = "mid.bin", mime = "application/octet-stream", image = false),
        )
        assertNotNull(failedJob)
        failedJob!!.join()
        assertTrue("mid.bin" in model.snapshot.notice.orEmpty())
        assertTrue("desk stopped answering" in model.snapshot.notice.orEmpty())

        // Successful upload adds chip and send puts U+FFFC placeholders in text.
        source.blobFailure = null
        val pngHeader = byteArrayOf(
            0x89.toByte(),
            0x50,
            0x4E,
            0x47,
            0x0D,
            0x0A,
            0x1A,
            0x0A,
        )
        model.attach(PickedAttachment.fromFile(pngHeader, "red.png"))!!.join()
        model.attach(PickedAttachment.fromFile("hello".encodeToByteArray(), "notes.txt"))!!.join()
        assertEquals(2, model.attachments.size)
        assertTrue(model.attachments[0] is DraftAttachment.Image)
        assertTrue(model.attachments[1] is DraftAttachment.File)

        model.type("Check these")
        model.send()!!.join()
        val sent = source.sent.single()
        assertEquals("Check these\uFFFC\uFFFC", sent.text)
        assertEquals(2, sent.attachments.size)
    }

    @Test
    fun pasteAsAttachmentExtractsLongPasteAndDeduplicatesFieldEcho() = runTest {
        val source = FakeChat(emptyList(), chatFrame())
        val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

        model.type("Look at this: ")
        val crlfLog = (1..12).joinToString("\r\n") { "line $it" } + "\r\n"
        model.type("Look at this: $crlfLog")

        assertEquals("Look at this: ", model.draft)
        val normalized = (1..12).joinToString("\n") { "line $it" } + "\n"
        assertEquals(listOf(DraftAttachment.Text(name = "Pasted text", text = normalized)), model.attachments)
        assertEquals(12, ChatModel.lines(normalized))

        // Echo of paste from text field does not create a second attachment.
        model.type("Look at this: $crlfLog")
        assertEquals(1, model.attachments.size)

        // Typing while field still echoes the paste keeps typed suffix cleanly.
        model.type("Look at this: ${crlfLog}now")
        assertEquals("Look at this: now", model.draft)
        assertEquals(1, model.attachments.size)

        // Short paste stays inline text; explicit long paste becomes attachment chip.
        model.paste("short\ntext")
        assertEquals("Look at this: nowshort\ntext", model.draft)
        assertEquals(1, model.attachments.size)

        model.paste("x".repeat(1_000))
        assertEquals(2, model.attachments.size)
    }

    @Test
    fun slashCommandAutocompleteAndClientCommandOutcomesWork() = runTest {
        val source = FakeChat(emptyList(), chatFrame())
        source.offered = offering(
            names = listOf(
                "clear",
                "compact",
                "context",
                "cost",
                "config-check",
                "code-review:review",
                "documents:documents",
                "init",
            ),
            skills = listOf("autopilot", "documents:documents"),
        )
        val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

        model.setDraft("/c")
        assertEquals(
            listOf("clear", "compact", "context", "cost", "config-check"),
            model.snapshot.slashMatches.map { it.command.name },
        )

        model.setDraft("/rev")
        assertEquals(listOf("code-review:review"), model.snapshot.slashMatches.map { it.command.name })

        model.setDraft("please use \$auto")
        assertEquals(listOf("autopilot"), model.snapshot.slashMatches.map { it.command.name })
        model.pick(model.snapshot.slashMatches.first())
        assertEquals("please use \$autopilot ", model.draft)
        assertTrue(model.snapshot.slashMatches.isEmpty())

        // Client command Pick opens settings and clears draft.
        source.commandOutcome = CommandOutcome.Pick(ClientCommand.Model)
        model.setDraft("/model")
        model.send()!!.join()
        assertEquals(ChatAsk.Settings(ClientCommand.Model), model.snapshot.asks)
        assertEquals("", model.draft)
        model.clearAsk()
        assertNull(model.snapshot.asks)

        // Client command AskName opens rename.
        source.commandOutcome = CommandOutcome.AskName
        model.setDraft("/rename")
        model.send()!!.join()
        assertEquals(ChatAsk.Rename, model.snapshot.asks)
        assertEquals("", model.draft)

        // Client command Refused with skill hint keeps draft and shows notice.
        source.current = chatFrame(kind = Kind.Codex)
        source.pending.session = true
        source.deliver(model)
        source.commandOutcome = CommandOutcome.Refused(command = "/lint", offered = listOf("compact"), hint = "\$lint")
        model.setDraft("/lint")
        model.send()!!.join()
        assertEquals("/lint", model.draft)
        assertEquals("Codex takes a skill as a mention: type \$lint.", model.snapshot.notice)

        // Client command Send forwards to agent as typed.
        source.commandOutcome = CommandOutcome.Send
        model.setDraft("/compact")
        model.send()!!.join()
        assertEquals(listOf("/compact"), source.sent.map { it.text })
    }

    @Test
    fun askCardAnswersAndUnsentQuestionAndFormStateSurviveUntilAskCloses() = runTest {
        val source = FakeChat(listOf(proseRow("a", 1)), chatFrame())
        val fields = listOf(
            FormField(
                name = "title",
                title = "Title",
                description = "",
                required = true,
                kind = FormFieldKind.Text(maxLength = 50, minLength = 3),
                initial = FormValue.Text(""),
            ),
        )
        source.card = AskCard(
            kind = Kind.ClaudeSdk,
            key = "ask:1",
            itemKey = "item:1",
            position = 1L,
            count = 1L,
            body = AskBody.Question(emptyList()),
            choices = emptyList(),
            questionNote = true,
            questionSkip = true,
            questionReply = true,
            stopsTurn = true,
            defaultToNo = false,
            state = CardState.Open,
        )
        val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

        // Permission/choice answer records choice + note.
        model.answer(choice = 1, note = "only read-only")!!.join()
        assertEquals(listOf(Triple("ask:1", 1, "only read-only")), source.answeredChoices)

        // Question draft preserves multi-select picks + "Something else" + note.
        val qDraft = QuestionDraft(
            step = 1,
            picks = listOf(setOf(0, 2), emptySet()),
            others = listOf("custom option", null),
            highlighted = listOf(2, null),
            skipped = listOf(false, true),
            notes = listOf("staging only", ""),
            noting = listOf(true, false),
        )
        model.keepQuestionDraft("ask:1", qDraft)
        model.keepQuestionDraft("ask:other", qDraft)
        assertEquals(qDraft, model.questionDraft("ask:1"))
        assertNull(model.questionDraft("ask:other"))

        val encoded = qDraft.toResponses()
        assertEquals(
            listOf(
                QuestionResponse(note = "staging only", other = "custom option", picked = listOf(0, 2)),
                QuestionResponse(note = null, other = null, picked = emptyList()),
            ),
            encoded,
        )
        model.answerQuestions(encoded)!!.join()
        assertEquals(listOf(encoded), source.responded)
        model.replyInstead("Hold off", encoded)!!.join()
        assertEquals(listOf("Hold off"), source.replied)

        // Form draft + live validation via Bridge.formProblems.
        val emptyValues = listOf(FormValue.Text(""))
        val problems = model.formProblems(fields, emptyValues)
        assertEquals(1, problems.size)
        assertEquals(FieldProblem.Required, problems.first().problem)

        val validValues = listOf(FormValue.Text("Fix login"))
        assertTrue(model.formProblems(fields, validValues).isEmpty())
        model.keepFormDraft("ask:1", validValues)
        assertEquals(validValues, model.formDraft("ask:1"))
        model.submitForm(validValues)!!.join()
        assertEquals(listOf(validValues), source.submittedForms)

        // When head ask changes to ask:2, unsent drafts for ask:1 clear permanently.
        source.card = source.card!!.copy(key = "ask:2")
        source.pending.session = true
        source.deliver(model)
        assertNull(model.questionDraft("ask:1"))
        assertNull(model.formDraft("ask:1"))
    }

    @Test
    fun subagentStandingAndGroupsRestampOnlyAffectedCells() {
        val group = SubagentGroup(first = "c1", agents = listOf(listOf(9), listOf(10)), open = false)
        val rows = listOf(
            proseRow("p1", 1, "hello"),
            subagentCallRow("c1", 2, opensAgent = 9, group = group),
            subagentCallRow("c2", 3, opensAgent = 10, group = group),
        )
        val source = FakeChat(rows, chatFrame())
        val initialStanding = GroupState(agents = 2, running = 2, needsYou = 0, idle = 0, finished = 0, ended = 0, failed = 0, unknown = 0)
        source.agents = ChatAgents(
            agents = listOf(
                OpenedAgent(AgentKey(listOf(1), listOf(9)), "sub-9", Kind.ClaudeSdk, PhaseView.Working),
                OpenedAgent(AgentKey(listOf(1), listOf(10)), "sub-10", Kind.ClaudeSdk, PhaseView.Working),
            ),
            groups = listOf(GroupView(first = "c1", state = initialStanding)),
            live = initialStanding,
        )
        val model = ChatModel(source = source, scope = idle, loadingHintAfterMs = -1L)

        val proseCell = model.cell("p1")
        val callCell = model.cell("c1")
        val beforeProseRev = proseCell.revision
        val beforeCallRev = callCell.revision
        assertEquals(AgentStanding.Running, AgentStanding.of(callCell.openedAgent, CallPhase.Succeeded))
        assertEquals(listOf("p1", "c1"), listOf("p1", "c1", "c2").filter { model.shows(model.cell(it).row!!) })

        // Fleet reports sub-9 now needs you -> only call/group cells restamp, prose cell untouched.
        val nextStanding = GroupState(agents = 2, running = 1, needsYou = 1, idle = 0, finished = 0, ended = 0, failed = 0, unknown = 0)
        source.agents = ChatAgents(
            agents = listOf(
                OpenedAgent(AgentKey(listOf(1), listOf(9)), "sub-9", Kind.ClaudeSdk, PhaseView.NeedsYou),
                OpenedAgent(AgentKey(listOf(1), listOf(10)), "sub-10", Kind.ClaudeSdk, PhaseView.Working),
            ),
            groups = listOf(GroupView(first = "c1", state = nextStanding)),
            live = nextStanding,
        )
        source.deliver(model)

        assertEquals(beforeProseRev, proseCell.revision)
        assertTrue(callCell.revision > beforeCallRev)
        assertEquals(AgentStanding.NeedsYou, AgentStanding.of(callCell.openedAgent, CallPhase.Succeeded))
        assertEquals("1 running · 1 waiting on you", ChatWords.subagentGroupState(callCell.groupState!!))
        assertEquals(AgentStanding.NeedsYou, ChatWords.subagentGroupStanding(callCell.groupState!!))
        assertEquals(nextStanding, model.snapshot.liveSubagents)

        // Toggling group expands all group calls.
        model.toggleGroup(callCell.row!!.group!!)
        source.deliver(model)
        assertEquals(listOf("p1", "c1", "c2"), listOf("p1", "c1", "c2").filter { model.shows(model.cell(it).row!!) })
    }

    @Test
    fun nativeStopSwitchBackWithdrawRefusedAndSettingsWork() = runTest {
        // Native subagent composer cannot send and stops its job by key.
        val nativeSource = FakeChat(
            emptyList(),
            chatFrame(mode = Composer.Native(stop = JobStop.Offered(key = "task-7")), phase = PhaseView.Working),
        )
        val nativeModel = ChatModel(source = nativeSource, scope = this, loadingHintAfterMs = -1L)
        nativeModel.setDraft("hello")
        assertFalse(nativeModel.canSend)
        assertFalse(nativeModel.canResume)
        assertEquals(JobStop.Offered("task-7"), nativeModel.currentFrame?.composer?.mode?.nativeStop)
        nativeModel.stopJob("task-7")!!.join()
        assertEquals(listOf("task-7"), nativeSource.stopped)

        // Switched-away banner & switchBack().
        val switchedSource = FakeChat(emptyList(), chatFrame(switchedAway = true))
        val switchedModel = ChatModel(source = switchedSource, scope = this, loadingHintAfterMs = -1L)
        assertTrue(switchedModel.currentFrame?.composer?.switchedAway == true)
        switchedModel.switchBack()!!.join()
        assertEquals(1, switchedSource.switchBacks)

        // Withdraw restores words + attachments; refused prompt auto-restores with reason and discards.
        val photo = BlobRef(hash = listOf(5), name = "shot.png", mime = "image/png", size = 100L)
        val refusingFrame = chatFrame().copy(
            refused = listOf(
                RefusedPrompt(
                    inputId = listOf(8),
                    reason = Refusal(reason = RefusalReason.HostUnreachable, detail = null),
                ),
            ),
        )
        val source = FakeChat(emptyList(), refusingFrame)
        source.drafts[listOf(8)] = draftOf("retry me", listOf(DraftAttachment.Image(photo)))
        source.drafts[listOf(7)] = draftOf("queued prompt")
        val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

        assertEquals("retry me", model.draft)
        assertEquals(listOf(DraftAttachment.Image(photo)), model.attachments)
        assertEquals("Not sent: its host is away", model.snapshot.notice)
        assertEquals(listOf(listOf(8)), source.discarded)
        assertTrue(model.currentFrame!!.refused.isEmpty())

        val queued = QueuedRow(
            inputId = listOf(7),
            text = listOf(Segment.Text("queued prompt")),
            steered = false,
            canWithdraw = true,
            canSendNow = true,
        )
        model.withdraw(queued)!!.join()
        assertEquals("queued prompt\nretry me", model.draft)
        model.sendNow(queued)!!.join()
        assertEquals(listOf(listOf(7)), source.sentNow)

        // Settings change sends SettingChange and waits for agent delta.
        model.change(SettingChange.Model("sonnet"))!!.join()
        assertEquals(listOf(SettingChange.Model("sonnet")), source.changed)
        model.interrupt()!!.join()
        assertEquals(1, source.interrupts)

        // Catching up disables send without dropping draft.
        source.current = chatFrame(mode = Composer.Waiting(Waiting.CatchingUp), caughtUp = false)
        source.pending.session = true
        source.deliver(model)
        assertFalse(model.canSend)
        assertNull(model.send())

        // Exited agent resumes with draft.
        source.current = chatFrame(mode = Composer.Resume, phase = PhaseView.Exited(ExitCause.Ended))
        source.pending.session = true
        source.deliver(model)
        assertTrue(model.canResume)
        model.clearDraft()
        model.setDraft("carry on")
        model.resume()!!.join()
        assertEquals(listOf("carry on"), source.resumed.map { it.text })
        assertEquals("", model.draft)
    }

    @Test
    fun blobCacheAttachmentPreviewOverviewAndLoadingHintWork() = runTest {
        val tempDir = Files.createTempDirectory("amux-chat-attachments").toFile()
        try {
            val shot = BlobRef(hash = listOf(5, 6), name = "../shots/screen.png", mime = "image/png", size = 4L)
            val imageRow = Row(
                id = "r1",
                order = 1L,
                atMs = 0L,
                kind = RowKind.Prose(
                    text = listOf(Segment.Attachment(AttachmentView.Image(shot))),
                    streaming = false,
                    workingNote = false,
                ),
                collapsed = false,
                attention = false,
            )
            val git = Git(
                baseBranch = "main",
                branch = "fix",
                uncommitted = ChangeTotals(files = 1, added = 2, removed = 0),
                onBranch = ChangeTotals(files = 2, added = 9, removed = 1),
            )
            val source = FakeChat(listOf(imageRow), chatFrame().copy(git = git))
            source.files = Changes(
                totals = ChangeTotals(1, 2, 0),
                folders = listOf(Folder("", listOf(ChangedFile("README.md", "README.md", 2, 0, FileStatus.Modified, false)))),
            )
            val model = ChatModel(
                source = source,
                scope = this,
                attachmentDir = tempDir,
                loadingHintAfterMs = -1L,
                ioDispatcher = UnconfinedTestDispatcher(testScheduler),
            )

            // Failed attachment fetch retries on openAttachment.
            source.failedFetches.add(shot.hash)
            model.openAttachment(shot)
            assertEquals(listOf(shot.hash), source.fetchedAgain)
            assertNull(model.snapshot.openedAttachment)

            // Landing bytes + unchanged row re-delivery bumps attachmentRevision and opens file safely.
            val cell = model.cell("r1")
            val beforeAttRev = cell.attachmentRevision
            source.failedFetches.clear()
            source.landed[shot.hash] = "png!".encodeToByteArray()
            model.openAttachment(shot)
            runCurrent()
            val opened = model.snapshot.openedAttachment
            assertNotNull(opened)
            assertEquals("screen.png", opened!!.name)
            assertEquals("png!", opened.readText())
            model.apply(ChatDelta(chat = 0L, rows = listOf(imageRow)))
            assertEquals(beforeAttRev + 1, cell.attachmentRevision)

            // Overview changed files fetch when shown and on comparison switch.
            model.showOverview(true)
            model.listingJob?.join()
            assertEquals(listOf(Comparison.Uncommitted), source.listings)
            assertEquals(source.files, model.currentOverview?.changes)

            model.compare(Comparison.OnBranch)
            model.listingJob?.join()
            assertEquals(listOf(Comparison.Uncommitted, Comparison.OnBranch), source.listings)

            val explicit = model.openOverview(Comparison.OnBranch).await()
            assertTrue(explicit.isSuccess)
        } finally {
            tempDir.deleteRecursively()
        }
    }

    @Test
    fun emptyChatShowsLoadingHintAfterDelayAndClearsOnArrival() = runTest(StandardTestDispatcher()) {
        val source = FakeChat(emptyList(), chatFrame(caughtUp = false))
        val model = ChatModel(source = source, scope = this)

        assertFalse(model.snapshot.loadingHint)
        advanceTimeBy(ChatModel.LOADING_HINT_DELAY_MS)
        runCurrent()
        assertTrue(model.snapshot.loadingHint)

        source.arrive(listOf(proseRow("a", 1)))
        source.deliver(model)
        assertFalse(model.snapshot.loadingHint)
        model.close()
    }

    @Test
    fun askAnswersRejectStaleExpectedKeyAndDispatchMatchingKey() = runTest {
        val source = FakeChat(listOf(proseRow("a", 1)), chatFrame())
        source.card = AskCard(
            kind = Kind.ClaudeSdk,
            key = "ask:new",
            itemKey = "item:2",
            position = 1L,
            count = 1L,
            body = AskBody.Question(emptyList()),
            choices = emptyList(),
            questionNote = true,
            questionSkip = true,
            questionReply = true,
            stopsTurn = true,
            defaultToNo = false,
            state = CardState.Open,
        )
        val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

        // Stale ask key must be rejected silently without dispatching to source.
        assertNull(model.answer(choice = 0, note = "old", expectedKey = "ask:old"))
        assertNull(model.answerQuestions(emptyList(), expectedKey = "ask:old"))
        assertNull(model.replyInstead("old", emptyList(), expectedKey = "ask:old"))
        assertNull(model.submitForm(emptyList(), expectedKey = "ask:old"))
        assertTrue(source.answeredChoices.isEmpty())
        assertTrue(source.responded.isEmpty())
        assertTrue(source.replied.isEmpty())
        assertTrue(source.submittedForms.isEmpty())

        // Matching ask key dispatches cleanly.
        model.answer(choice = 0, note = "ok", expectedKey = "ask:new")!!.join()
        assertEquals(listOf(Triple("ask:new", 0, "ok")), source.answeredChoices)
    }

    @Test
    fun openingRefusedPromptPersistsCombinedDraftBeforeDiscarding() {
        val refusingFrame = chatFrame().copy(
            refused = listOf(
                RefusedPrompt(
                    inputId = listOf(42),
                    reason = Refusal(reason = RefusalReason.HostUnreachable, detail = null),
                ),
            ),
        )
        val source = FakeChat(emptyList(), refusingFrame)
        source.initialKeptDraft = draftOf("already typed")
        source.drafts[listOf(42)] = draftOf("refused prompt")

        val model = ChatModel(source = source, scope = idle, loadingHintAfterMs = -1L)

        assertEquals("refused prompt\nalready typed", model.draft)
        assertEquals(listOf("keep:refused prompt\nalready typed", "discard"), source.draftEvents)
        assertEquals(1, source.keptDrafts.size)
        assertEquals(listOf(listOf(42)), source.discarded)
    }

    @Test
    fun unarrivedKeyLookupDoesNotCacheNullRowAndPopulatesOnEdgeArrival() {
        val source = FakeChat(listOf(proseRow("a", 1)), chatFrame())
        val model = ChatModel(source = source, scope = idle, loadingHintAfterMs = -1L)

        // Look up a key before it arrives in the held window (e.g. ActivityKind.Running).
        assertNull(model.rowOrNull("cmd-late"))
        assertNull(model.cell("cmd-late").row)

        // Key arrives as a new edge key without row payload in delta.rows.
        source.ordered.add(proseRow("cmd-late", 2, "running cargo test"))
        source.pending.keys.add("cmd-late")
        source.deliver(model)

        // First draw reads the arrived row instead of reusing a cached null cell.
        val cell = model.cell("cmd-late")
        assertEquals("cmd-late", cell.row?.id)
        assertEquals(proseRow("cmd-late", 2, "running cargo test").kind, cell.row?.kind)
    }

    @Test
    fun middleOfSentencePastePreservesPositionOnSendEditAndReopen() = runTest {
        val source = FakeChat(emptyList(), chatFrame())
        val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

        model.type("Start  end")
        val bigPaste = (1..10).joinToString("\n") { "line $it" }
        model.type("Start ${bigPaste} end")

        assertEquals("Start  end", model.draft)
        assertEquals(1, model.attachments.size)
        val kept = source.keptDrafts.last()
        assertEquals("Start \uFFFC end", kept.text)
        assertEquals("Start  end", kept.words)

        // Editing before the paste shifts its offset so the placeholder stays in place.
        model.type("Hey! Start  end")
        assertEquals("Hey! Start \uFFFC end", source.keptDrafts.last().text)

        // Reopening the persisted draft in a fresh ChatModel preserves the position.
        val reopenedSource = FakeChat(emptyList(), chatFrame())
        reopenedSource.initialKeptDraft = source.keptDrafts.last()
        val reopened = ChatModel(source = reopenedSource, scope = this, loadingHintAfterMs = -1L)
        assertEquals("Hey! Start  end", reopened.draft)
        reopened.send()!!.join()
        assertEquals("Hey! Start \uFFFC end", reopenedSource.sent.single().text)
    }

    @Test
    fun overviewListingCancelsPriorJobTracksComparisonAndRetriesAfterFailure() =
        runTest(StandardTestDispatcher()) {
            val git = Git(
                baseBranch = "main",
                branch = "fix",
                uncommitted = ChangeTotals(files = 1, added = 2, removed = 0),
                onBranch = ChangeTotals(files = 2, added = 9, removed = 1),
            )
            val source = FakeChat(emptyList(), chatFrame().copy(git = git))
            source.files = Changes(
                totals = ChangeTotals(1, 2, 0),
                folders = listOf(Folder("", listOf(ChangedFile("a.kt", "a.kt", 2, 0, FileStatus.Modified, false)))),
            )
            source.overviewGate = kotlinx.coroutines.CompletableDeferred()
            val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

            model.showOverview(true)
            val firstJob = model.listingJob
            assertNotNull(firstJob)
            assertTrue(model.snapshot.overviewChangesLoading)

            // Switching comparison cancels the prior in-flight listing job immediately.
            model.compare(Comparison.OnBranch)
            assertTrue(firstJob!!.isCancelled)
            assertEquals(Comparison.OnBranch, model.snapshot.comparison)
            assertNull(model.snapshot.overviewChangesComparison)

            // Simulate failure on OnBranch -> error exposed and listed cleared for retry.
            source.overviewFailure = RuntimeFailure("host away")
            source.overviewGate!!.complete(Unit)
            runCurrent()
            assertFalse(model.snapshot.overviewChangesLoading)
            assertEquals("host away", model.snapshot.overviewChangesError)
            assertNull(model.snapshot.overviewChangesComparison)

            // Retrying succeeds and binds overviewChangesComparison to OnBranch.
            source.overviewFailure = null
            source.overviewGate = null
            model.retryChangedFiles()
            runCurrent()
            assertFalse(model.snapshot.overviewChangesLoading)
            assertNull(model.snapshot.overviewChangesError)
            assertEquals(Comparison.OnBranch, model.snapshot.overviewChangesComparison)
            assertEquals(source.files, model.currentOverview?.changes)
        }

    @Test
    fun blobCacheEvictsLeastRecentlyUsedWithinByteBudget() {
        val source = FakeChat(emptyList(), chatFrame())
        source.landed[listOf(1)] = ByteArray(6) { 1 }
        source.landed[listOf(2)] = ByteArray(4) { 2 }
        source.landed[listOf(3)] = ByteArray(5) { 3 }
        source.landed[listOf(4)] = ByteArray(12) { 4 }
        val model = ChatModel(
            source = source,
            scope = idle,
            loadingHintAfterMs = -1L,
            blobCacheMaxBytes = 10L,
        )

        assertNotNull(model.blob(listOf(1)))
        assertNotNull(model.blob(listOf(2)))
        assertEquals(10L, model.blobCacheBytes)

        // Touch key 1 so key 2 becomes least recently used.
        assertNotNull(model.blob(listOf(1)))
        // Adding key 3 (5B) evicts key 2 (4B), leaving 1 (6B) + 3 (5B) -> also evicts 1 -> 5B.
        assertNotNull(model.blob(listOf(3)))
        assertTrue(model.blobCacheBytes <= 10L)

        // Oversized blob (> budget) is returned without exceeding budget.
        assertNotNull(model.blob(listOf(4)))
        assertTrue(model.blobCacheBytes <= 10L)
    }

    @Test
    fun runCommandSendStillDispatchesOriginalTypedDraftIfDraftChanged() = runTest {
        val source = FakeChat(emptyList(), chatFrame())
        source.commandOutcome = CommandOutcome.Send
        val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

        model.setDraft("/compact")
        val job = model.send()!!
        // User begins typing a new draft before command coroutine finishes.
        model.setDraft("next prompt")
        job.join()

        assertEquals(listOf("/compact"), source.sent.map { it.text })
        assertEquals("next prompt", model.draft)
    }

    @Test
    fun nativeImageSnifferDetectsMagicBytesAndRejectsMisnamedFiles() {
        val png = byteArrayOf(0x89.toByte(), 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00)
        val jpeg = byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0xFF.toByte(), 0xE0.toByte())
        val gif87 = "GIF87a...".encodeToByteArray()
        val gif89 = "GIF89a...".encodeToByteArray()
        val webp = "RIFF1234WEBP....".encodeToByteArray()
        val heic = byteArrayOf(0, 0, 0, 24) + "ftypheic".encodeToByteArray()
        val fakePng = "not actually a png".encodeToByteArray()

        assertEquals("image/png", PickedAttachment.sniffNativeImageMime(png))
        assertEquals("image/jpeg", PickedAttachment.sniffNativeImageMime(jpeg))
        assertEquals("image/gif", PickedAttachment.sniffNativeImageMime(gif87))
        assertEquals("image/gif", PickedAttachment.sniffNativeImageMime(gif89))
        assertEquals("image/webp", PickedAttachment.sniffNativeImageMime(webp))
        assertNull(PickedAttachment.sniffNativeImageMime(heic))
        assertNull(PickedAttachment.sniffNativeImageMime(fakePng))

        val misnamed = PickedAttachment.fromFile(fakePng, "fake.png", "image/png")
        assertFalse("misnamed PNG must be classified as non-image file", misnamed.image)

        val realJpeg = PickedAttachment.fromFile(jpeg, "photo.bin", "application/octet-stream")
        assertTrue(realJpeg.image)
        assertEquals("image/jpeg", realJpeg.mime)
    }

    @Test
    fun attachmentPreparationBlocksSendAndResumeUntilDoneFailureOrCancellation() =
        runTest(StandardTestDispatcher()) {
            val source = FakeChat(emptyList(), chatFrame())
            val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)
            model.setDraft("ship this with attachment")
            assertTrue(model.canSend)

            // Normal completion keeps canSend == false while active and restores it after.
            val gate = CompletableDeferred<Unit>()
            val prepJob = launch {
                model.withAttachmentPreparation {
                    assertFalse(model.canSend)
                    assertEquals(1, model.snapshot.uploading)
                    assertNull(model.send())
                    gate.await()
                }
            }
            runCurrent()
            assertFalse(model.canSend)
            assertEquals(1, model.snapshot.uploading)
            gate.complete(Unit)
            prepJob.join()
            assertTrue(model.canSend)
            assertEquals(0, model.snapshot.uploading)

            // Failure releases the preparation count.
            runCatching {
                model.withAttachmentPreparation {
                    assertFalse(model.canSend)
                    throw IllegalStateException("disk error")
                }
            }
            assertTrue(model.canSend)
            assertEquals(0, model.snapshot.uploading)

            // Cancellation releases the preparation count and restores canResume in Resume mode.
            source.current = chatFrame(mode = Composer.Resume, phase = PhaseView.Exited(ExitCause.Ended))
            source.pending.session = true
            source.deliver(model)
            assertTrue(model.canResume)

            val cancelGate = CompletableDeferred<Unit>()
            val cancelJob = launch {
                model.withAttachmentPreparation {
                    cancelGate.await()
                }
            }
            runCurrent()
            assertFalse(model.canResume)
            assertEquals(1, model.snapshot.uploading)
            cancelJob.cancel()
            cancelJob.join()
            assertTrue(model.canResume)
            assertEquals(0, model.snapshot.uploading)
        }

    @Test
    fun outOfOrderUncommittedDeltaDoesNotOverwriteOnBranchOverviewState() =
        runTest(StandardTestDispatcher()) {
            val git = Git(
                baseBranch = "main",
                branch = "fix",
                uncommitted = ChangeTotals(files = 1, added = 2, removed = 0),
                onBranch = ChangeTotals(files = 2, added = 9, removed = 1),
            )
            val uncommittedChanges = Changes(
                totals = ChangeTotals(1, 2, 0),
                folders = listOf(Folder("", listOf(ChangedFile("dirty.kt", "dirty.kt", 2, 0, FileStatus.Modified, false)))),
            )
            val onBranchChanges = Changes(
                totals = ChangeTotals(2, 9, 1),
                folders = listOf(Folder("", listOf(ChangedFile("branch.kt", "branch.kt", 9, 1, FileStatus.Modified, false)))),
            )
            val source = FakeChat(emptyList(), chatFrame().copy(git = git))
            source.files = uncommittedChanges
            source.overviewGate = CompletableDeferred()
            val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

            model.showOverview(true)
            assertTrue(model.snapshot.overviewChangesLoading)

            // Switch to OnBranch while Uncommitted is in flight.
            model.compare(Comparison.OnBranch)
            assertTrue(model.snapshot.overviewChangesLoading)
            assertNull(model.snapshot.overviewChangesComparison)

            // Late untagged delta from the cancelled Uncommitted request arrives while OnBranch is loading.
            model.apply(
                ChatDelta(
                    chat = 0L,
                    rows = emptyList(),
                    overview = Overview(jobs = emptyList(), failedServers = emptyList(), changes = uncommittedChanges),
                ),
            )
            assertTrue(model.snapshot.overviewChangesLoading)
            assertNull(model.snapshot.overviewChangesComparison)
            assertNull(model.currentOverview?.changes)

            // OnBranch fails; another stale Uncommitted delta must not clear the error or masquerade as OnBranch.
            source.overviewFailure = RuntimeFailure("host away")
            source.overviewGate!!.complete(Unit)
            runCurrent()
            assertFalse(model.snapshot.overviewChangesLoading)
            assertEquals("host away", model.snapshot.overviewChangesError)

            model.apply(
                ChatDelta(
                    chat = 0L,
                    rows = emptyList(),
                    overview = Overview(jobs = emptyList(), failedServers = emptyList(), changes = uncommittedChanges),
                ),
            )
            assertEquals("host away", model.snapshot.overviewChangesError)
            assertNull(model.snapshot.overviewChangesComparison)
            assertNull(model.currentOverview?.changes)

            // Retry succeeds with OnBranch changes; a subsequent stale delta keeps OnBranch intact.
            source.overviewFailure = null
            source.overviewGate = null
            source.files = onBranchChanges
            model.retryChangedFiles()
            runCurrent()
            assertEquals(Comparison.OnBranch, model.snapshot.overviewChangesComparison)
            assertEquals(onBranchChanges, model.currentOverview?.changes)

            model.apply(
                ChatDelta(
                    chat = 0L,
                    rows = emptyList(),
                    overview = Overview(jobs = emptyList(), failedServers = emptyList(), changes = uncommittedChanges),
                ),
            )
            assertEquals(Comparison.OnBranch, model.snapshot.overviewChangesComparison)
            assertEquals(onBranchChanges, model.currentOverview?.changes)
        }

    @Test
    fun restoringRefusedPromptWithTrailingFileAheadOfNewerDraftPreservesOrder() = runTest {
        val source = FakeChat(emptyList(), chatFrame())
        val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

        // Newer draft contains text with an inline paste B in the middle.
        model.type("Newer  end")
        val pasteText = (1..10).joinToString("\n") { "pasted line $it" }
        model.type("Newer ${pasteText} end")
        val pasteB = model.attachments.single()

        // A refused prompt ended with trailing file A.
        val fileA = DraftAttachment.File(
            BlobRef(hash = listOf(1, 2, 3), name = "report.pdf", mime = "application/pdf", size = 128L),
        )
        source.drafts[listOf(99)] = Draft(
            parts = listOf(
                DraftPart.Text("First"),
                DraftPart.Attachment(fileA),
            ),
        )
        model.apply(
            ChatDelta(
                chat = 0L,
                rows = emptyList(),
                frame = chatFrame().copy(
                    refused = listOf(
                        RefusedPrompt(
                            inputId = listOf(99),
                            reason = Refusal(reason = RefusalReason.HostUnreachable, detail = null),
                        ),
                    ),
                ),
            ),
        )

        assertEquals("First\nNewer  end", model.draft)
        assertEquals(listOf(fileA, pasteB), model.attachments)

        model.send()!!.join()
        val sent = source.sent.single()
        assertEquals(
            listOf(
                DraftPart.Text("First"),
                DraftPart.Attachment(fileA),
                DraftPart.Text("\nNewer "),
                DraftPart.Attachment(pasteB),
                DraftPart.Text(" end"),
            ),
            sent.parts,
        )
    }

    @Test
    fun questionDraftToResponsesPreservesNonblankOtherVerbatimAndDropsBlank() {
        val draft = QuestionDraft(
            picks = listOf(emptySet(), emptySet()),
            others = listOf("  secret  ", "   "),
            highlighted = listOf(null, null),
        )
        val responses = draft.toResponses()
        assertEquals("  secret  ", responses[0].other)
        assertNull(responses[1].other)
    }

    @Test
    fun anActCancelledByClosingTheChatSetsNoNotice() = runTest {
        val source = FakeChat(emptyList(), chatFrame())
        val answer = CompletableDeferred<ActOutcome?>()
        source.interruptAnswer = answer
        val model = ChatModel(source = source, scope = this, loadingHintAfterMs = -1L)

        val interrupting = model.interrupt()!!
        runCurrent()
        assertEquals(1, source.interrupts)

        // Closing the chat cancels the act; a closed chat's answer must not
        // read as "This chat is closed." over whatever the person sees next.
        model.close()
        answer.complete(null)
        runCurrent()
        assertTrue(interrupting.isCancelled)
        assertNull(model.snapshot.notice)
    }
}
