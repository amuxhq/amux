package sh.amux.shell

import android.app.Activity
import android.content.Intent
import android.net.Uri
import java.io.File
import java.util.zip.ZipFile
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class ShellPlatformExportTest {
    @Test
    fun prepareShareableExportZipsDirectoryAndPrunesOnlyAgedZips() {
        runBlocking {
            val context = RuntimeEnvironment.getApplication()
            val exportsDir = File(context.cacheDir, "exports").apply {
                deleteRecursively()
                mkdirs()
            }
            val nowMs = 1_700_000_000_000L
            val agedZip = File(exportsDir, "old-dump.zip").apply {
                writeText("stale")
                setLastModified(nowMs - ShellPlatform.EXPORT_RETENTION_MS - 60_000L)
            }
            val recentZip = File(exportsDir, "recent-dump.zip").apply {
                writeText("still-being-read")
                setLastModified(nowMs - 60_000L)
            }
            assertTrue(agedZip.exists())
            assertTrue(recentZip.exists())

            val sourceDir = File(context.cacheDir, "raw-dump-bundle").apply {
                deleteRecursively()
                mkdirs()
                File(this, "report.json").writeText("""{"kind":"dump"}""")
                File(this, "sub/log.txt").apply {
                    parentFile!!.mkdirs()
                    writeText("ok")
                }
            }

            val zipped = ShellPlatform.prepareShareableExport(context, sourceDir, nowMs = nowMs)
            assertFalse("export zip older than retention window should be pruned", agedZip.exists())
            assertTrue("recent export zip within retention window must be preserved", recentZip.exists())
            assertTrue(zipped.isFile)
            assertEquals(exportsDir.canonicalFile, zipped.parentFile!!.canonicalFile)
            assertEquals("raw-dump-bundle.zip", zipped.name)

            ZipFile(zipped).use { zf ->
                val names = zf.entries().toList().map { it.name }.toSet()
                assertEquals(setOf("report.json", "sub/log.txt"), names)
            }
            sourceDir.deleteRecursively()
        }
    }

    @Test
    fun buildShareIntentAttachesClipDataGrantFlagsAndContextAppropriateTaskFlag() {
        val app = RuntimeEnvironment.getApplication()
        val activity = Robolectric.buildActivity(Activity::class.java).create().get()
        val file = File(app.cacheDir, "exports/diag.zip")
        val uri = Uri.parse("content://sh.amux.android.fileprovider/exports/diag.zip")

        val appChooser = ShellPlatform.buildShareIntent(app, file, uri)
        assertTrue((appChooser.flags and Intent.FLAG_GRANT_READ_URI_PERMISSION) != 0)
        assertTrue(
            "non-Activity context should set FLAG_ACTIVITY_NEW_TASK",
            (appChooser.flags and Intent.FLAG_ACTIVITY_NEW_TASK) != 0,
        )

        val inner = appChooser.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
        assertNotNull(inner)
        assertEquals(Intent.ACTION_SEND, inner!!.action)
        assertNotNull("send intent must attach ClipData for read permission propagation", inner.clipData)
        assertEquals(uri, inner.clipData!!.getItemAt(0).uri)
        assertTrue((inner.flags and Intent.FLAG_GRANT_READ_URI_PERMISSION) != 0)

        val activityChooser = ShellPlatform.buildShareIntent(activity, file, uri)
        assertTrue((activityChooser.flags and Intent.FLAG_GRANT_READ_URI_PERMISSION) != 0)
        assertEquals(
            "Activity context must not force FLAG_ACTIVITY_NEW_TASK",
            0,
            activityChooser.flags and Intent.FLAG_ACTIVITY_NEW_TASK,
        )
    }

    @Test
    fun shareFileReturnsFailureWhenFileProviderCannotResolveUri() {
        val app = RuntimeEnvironment.getApplication()
        val unconfiguredFile = File(app.cacheDir, "exports/missing-provider.zip")
        val result = ShellPlatform.shareFile(app, unconfiguredFile)
        assertTrue("missing FileProvider authority in library test must return failure", result.isFailure)
    }
}
