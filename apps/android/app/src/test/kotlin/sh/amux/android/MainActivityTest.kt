package sh.amux.android

import android.Manifest
import android.content.ComponentName
import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.pm.PackageManager
import android.net.Uri
import java.io.File
import java.nio.ByteBuffer
import java.util.Base64
import java.util.UUID
import javax.xml.parsers.DocumentBuilderFactory
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
import org.robolectric.RuntimeEnvironment
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
    fun mergedManifestDeclaresScreenCapturePermissionSingleTaskAutoVerifyAndQueries() {
        val app = RuntimeEnvironment.getApplication()
        val pm = app.packageManager
        val pkgInfo = pm.getPackageInfo(app.packageName, PackageManager.GET_PERMISSIONS)
        val permissions = pkgInfo.requestedPermissions?.toSet().orEmpty()
        assertTrue(
            "merged manifest must declare DETECT_SCREEN_CAPTURE",
            Manifest.permission.DETECT_SCREEN_CAPTURE in permissions,
        )
        assertTrue(Manifest.permission.POST_NOTIFICATIONS in permissions)
        assertTrue(Manifest.permission.RECORD_AUDIO in permissions)

        val activityInfo = pm.getActivityInfo(ComponentName(app, MainActivity::class.java), 0)
        assertEquals(
            "MainActivity must use singleTask so amux://callback clears Custom Tabs cleanly",
            ActivityInfo.LAUNCH_SINGLE_TASK,
            activityInfo.launchMode,
        )

        // Inspect the merged debug manifest XML directly for autoVerify and
        // <queries> intents: the one the build gave Robolectric, which it
        // names in the test config on the classpath.
        val testConfig = java.util.Properties().apply {
            MainActivityTest::class.java.getResourceAsStream("/com/android/tools/test_config.properties")?.use { load(it) }
        }
        val mergedManifest = testConfig.getProperty("android_merged_manifest")?.let(::File)?.takeIf { it.isFile }
        assertNotNull("manifest file must exist for inspection", mergedManifest)
        val doc = DocumentBuilderFactory.newInstance().apply { isNamespaceAware = true }
            .newDocumentBuilder()
            .parse(mergedManifest!!)

        val androidNs = "http://schemas.android.com/apk/res/android"
        val intentFilters = doc.getElementsByTagName("intent-filter")
        var foundAutoVerifyAppLink = false
        for (i in 0 until intentFilters.length) {
            val filter = intentFilters.item(i) as org.w3c.dom.Element
            val autoVerify = filter.getAttributeNS(androidNs, "autoVerify")
            val dataNodes = filter.getElementsByTagName("data")
            for (j in 0 until dataNodes.length) {
                val data = dataNodes.item(j) as org.w3c.dom.Element
                if (data.getAttributeNS(androidNs, "host") == "amux.sh" &&
                    data.getAttributeNS(androidNs, "path") == "/p"
                ) {
                    assertEquals("true", autoVerify)
                    foundAutoVerifyAppLink = true
                }
            }
        }
        assertTrue("https://amux.sh/p filter must declare android:autoVerify=\"true\"", foundAutoVerifyAppLink)

        val queryActions = mutableSetOf<String>()
        val queriesNodes = doc.getElementsByTagName("queries")
        for (i in 0 until queriesNodes.length) {
            val queries = queriesNodes.item(i) as org.w3c.dom.Element
            val actions = queries.getElementsByTagName("action")
            for (j in 0 until actions.length) {
                val action = actions.item(j) as org.w3c.dom.Element
                queryActions += action.getAttributeNS(androidNs, "name")
            }
        }
        assertTrue(
            "manifest <queries> must declare CustomTabsService",
            "android.support.customtabs.action.CustomTabsService" in queryActions,
        )
        assertTrue(
            "manifest <queries> must declare RecognitionService",
            "android.speech.RecognitionService" in queryActions,
        )
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
