package sh.amux.shell

import java.nio.ByteBuffer
import java.util.Base64
import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.core.runtime.HostId

class DeepLinkTest {
    private fun encodeInvitation(
        host: HostId,
        port: Int,
        ipv4s: List<ByteArray>,
    ): String {
        val payload = ByteBuffer.allocate(1 + 16 + 32 + 2 + ipv4s.size * 4)
        payload.put(1.toByte())
        payload.put(host.byteArray)
        payload.put(ByteArray(32) { 0x42 })
        payload.putShort(port.toShort())
        for (ip in ipv4s) {
            payload.put(ip)
        }
        val fragment = Base64.getUrlEncoder().withoutPadding().encodeToString(payload.array())
        return "https://amux.sh/p#$fragment"
    }

    @Test
    fun parsesValidDirectAndRelayPairingLinks() {
        val host = HostId(UUID.fromString("12345678-1234-5678-9abc-def012345678"))
        val directUrl = encodeInvitation(host, 4433, listOf(byteArrayOf(192.toByte(), 168.toByte(), 1, 10)))
        val direct = DeepLink.parse(directUrl) as AppLink.Pairing
        assertEquals(host, direct.invitation.host)
        assertEquals(listOf("192.168.1.10:4433"), direct.invitation.addrs)
        assertEquals(false, direct.invitation.needsAnAccount)

        val relayUrl = encodeInvitation(host, 0, emptyList())
        val relay = DeepLink.parse(relayUrl) as AppLink.Pairing
        assertEquals(host, relay.invitation.host)
        assertTrue(relay.invitation.needsAnAccount)
    }

    @Test
    fun parsesOAuthCallbackAndRejectsMalformedLinks() {
        val callback = DeepLink.parse("amux://callback?code=abc&state=xyz")
        assertEquals(AppLink.AuthCallback("amux://callback?code=abc&state=xyz"), callback)

        assertNull(DeepLink.parse(""))
        assertNull(DeepLink.parse("https://amux.sh/other#abc"))
        assertNull(DeepLink.parse("https://amux.sh/p#not-valid-payload"))
        assertNull(DeepLink.parse("amux://unknown"))
    }
}
