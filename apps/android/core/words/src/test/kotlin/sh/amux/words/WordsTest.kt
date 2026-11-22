package sh.amux.words

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test
import java.time.Duration

class WordsTest {
    @Test
    fun joinFactsUsesMiddleDotAndSkipsBlank() {
        assertEquals("studio · 2m", Words.joinFacts("studio", "", "2m"))
    }

    @Test
    fun anOpenableLinkIsAWebPageOrAnEmailAddress() {
        assertNotNull(OpenableLink.parse("HTTPS://example.com"))
        assertNotNull(OpenableLink.parse("mailto:ada@example.com"))
        assertNull(OpenableLink.parse("amux://callback?code=abc"))
        assertNull(OpenableLink.parse("shortcuts://run-shortcut?name=x"))
        assertNull(OpenableLink.parse("javascript:alert(1)"))
        assertNull(OpenableLink.parse("example.com"))
        assertNull(OpenableLink.parse(""))
    }

    @Test
    fun elapsedSpellsSecondsMinutesHoursAndDays() {
        assertEquals("0s", Elapsed.spelled(-3.0))
        assertEquals("0s", Elapsed.spelled(0.4))
        assertEquals("59s", Elapsed.spelled(59.9))
        assertEquals("1m", Elapsed.spelled(60.0))
        assertEquals("59m", Elapsed.spelled(3_599.0))
        assertEquals("1h", Elapsed.spelled(3_600.0))
        assertEquals("23h", Elapsed.spelled(86_399.0))
        assertEquals("1d", Elapsed.spelled(86_400.0))
        assertEquals("3d", Elapsed.spelled(Duration.ofDays(3)))
    }
}
