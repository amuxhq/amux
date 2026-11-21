package sh.amux.core.runtime

import java.io.File
import java.net.InetAddress
import java.nio.charset.StandardCharsets
import java.util.UUID
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.Found
import sh.amux.values.ReportSent

@OptIn(ExperimentalCoroutinesApi::class)
class ServicesAndReportingTest {
    private val hostId = HostId(UUID.fromString("11112222-3333-4444-5555-666677778888"))

    @Test
    fun nsdDiscoveryTrackerParsesTxtFormatsAddressesAndTracksLoss() {
        val emissions = mutableListOf<List<Found>>()
        val tracker = NsdDiscoveryTracker { emissions.add(it) }

        val attrs = mapOf(
            "hid" to hostId.toString().toByteArray(StandardCharsets.UTF_8),
            "name" to "desk".toByteArray(StandardCharsets.UTF_8),
            "scope" to "".toByteArray(StandardCharsets.UTF_8),
            "pair" to "1".toByteArray(StandardCharsets.UTF_8),
        )
        val ipv4 = InetAddress.getByName("192.168.1.10")
        val linkLocalV6 = InetAddress.getByName("fe80::1")
        val globalV6 = InetAddress.getByName("2001:db8::1")

        tracker.onServiceUpdated(
            serviceName = "desk (2)",
            attributes = attrs,
            addresses = listOf(ipv4, linkLocalV6, globalV6),
            port = 41999,
        )

        assertEquals(1, emissions.size)
        val found = emissions.last().single()
        assertEquals(hostId.bytes, found.hostId)
        assertEquals("desk", found.name)
        assertEquals(true, found.pairingOpen)
        assertEquals(listOf("192.168.1.10:41999", "[2001:db8:0:0:0:0:0:1]:41999"), found.addrs)

        // Closing pairing window updates in place.
        tracker.onServiceUpdated(
            serviceName = "desk (2)",
            attributes = attrs + ("pair" to "0".toByteArray(StandardCharsets.UTF_8)),
            addresses = listOf(ipv4),
            port = 41999,
        )
        assertEquals(false, emissions.last().single().pairingOpen)

        // Losing service removes it and emits empty list.
        tracker.onServiceLost("desk (2)")
        assertTrue(emissions.last().isEmpty())
    }

    @Test
    fun networkChangeDebouncerSuppressesInitialEventAndCoalescesBursts() = runTest {
        var notifications = 0
        val debouncer = NetworkChangeDebouncer(scope = this, debounceMs = 250L)
        debouncer.start { notifications += 1 }

        // Initial registration callback is suppressed.
        debouncer.onEvent()
        advanceTimeBy(500L)
        assertEquals(0, notifications)

        // Burst of 3 handover events coalesces into 1 notification.
        debouncer.onEvent()
        advanceTimeBy(100L)
        debouncer.onEvent()
        debouncer.onEvent()
        advanceTimeBy(200L)
        assertEquals(1, notifications)

        debouncer.stop()
    }

    @Test
    fun reportAssemblyAndStoreBuildContractJsonAndPreserveSealedBytesOnRetry() = runTest {
        val frame = FrozenFrame(
            png = byteArrayOf(1, 2, 3, 4),
            widthDp = 412.0,
            heightDp = 915.0,
            scale = 2.625,
        )
        val dumpDir = File("/tmp/amux-dump-test")
        val capture = ReportCapture(
            frame = frame,
            dump = CompletableDeferred(Result.success(dumpDir)),
            route = "agents",
        )
        val store = ReportStore()
        assertTrue(store.offer { capture })
        assertTrue(store.state.value.offering)
        assertFalse(store.state.value.open)

        store.accept()
        store.setNote("button clipped")
        store.mark(ReportMark(10.0, 20.0, 100.0, 40.0, "here"))

        // Unsigned send refuses cleanly without losing draft.
        store.send(submitter = null, account = null, build = "0.10.0", gitSha = "abc1234", nowMs = 1_700_000_000_000L)
        assertTrue(store.state.value.sending is ReportSending.Failed)

        val uploads = mutableListOf<ReportUpload>()
        var failOnce = true
        val submitter = ReportSubmitting { upload ->
            uploads.add(upload)
            if (failOnce) {
                failOnce = false
                Result.success(ReportSent(reportPath = "/saved/report-1", id = "", receivedAtMs = 0L, notSent = "offline"))
            } else {
                Result.success(ReportSent(reportPath = "/saved/report-1", id = "rep-99", receivedAtMs = 1_700_000_005_000L, notSent = ""))
            }
        }

        store.send(submitter, AccountId("ada"), build = "0.10.0", gitSha = "abc1234", nowMs = 1_700_000_000_000L)
        assertEquals(ReportSending.Failed("offline"), store.state.value.sending)

        // Retry reuses sealed bytes and saved report path.
        store.send(submitter, AccountId("ada"), build = "0.10.0", gitSha = "abc1234", nowMs = 1_700_000_099_000L)
        assertEquals(
            ReportSending.Sent(ReportReceipt("rep-99", 1_700_000_005_000L)),
            store.state.value.sending,
        )
        assertEquals(2, uploads.size)
        assertEquals(dumpDir.absolutePath, uploads[0].path)
        assertEquals("/saved/report-1", uploads[1].path)
        assertTrue(uploads[0].header.contentEquals(uploads[1].header))

        val headerJson = AmuxJson.json.parseToJsonElement(
            uploads[0].header.toString(StandardCharsets.UTF_8),
        ).jsonObject
        assertEquals(1, headerJson["schema_version"]?.jsonPrimitive?.content?.toInt())
        assertEquals("report", headerJson["kind"]?.jsonPrimitive?.content)
        assertEquals("abc1234", headerJson["git_sha"]?.jsonPrimitive?.content)
        assertEquals("button clipped", headerJson["note"]?.jsonPrimitive?.content)
        assertEquals(1, headerJson["marks"]?.jsonArray?.size)
        val parts = headerJson["parts"]!!.jsonObject
        assertEquals("present", parts["frame"]?.jsonPrimitive?.content)
        assertEquals("present", parts["dump"]?.jsonPrimitive?.content)
        assertNotNull(parts["trace"]?.jsonObject?.get("absent"))
    }

    @Test
    fun stubBillingAndPushAccountWantedDefaults() = runTest {
        val paywall = PaywallStore()
        paywall.load(StubBillingStoreFront())
        assertTrue(paywall.snapshot.phase is PaywallStore.Phase.Failed)
        assertEquals(CloudEndpoint.Production.accountPageUrl, paywall.snapshot.manageUrl)

        assertEquals(false, PushAccount.wanted(signedIn = false, pro = true, entitlement = Entitlement.None))
        assertEquals(true, PushAccount.wanted(signedIn = true, pro = true, entitlement = Entitlement.None))
        assertEquals(true, PushAccount.wanted(signedIn = true, pro = null, entitlement = Entitlement.Active(Grant.Granted)))
        assertNull(PushAccount.wanted(signedIn = true, pro = null, entitlement = Entitlement.None))
    }
}
