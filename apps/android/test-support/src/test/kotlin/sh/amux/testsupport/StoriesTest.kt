package sh.amux.testsupport

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class StoriesTest {
    @Test
    fun registryContainsExpectedUniqueStories() {
        val ids = Stories.all.map { it.id }
        assertEquals("Story IDs must be unique", ids.size, ids.toSet().size)
        val required = listOf(
            "fleet.busy",
            "fleet.no-agents",
            "fleet.first-run",
            "fleet.first-run-two-found",
            "fleet.first-run-window-open",
            "fleet.first-run-looking",
            "fleet.first-run-signed-out",
            "fleet.filter",
            "fleet.no-matches",
            "fleet.large-text",
            "fleet.two-accounts",
            "chat.active",
            "chat.streaming",
            "chat.native-subagent",
            "chat.exited",
            "chat.offline-draft",
            "chat.overview",
            "chat.settings",
            "chat.loading",
            "chat.queued",
            "chat.signed-out",
            "chat.large-text",
            "chat.ask-command",
            "chat.ask-edit",
            "chat.ask-question",
            "chat.ask-plan",
            "chat.ask-form",
            "ask.question-large-text",
            "review.diff",
            "review.no-changes",
            "hosts.paired",
            "hosts.empty",
            "hosts.nearby-found",
            "pairing.pin-entering",
            "pairing.confirm",
            "pairing.refused",
            "store.failure",
        )
        for (id in required) {
            assertTrue("Missing required story: $id", id in ids)
        }
        val byId = Stories.all.associateBy { it.id }
        assertTrue("chat.large-text must use >= 1.5x font scale", byId.getValue("chat.large-text").fontScale >= 1.5f)
        assertTrue(
            "ask.question-large-text must use >= 1.5x font scale",
            byId.getValue("ask.question-large-text").fontScale >= 1.5f,
        )
    }

    @Test
    fun matchingSupportsPrefixAndExactFilters() {
        assertEquals(Stories.all.size, Stories.matching(null).size)
        assertEquals(Stories.all.size, Stories.matching("").size)
        assertEquals(listOf("fleet.busy"), Stories.matching("fleet.busy").map { it.id })
        assertEquals(
            listOf("hosts.paired", "hosts.empty", "hosts.nearby-found"),
            Stories.matching("hosts.").map { it.id },
        )
    }
}
