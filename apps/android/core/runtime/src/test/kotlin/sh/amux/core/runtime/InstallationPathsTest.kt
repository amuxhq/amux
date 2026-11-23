package sh.amux.core.runtime

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Test

class InstallationPathsTest {
    @Test
    fun rootDirLivesUnderAmuxInstallation() {
        val base = File("/data/user/0/sh.amux.android/files")
        assertEquals(
            File(base, "amux/installation"),
            InstallationPaths.rootDir(base),
        )
    }
}
