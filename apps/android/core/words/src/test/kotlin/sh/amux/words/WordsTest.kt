package sh.amux.words

import org.junit.Assert.assertEquals
import org.junit.Test

class WordsTest {
    @Test
    fun joinFactsUsesMiddleDotAndSkipsBlank() {
        assertEquals("studio · 2m", Words.joinFacts("studio", "", "2m"))
    }
}
