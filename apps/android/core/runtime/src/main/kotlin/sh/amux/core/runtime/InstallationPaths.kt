package sh.amux.core.runtime

import java.io.File

/**
 * Resolves the installation directory under app-private storage, matching the
 * manifest backup-exclusion path.
 */
object InstallationPaths {
    const val RELATIVE_PATH: String = "amux/installation"

    fun rootDir(filesDir: File): File = File(filesDir, RELATIVE_PATH)
}
