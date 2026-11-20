package sh.amux.design

import org.junit.Assert.assertNotNull
import org.junit.Test

class AmuxFontsTest {
    @Test
    fun bundledFontsAreReferenced() {
        assertNotNull(AmuxFonts.Sans)
        assertNotNull(AmuxFonts.Mono)
    }
}
