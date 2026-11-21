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
    fun nsdDiscoveryTrackerPublishesEmptySnapshotOnForgetAndIgnoresStaleGenerations() {
        val emissions = mutableListOf<List<Found>>()
        val tracker = NsdDiscoveryTracker { emissions.add(it) }
        val attrs = mapOf(
            "hid" to hostId.toString().toByteArray(StandardCharsets.UTF_8),
            "name" to "desk".toByteArray(StandardCharsets.UTF_8),
            "scope" to "".toByteArray(StandardCharsets.UTF_8),
            "pair" to "1".toByteArray(StandardCharsets.UTF_8),
        )
        val ipv4 = InetAddress.getByName("192.168.1.10")

        val gen1 = tracker.beginSession()
        tracker.onServiceUpdated("desk", attrs, listOf(ipv4), 41999, generation = gen1)
        assertEquals(1, emissions.last().size)

        // Stopping discovery clears local state AND withdraws results by emitting emptyList().
        tracker.forget()
        assertTrue(tracker.current().isEmpty())
        assertTrue(emissions.last().isEmpty())
        val countAfterForget = emissions.size

        // Callbacks from superseded browse sessions are ignored.
        val gen2 = tracker.beginSession()
        tracker.onServiceUpdated("desk", attrs, listOf(ipv4), 41999, generation = gen1)
        assertEquals(countAfterForget, emissions.size)
        assertTrue(tracker.current().isEmpty())

        // Fresh session callbacks take effect.
        tracker.onServiceUpdated("desk", attrs, listOf(ipv4), 41999, generation = gen2)
        assertEquals(1, tracker.current().size)
        assertEquals("desk", emissions.last().single().name)
    }

    @Test
    fun networkChangeDebouncerSuppressesInitialBurstAndRssiJitter() = runTest {
        var notifications = 0
        val debouncer = NetworkChangeDebouncer(scope = this, debounceMs = 250L)
        debouncer.start { notifications += 1 }

        val wifiMask = 1 shl 1
        val cellMask = 1 shl 0

        // Entire initial registration burst (onAvailable + initial capabilities) is suppressed.
        debouncer.onAvailable(networkId = 100L)
        debouncer.onCapabilitiesChanged(networkId = 100L, transports = wifiMask)
        advanceTimeBy(500L)
        assertEquals(0, notifications)

        // Repeated capabilities with identical network + transports (RSSI/bandwidth jitter) are ignored.
        debouncer.onCapabilitiesChanged(networkId = 100L, transports = wifiMask)
        debouncer.onCapabilitiesChanged(networkId = 100L, transports = wifiMask)
        advanceTimeBy(500L)
        assertEquals(0, notifications)

        // Real transport handover (Wi-Fi -> cellular on new network) coalesces into 1 notification.
        debouncer.onLost(networkId = 100L)
        advanceTimeBy(50L)
        debouncer.onAvailable(networkId = 200L)
        debouncer.onCapabilitiesChanged(networkId = 200L, transports = cellMask)
        advanceTimeBy(300L)
        assertEquals(1, notifications)

        // Adding VPN transport on the same network counts as a transport change.
        debouncer.onCapabilitiesChanged(networkId = 200L, transports = cellMask or (1 shl 4))
        advanceTimeBy(300L)
        assertEquals(2, notifications)

        debouncer.stop()
    }

    @Test
    fun storeBundleCachesPartialFleetDeltasAndAbandonsLatePendingPair() = runTest {
        val abandoned = mutableListOf<sh.amux.values.PendingPair>()
        val pairDeferred = CompletableDeferred<Result<sh.amux.values.PendingPair>>()
        val bundle = StoreBundle(
            account = AccountId("signed-out"),
            scope = this,
            beginPairOp = { pairDeferred.await() },
            abandonPairOp = { pending ->
                abandoned.add(pending)
                Result.success(Unit)
            },
        )

        val view = sh.amux.values.FleetView(sections = emptyList(), needYou = 0, working = 0)
        val deskHost = sh.amux.values.HostView(
            hostId = hostId.bytes,
            name = "desk",
            local = false,
            trusted = true,
            candidate = false,
            reach = sh.amux.values.Reach.Online(sh.amux.values.RouteKind.Direct),
            addrs = emptyList(),
            relayOnly = false,
            pairingOpen = false,
            current = true,
            providers = emptyList(),
            platform = sh.amux.values.Platform.Macos,
        )

        // Partial delta with only view is cached until hosts arrive.
        bundle.moved(FleetEvent.Fleet(sh.amux.values.FleetDelta(view = view, hosts = null)))
        assertEquals(0, bundle.applied.value)

        // Second partial delta with only hosts merges with the cached view!
        bundle.moved(FleetEvent.Fleet(sh.amux.values.FleetDelta(view = null, hosts = listOf(deskHost))))
        assertEquals(1, bundle.applied.value)
        assertEquals(1, bundle.hosts.snapshot.hosts.size)
        assertEquals("desk", bundle.hosts.snapshot.hosts.single().name)

        // Start pairing via link, then cancel before beginPair completes.
        assertTrue(bundle.pairLink("https://amux.sh/p#abc"))
        assertEquals(PairingStore.Phase.Checking, bundle.pairing.snapshot.phase)
        bundle.cancelPairing()
        assertEquals(PairingStore.Phase.Entering, bundle.pairing.snapshot.phase)

        val latePending = sh.amux.values.PendingPair(
            token = listOf(9, 8, 7),
            hostId = hostId.bytes,
            name = "desk",
            fingerprint = "deadbeef",
            expiresAtMs = 999_999L,
            via = sh.amux.values.RouteKind.Direct,
        )
        pairDeferred.complete(Result.success(latePending))
        advanceTimeBy(100L)

        assertEquals(PairingStore.Phase.Entering, bundle.pairing.snapshot.phase)
        assertEquals(listOf(latePending), abandoned)
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
    fun reportStoreRefusesCrossAccountUploadAndAppliesRedactor() = runTest {
        val originalPng = byteArrayOf(10, 20, 30, 40)
        val redactedPng = byteArrayOf(99, 98, 97)
        val capture = ReportCapture(
            frame = FrozenFrame(png = originalPng, widthDp = 400.0, heightDp = 800.0, scale = 2.0),
            dump = CompletableDeferred(Result.success(File("/tmp/amux-dump-a"))),
            route = "chat",
            account = AccountId("account-a"),
            profileId = "profile-a",
        )
        val store = ReportStore()
        assertTrue(store.offer { capture })
        store.accept()
        store.mark(ReportMark(10.0, 20.0, 80.0, 40.0, note = "secret", redact = true))

        val uploads = mutableListOf<ReportUpload>()
        val submitter = ReportSubmitting { upload ->
            uploads.add(upload)
            Result.success(ReportSent(reportPath = "/saved/r1", id = "rep-1", receivedAtMs = 100L, notSent = ""))
        }

        // Switching to account-b must refuse uploading account-a's capture.
        store.send(
            submitter = submitter,
            account = AccountId("account-b"),
            build = "0.10.0",
            redactor = { _, _ -> redactedPng },
        )
        assertTrue(store.state.value.sending is ReportSending.Failed)
        assertTrue(uploads.isEmpty())

        // Submitting under the original account applies redaction and uploads redacted bytes.
        var redactedMarksSeen: List<ReportMark> = emptyList()
        store.send(
            submitter = submitter,
            account = AccountId("account-a"),
            build = "0.10.0",
            redactor = { _, marks ->
                redactedMarksSeen = marks
                redactedPng
            },
        )
        assertTrue(store.state.value.sending is ReportSending.Sent)
        assertEquals(1, uploads.size)
        assertEquals(1, redactedMarksSeen.size)
        assertTrue(redactedMarksSeen.single().redact)
        assertTrue(uploads.single().frame.contentEquals(redactedPng))
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
