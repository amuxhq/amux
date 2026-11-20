package sh.amux.shell

import org.junit.Assert.assertEquals
import org.junit.Test
import sh.amux.core.runtime.InstallationPaths

class ShellBoundaryTest {
    @Test
    fun shellCanAccessRuntimePaths() {
        assertEquals("amux/installation", InstallationPaths.RELATIVE_PATH)
    }
}
