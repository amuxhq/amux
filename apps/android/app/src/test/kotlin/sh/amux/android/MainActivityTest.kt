package sh.amux.android

import android.content.Intent
import android.net.Uri
import java.nio.ByteBuffer
import java.util.Base64
import java.util.UUID
import org.junit.Assert.assertEquals
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
    @Test
    fun launchesMainActivityAndHandlesPairingDeepLinkIntent() {
        val host = HostId(UUID.fromString("12345678-1234-5678-9abc-def012345678"))
        val payload = ByteBuffer.allocate(1 + 16 + 32 + 2 + 4).apply {
            put(1.toByte())
            put(host.byteArray)
            put(ByteArray(32) { 0x11 })
            putShort(4433)
            put(byteArrayOf(192.toByte(), 168.toByte(), 1, 20))
        }
        val fragment = Base64.getUrlEncoder().withoutPadding().encodeToString(payload.array())
        val linkIntent = Intent(Intent.ACTION_VIEW, Uri.parse("https://amux.sh/p#$fragment"))

        val controller = Robolectric.buildActivity(MainActivity::class.java, linkIntent).setup()
        val activity = controller.get()
        assertEquals("amux", activity.getString(R.string.app_name))
        assertTrue(DebugHooks.enabled)

        val app = activity.application as AmuxApplication
        val modal = app.composition.router.snapshot.pairing
        assertTrue(modal is PairingModal.Link)
        assertEquals(host, (modal as PairingModal.Link).invitation.host)
    }
}
