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
