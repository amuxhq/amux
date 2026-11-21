package sh.amux.android

import android.content.Intent
import android.net.Uri
import java.nio.ByteBuffer
import java.util.Base64
import java.util.UUID
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import sh.amux.core.runtime.HostId
import sh.amux.shell.PairingModal

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class MainActivityTest {
    private fun pairingUri(host: HostId): Uri {
        val payload = ByteBuffer.allocate(1 + 16 + 32 + 2 + 4).apply {
            put(1.toByte())
            put(host.byteArray)
            put(ByteArray(32) { 0x11 })
            putShort(4433)
            put(byteArrayOf(192.toByte(), 168.toByte(), 1, 20))
        }
        val fragment = Base64.getUrlEncoder().withoutPadding().encodeToString(payload.array())
        return Uri.parse("https://amux.sh/p#$fragment")
    }

    @Test
    fun launchesMainActivityAndHandlesColdAndWarmPairingAppLinks() {
        val coldHost = HostId(UUID.fromString("12345678-1234-5678-9abc-def012345678"))
        val coldIntent = Intent(Intent.ACTION_VIEW, pairingUri(coldHost))

        val controller = Robolectric.buildActivity(MainActivity::class.java, coldIntent).setup()
        val activity = controller.get()
        assertEquals("amux", activity.getString(R.string.app_name))
        assertTrue(DebugHooks.enabled)

        val app = activity.application as AmuxApplication
        val coldModal = app.composition.router.snapshot.pairing
        assertTrue(coldModal is PairingModal.Link)
        assertEquals(coldHost, (coldModal as PairingModal.Link).invitation.host)

        // Warm App Link via onNewIntent replaces the pairing modal cleanly
        val warmHost = HostId(UUID.fromString("87654321-4321-8765-cba9-fed098765432"))
        val warmIntent = Intent(Intent.ACTION_VIEW, pairingUri(warmHost))
        controller.newIntent(warmIntent)
        val warmModal = app.composition.router.snapshot.pairing
        assertTrue(warmModal is PairingModal.Link)
        assertEquals(warmHost, (warmModal as PairingModal.Link).invitation.host)

        controller.pause().stop().destroy()
    }

    @Test
    fun deliversWarmOAuthCallbackToPendingBrowserSessionBeforeResume() {
        runBlocking {
            val controller = Robolectric.buildActivity(MainActivity::class.java).setup()
            val activity = controller.get()
            val composition = (activity.application as AmuxApplication).composition

            val pending = async {
                composition.oauthBrowser.present(
                    url = "https://amux.sh/connect/authorize?state=xyz",
                    callbackScheme = "amux",
                )
            }
            // Let present() register the pending session on this coroutine dispatcher
            kotlinx.coroutines.yield()
            assertTrue(composition.oauthBrowser.hasPendingSession)

            val callbackUri = Uri.parse("amux://callback?code=abc123&state=xyz")
            controller.newIntent(Intent(Intent.ACTION_VIEW, callbackUri))
            controller.resume()

            assertEquals(callbackUri.toString(), pending.await())
            assertEquals(callbackUri.toString(), composition.lastAuthCallback.value)
            assertFalse(composition.oauthBrowser.hasPendingSession)

            controller.pause().stop().destroy()
        }
    }

    @Test
    fun screenCaptureCallbackOffersReportWithFrozenCapture() {
        val controller = Robolectric.buildActivity(MainActivity::class.java).setup()
        val activity = controller.get()
        val composition = (activity.application as AmuxApplication).composition
        composition.report.dismiss()

        activity.screenCaptureCallback.onScreenCaptured()

        val state = composition.report.state.value
        assertTrue("screenshot callback should raise report offer", state.offering)
        assertFalse("screenshot offer should not immediately cover screen", state.open)
        assertNotNull("capture should be attached", state.capture)

        composition.report.dismiss()
        controller.pause().stop().destroy()
    }

    @Test
    fun doorDataDirConfinementRejectsPathsOutsideAmuxDoor() {
        val filesDir = java.nio.file.Files.createTempDirectory("amux-door-path-test").toFile()
        try {
            val defaultDir = DoorHost.resolveDoorDataDir(filesDir, null)
            assertTrue(defaultDir.path.startsWith(java.io.File(filesDir, "amux-door").canonicalPath))

            val subDir = DoorHost.resolveDoorDataDir(filesDir, "run-1")
            assertTrue(subDir.path.startsWith(java.io.File(filesDir, "amux-door").canonicalPath))

            for (bad in listOf("..", "../amux/installation", ".", "/tmp/escape")) {
                var rejected = false
                try {
                    DoorHost.resolveDoorDataDir(filesDir, bad)
                } catch (_: IllegalArgumentException) {
                    rejected = true
                }
                assertTrue("expected door data_dir $bad to be rejected", rejected)
            }
        } finally {
            filesDir.deleteRecursively()
        }
    }
}
