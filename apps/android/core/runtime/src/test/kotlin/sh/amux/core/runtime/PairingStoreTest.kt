package sh.amux.core.runtime

import java.util.Base64
import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.PairRequest
import sh.amux.values.PendingPair
import sh.amux.values.RouteKind

class PairingStoreTest {
    private val hostId = HostId(UUID.fromString("01020304-0506-0708-090a-0b0c0d0e0f10"))

    private fun buildInvitationLink(
        base: String = "https://amux.sh/p",
        version: Byte = 1,
        host: HostId = hostId,
        port: Int = 41999,
        ipv4s: List<ByteArray> = listOf(byteArrayOf(192.toByte(), 168.toByte(), 1, 42)),
    ): String {
        val secret = ByteArray(32) { i -> (i + 1).toByte() }
        val total = 1 + 16 + 32 + 2 + ipv4s.size * 4
        val bytes = ByteArray(total)
        bytes[0] = version
        host.byteArray.copyInto(bytes, destinationOffset = 1)
        secret.copyInto(bytes, destinationOffset = 17)
        bytes[49] = ((port ushr 8) and 0xFF).toByte()
        bytes[50] = (port and 0xFF).toByte()
        var offset = 51
        for (ip in ipv4s) {
            ip.copyInto(bytes, destinationOffset = offset)
            offset += 4
        }
        val payload = Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)
        return "$base#$payload"
    }

    @Test
    fun parsesValidQrLinksAndRejectsMalformedOnes() {
        val lanLink = buildInvitationLink()
        val parsed = PairingInvitation.parse(lanLink)
        assertNotNull(parsed)
        assertEquals(hostId, parsed!!.host)
        assertEquals(listOf("192.168.1.42:41999"), parsed.addrs)
        assertFalse(parsed.needsAnAccount)
        assertEquals("invitation from $hostId", parsed.toString())

        val relayOnlyLink = buildInvitationLink(
            base = "https://cloud.example.com/p",
            port = 0,
            ipv4s = emptyList(),
        )
        val relayParsed = PairingInvitation.parse(relayOnlyLink)
        assertNotNull(relayParsed)
        assertTrue(relayParsed!!.needsAnAccount)

        // Malformed links must be rejected cleanly.
        assertNull(PairingInvitation.parse("https://amux.sh/p"))
        assertNull(PairingInvitation.parse("https://amux.sh/p#"))
        assertNull(PairingInvitation.parse("https://amux.sh/p/extra#abc"))
        assertNull(PairingInvitation.parse("ftp://amux.sh/p#abc"))
        assertNull(PairingInvitation.parse("https://amux.sh/p#not-base64!!"))
        assertNull(PairingInvitation.parse(buildInvitationLink(version = 2)))
        assertNull(PairingInvitation.parse(buildInvitationLink(port = 0))) // port 0 with addrs
    }

    @Test
    fun pinEntryTransitionsAndStaleAttemptsAndLockoutToldProse() {
        val store = PairingStore()
        store.open()

        // Pasted code with spaces and extra characters trims to 6 digits.
        store.enter("418 20799")
        assertEquals("418207", store.snapshot.digits)
        assertEquals(
            PairRequest.Pin(pin = "418207", addrs = emptyList(), hostId = null),
            store.snapshot.request,
        )

        val firstAttempt = store.checking()
        assertEquals(PairingStore.Phase.Checking, store.snapshot.phase)

        // Abandon bumps attempt so a late answer from firstAttempt is ignored.
        store.abandoned()
        assertEquals(PairingStore.Phase.Entering, store.snapshot.phase)
        assertEquals("", store.snapshot.digits)

        val pending = PendingPair(
            token = listOf(1, 2, 3),
            hostId = hostId.bytes,
            name = "desk",
            fingerprint = "deadbeef",
            expiresAtMs = 999_999L,
            via = RouteKind.Direct,
        )
        store.reached(Result.success(pending), attempt = firstAttempt)
        assertEquals(PairingStore.Phase.Entering, store.snapshot.phase)

        // Lockout prose from core passes through as `told`, raw code stays masked.
        val secondAttempt = store.checking()
        store.reached(
            Result.failure(RuntimeFailure("Too many wrong codes closed this pairing window.")),
            attempt = secondAttempt,
        )
        assertEquals(PairingStore.Phase.Refused, store.snapshot.phase)
        assertEquals(
            "Too many wrong codes closed this pairing window.",
            store.snapshot.told,
        )

        val thirdAttempt = store.checking()
        store.reached(
            Result.failure(RuntimeFailure("INVALID_PIN")),
            attempt = thirdAttempt,
            relayOnly = true,
        )
        assertEquals(PairingStore.Phase.NeedsSubscription, store.snapshot.phase)
        assertNull(store.snapshot.told)
        assertEquals("INVALID_PIN", store.snapshot.refusal)

        // Full success path: reached -> confirming -> confirmed -> trusted.
        store.open()
        val okAttempt = store.checking()
        store.reached(Result.success(pending), attempt = okAttempt)
        assertEquals(PairingStore.Phase.Confirming(pending), store.snapshot.phase)
        val confirmAttempt = store.checking()
        store.paired(Result.success("desk"), attempt = confirmAttempt)
        assertEquals(PairingStore.Phase.Trusted("desk"), store.snapshot.phase)
    }

    @Test
    fun normalizesLocalizedUnicodeDigitsToAsciiBeforeSubmittingPin() {
        val store = PairingStore()
        store.open()

        // Arabic-Indic digits ٤١٨٢٠٧ must normalize to ASCII 418207.
        store.enter("٤١٨٢٠٧")
        assertEquals("418207", store.snapshot.digits)
        assertEquals(
            PairRequest.Pin(pin = "418207", addrs = emptyList(), hostId = null),
            store.snapshot.request,
        )

        // Mixed localized/fullwidth digits with separators and extra chars normalize and truncate to 6.
        store.enter("４１-٨ २ 0 ٧ 9")
        assertEquals("418207", store.snapshot.digits)
    }
}
