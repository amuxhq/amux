package sh.amux.testsupport

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.core.runtime.ChatBridge
import sh.amux.core.runtime.ChatModel
import sh.amux.core.runtime.ChatPaging
import sh.amux.core.runtime.text
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.CallPhase
import sh.amux.values.CardState
import sh.amux.values.CommandMatch
import sh.amux.values.CommandView
import sh.amux.values.Comparison
import sh.amux.values.ExploreVerb
import sh.amux.values.FormField
import sh.amux.values.FormProblem
import sh.amux.values.FormValue
import sh.amux.values.Kind
import sh.amux.values.PageOutcome
import sh.amux.values.RowKind
import sh.amux.values.Segment
import sh.amux.values.SettingChange

class ScriptedChatTest {
    private val noJniBridge = object : ChatBridge {
        override val attachmentLimit: Long = 64L * 1024L * 1024L
        override fun commandMatches(commands: List<CommandView>, draft: String): List<CommandMatch> = emptyList()
        override fun pastesAsAttachment(text: String): Boolean = text.lines().size >= 8
        override fun formProblems(fields: List<FormField>, values: List<FormValue>): List<FormProblem> = emptyList()
    }

    @Test
    fun scriptedChatDrivesChatModelWithoutNativeRuntime() = runBlocking {
        val runRows = ScriptedChat.run(
            listOf(
                ScriptedChat.row("s1", 1, RowKind.Explore(ExploreVerb.Read, "A.kt", CallPhase.Succeeded, "")),
                ScriptedChat.row("s2", 2, RowKind.Explore(ExploreVerb.Read, "B.kt", CallPhase.Succeeded, "")),
            ),
        )
        val scripted = ScriptedChat(
            rows = runRows,
            frame = ScriptedChat.frame(hasOlder = true),
            card = AskCard(
                kind = Kind.ClaudeSdk,
                key = "ask:1",
                itemKey = "s2",
                position = 1L,
                count = 1L,
                body = AskBody.Command("rm -rf /tmp/x", "/repo", "clean up", "Remove temp dir"),
                choices = emptyList(),
                questionNote = false,
                questionSkip = false,
                questionReply = false,
                stopsTurn = true,
                defaultToNo = true,
                state = CardState.Open,
            ),
        )
        val model = ChatModel(
            source = scripted,
            scope = this,
            bridge = noJniBridge,
            loadingHintAfterMs = -1L,
        )

        // Folded run starts collapsed on s1 and shown on s2; toggling opens s1.
        assertTrue(model.cell("s1").row!!.collapsed)
        assertFalse(model.cell("s2").row!!.collapsed)
        model.toggleRun(model.cell("s2").row!!.run!!)
        scripted.deliver(model)
        assertFalse(model.cell("s1").row!!.collapsed)

        // Held arrivals while reading in history release on jumpToNewest.
        model.reading(atNewest = false)
        scripted.append(
            listOf(
                ScriptedChat.row("s3", 3, RowKind.Prose(listOf(Segment.Text("done")), false, false)),
            ),
        )
        scripted.deliver(model)
        assertTrue(model.newActivity)
        assertEquals(listOf("s1", "s2"), model.keys)

        model.jumpToNewest()
        scripted.deliver(model)
        assertEquals(listOf("s1", "s2", "s3"), model.keys)
        assertFalse(model.newActivity)

        // Held page waits deterministically for answerPage().
        scripted.holdsPage = true
        scripted.pagedOutcome = PageOutcome.OriginUnreachable
        val pageJob = model.reachedTop()
        assertNotNull(pageJob)
        assertEquals(ChatPaging.Fetching, model.snapshot.paging)
        scripted.answerPage()
        pageJob!!.join()
        assertEquals(ChatPaging.Unreachable, model.snapshot.paging)

        // Acts record cleanly.
        model.setDraft("hello")
        model.send()!!.join()
        assertEquals(listOf("hello"), scripted.sent.map { it.text })

        model.answer(choice = 0)!!.join()
        assertEquals(1, scripted.answers.size)

        model.change(SettingChange.Effort("high"))!!.join()
        assertEquals(listOf(SettingChange.Effort("high")), scripted.changed)

        model.openOverview(Comparison.Uncommitted).await()
        scripted.close()
        assertTrue(scripted.closed)
    }
}
