package sh.amux.testsupport

import org.junit.Assert.assertTrue
import org.junit.Test

class TestSupportTest {
    @Test
    fun testSupportIsEnabled() {
        assertTrue(TestSupport.enabled)
    }
}
