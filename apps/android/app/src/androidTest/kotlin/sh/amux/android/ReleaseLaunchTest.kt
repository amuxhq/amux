package sh.amux.android

import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import sh.amux.core.runtime.BridgeException
import sh.amux.core.runtime.NativeBridge
import sh.amux.core.runtime.Runtime
import sh.amux.values.ProfileView
import sh.amux.values.StartConfig

/**
 * Runs on the minified release build (`just android release-smoke`), in the
 * app's own process, against the shipping `libamux_app.so`.
 *
 * R8 renames and removes what no Kotlin code names, but the Rust bridge
 * finds `NativeBridge`'s methods and `BridgeException`'s constructor by name,
 * and the runtime's answers are decoded by generated serializers. A rule
 * missing for any of them builds cleanly and fails only on a phone, so this
 * test takes each path once: start the runtime, take the step that lists its
 * profiles, run an act, and have the bridge refuse a call.
 *
 * The test calls the runtime the way RuntimeCoordinator does, with every
 * argument named, so it reaches only what R8 kept for the app itself.
 */
class ReleaseLaunchTest {
    @Test
    fun theShippingBuildStartsStepsActsAndRefusesThroughTheBridge() {
        assertFalse(
            "the shipping library carries no driving tools",
            NativeBridge.version().endsWith("+debug-tools"),
        )

        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val dir = File(context.cacheDir, "release-launch").apply {
            deleteRecursively()
            mkdirs()
        }
        val listed = CompletableFuture<List<ProfileView>>()
        val runtime = Runtime.start(
            config = StartConfig(
                dataDir = dir.absolutePath,
                deviceName = "Release Launch",
                autoReports = false,
                discoveryScope = null,
                lan = false,
                lanBind = null,
                logPath = File(dir, "runtime.log").absolutePath,
                relayQuic = null,
                relayRoot = null,
                relayTcp = null,
                tail = null,
                telemetry = false,
            ),
            dispatch = { it() },
            onProfilesMoved = { profiles -> if (profiles.isNotEmpty()) listed.complete(profiles) },
            onStopped = { why -> listed.completeExceptionally(IllegalStateException(why)) },
        )
        try {
            // A step whose profiles do not decode is logged and skipped, so
            // the list arriving is what proves the serializers survived.
            val profiles = listed.get(30, TimeUnit.SECONDS)
            assertEquals("a fresh installation holds one profile", 1, profiles.size)

            val created = runBlocking { runtime.createProfile() }.getOrThrow()
            assertNotEquals(profiles.single().id, created.id)

            try {
                NativeBridge.profileOpen(Long.MAX_VALUE, created.id)
                fail("a runtime handle that was never issued opens nothing")
            } catch (refused: BridgeException) {
                assertTrue(refused.message.orEmpty().isNotBlank())
            }
        } finally {
            runtime.stop()
            dir.deleteRecursively()
        }
    }
}
