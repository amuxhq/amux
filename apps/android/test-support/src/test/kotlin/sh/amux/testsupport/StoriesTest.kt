package sh.amux.testsupport

import java.io.File
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class StoriesTest {
    @Serializable
    private data class StoriesCatalog(
        val stories: List<CatalogStory>,
    )

    @Serializable
    private data class CatalogStory(
        val name: String,
        val clients: List<String> = emptyList(),
    )

    private val json = Json { ignoreUnknownKeys = true }

    @Test
    fun registryCoversEveryPhoneStoryInStoriesJson() {
        val catalogFile = findStoriesJson()
        val catalog = json.decodeFromString<StoriesCatalog>(catalogFile.readText(Charsets.UTF_8))
        val phoneNames = catalog.stories
            .filter { "phone" in it.clients }
            .map { it.name }
            .toSet()
        assertTrue("Expected phone stories in tests/stories.json", phoneNames.isNotEmpty())

        val registeredIds = Stories.all.map { it.id }
        assertEquals("Story IDs must be unique", registeredIds.size, registeredIds.toSet().size)
        val registeredSet = registeredIds.toSet()

        val exclusions = Stories.IOS_ONLY_STORIES.keys
        val overlap = registeredSet.intersect(exclusions)
        assertTrue("Stories cannot be both registered and iOS-only excluded: $overlap", overlap.isEmpty())
        val unknownExclusions = exclusions - phoneNames
        assertTrue("iOS-only exclusions must refer to real phone stories: $unknownExclusions", unknownExclusions.isEmpty())

        val missing = (phoneNames - registeredSet - exclusions).sorted()
        assertTrue(
            "Android storybook is missing phone stories from tests/stories.json: $missing",
            missing.isEmpty(),
        )
    }

    @Test
    fun registryIncludesSettledAndLargeTextScales() {
        val byId = Stories.all.associateBy { it.id }
        assertTrue("Missing fleet.settled story", "fleet.settled" in byId)
        assertTrue("Missing store.failure story", "store.failure" in byId)

        val largeTextStories = Stories.all.filter { it.id.endsWith("-large-text") || it.id.endsWith(".large-text") }
        assertTrue("Expected large-text stories", largeTextStories.size >= 5)
        for (story in largeTextStories) {
            assertTrue(
                "${story.id} must use >= 1.5x font scale",
                story.fontScale >= 1.5f,
            )
        }
    }

    @Test
    fun matchingSupportsPrefixAndExactFilters() {
        assertEquals(Stories.all.size, Stories.matching(null).size)
        assertEquals(Stories.all.size, Stories.matching("").size)
        assertEquals(listOf("fleet.busy"), Stories.matching("fleet.busy").map { it.id })
        assertEquals(
            listOf(
                "hosts.list",
                "hosts.empty",
                "hosts.on-the-account-free",
                "hosts.nearby-found",
                "hosts.large-text",
            ),
            Stories.matching("hosts.").map { it.id },
        )
    }

    private fun findStoriesJson(): File {
        val candidates = listOf(
            File("../../../tests/stories.json"),
            File("../../tests/stories.json"),
            File("tests/stories.json"),
        )
        return candidates.firstOrNull { it.isFile }?.canonicalFile
            ?: error("Could not locate tests/stories.json from ${File(".").canonicalPath}")
    }
}
