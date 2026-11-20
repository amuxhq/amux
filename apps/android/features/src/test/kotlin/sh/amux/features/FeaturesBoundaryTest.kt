package sh.amux.features

import org.junit.Assert.assertEquals
import org.junit.Test
import sh.amux.words.Words

class FeaturesBoundaryTest {
    @Test
    fun featuresCanUsePureJvmWordsModule() {
        assertEquals("a · b", Words.joinFacts("a", "b"))
    }
}
