package sh.amux.core.runtime

import java.io.File
import java.net.InetAddress
import java.nio.charset.StandardCharsets
import java.util.UUID
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runCurrent
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
    fun networkChangeDebouncerTellsDefaultNetworkChangesOncePerBurst() = runTest {
        var notifications = 0
        val debouncer = NetworkChangeDebouncer(scope = this, debounceMs = 250L)
        debouncer.start(network = 100L) { notifications += 1 }

        // Registering repeats the network the watch started on.
        debouncer.available(network = 100L)
        advanceTimeBy(500L)
        assertEquals(0, notifications)

        // A handover (lost, then another network) is told once.
        debouncer.lost(network = 100L)
        advanceTimeBy(50L)
        debouncer.available(network = 200L)
        advanceTimeBy(300L)
        assertEquals(1, notifications)

        // Losing a network that is not the default tells nothing.
        debouncer.lost(network = 100L)
        advanceTimeBy(300L)
        assertEquals(1, notifications)

        // Coming back after none is a change.
        debouncer.lost(network = 200L)
        advanceTimeBy(300L)
        assertEquals(2, notifications)
        debouncer.available(network = 300L)
        advanceTimeBy(300L)
        assertEquals(3, notifications)

        // Nothing is told after a stop, even a burst already waiting.
        debouncer.lost(network = 300L)
        debouncer.stop()
        advanceTimeBy(300L)
        assertEquals(3, notifications)
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
            link = 0L,
            sameNetwork = false,
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
        val store = ReportStore(redactor = { frame, _ -> frame.png })
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
        var redactedMarksSeen: List<ReportMark> = emptyList()
        val store = ReportStore(
            redactor = { _, marks ->
                redactedMarksSeen = marks
                redactedPng
            },
        )
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
        )
        assertTrue(store.state.value.sending is ReportSending.Failed)
        assertTrue(uploads.isEmpty())

        // Submitting under the original account applies redaction and uploads redacted bytes.
        store.send(
            submitter = submitter,
            account = AccountId("account-a"),
            build = "0.10.0",
        )
        assertTrue(store.state.value.sending is ReportSending.Sent)
        assertEquals(1, uploads.size)
        assertEquals(1, redactedMarksSeen.size)
        assertTrue(redactedMarksSeen.single().redact)
        assertTrue(uploads.single().frame.contentEquals(redactedPng))
    }

    @Test
    fun reportStoreTracksResolvedScreenshotAndDumpStates() = runTest {
        val frameDeferred = CompletableDeferred<ByteArray>()
        val dumpDeferred = CompletableDeferred<Result<File>>()
        val capture = ReportCapture(
            frame = FrozenFrame(png = ByteArray(0), widthDp = 400.0, heightDp = 800.0, scale = 2.0),
            dump = dumpDeferred,
            framePng = frameDeferred,
            route = "chat",
        )
        val store = ReportStore(redactor = { frame, _ -> frame.png })
        assertTrue(store.offer { capture })
        assertEquals(ReportScreenshotState.Pending, store.state.value.screenshotState)
        assertEquals(ReportDumpState.Pending, store.state.value.dumpState)

        frameDeferred.complete(byteArrayOf(1, 2, 3))
        assertEquals(ReportScreenshotState.Present, store.state.value.screenshotState)
        assertTrue(store.state.value.capture!!.frame.png.contentEquals(byteArrayOf(1, 2, 3)))
        assertEquals(ReportDumpState.Pending, store.state.value.dumpState)

        dumpDeferred.complete(Result.success(File("/tmp/amux-dump-ready")))
        assertEquals(ReportDumpState.Ready, store.state.value.dumpState)

        // Failed / empty completions resolve cleanly to Absent.
        store.dismiss()
        val emptyFrameDeferred = CompletableDeferred<ByteArray>()
        val failedDumpDeferred = CompletableDeferred<Result<File>>()
        assertTrue(
            store.begin {
                ReportCapture(
                    frame = FrozenFrame(png = ByteArray(0), widthDp = 400.0, heightDp = 800.0, scale = 2.0),
                    dump = failedDumpDeferred,
                    framePng = emptyFrameDeferred,
                )
            },
        )
        assertEquals(ReportScreenshotState.Pending, store.state.value.screenshotState)
        assertEquals(ReportDumpState.Pending, store.state.value.dumpState)

        emptyFrameDeferred.complete(ByteArray(0))
        failedDumpDeferred.complete(Result.failure(RuntimeFailure("dump failed")))
        assertEquals(ReportScreenshotState.Absent, store.state.value.screenshotState)
        assertEquals(ReportDumpState.Absent, store.state.value.dumpState)
    }

    @Test
    fun reportStoreClaimsSendBeforeAssemblyAndCancelsStaleUploadOnDismiss() = runTest {
        val firstDump = CompletableDeferred<Result<File>>()
        val firstCapture = ReportCapture(
            frame = FrozenFrame(png = byteArrayOf(1, 2, 3), widthDp = 400.0, heightDp = 800.0, scale = 2.0),
            dump = firstDump,
            route = "chat",
            account = AccountId("ada"),
        )
        val store = ReportStore(redactor = { frame, _ -> frame.png })
        assertTrue(store.begin { firstCapture })

        val uploads = mutableListOf<ReportUpload>()
        val submitter = ReportSubmitting { upload ->
            uploads.add(upload)
            Result.success(ReportSent(reportPath = "/saved/r1", id = "rep-1", receivedAtMs = 100L, notSent = ""))
        }

        // Double-tap send while assembly is still awaiting the dump must only submit once.
        val firstSend = launch {
            store.send(submitter = submitter, account = AccountId("ada"), build = "0.10.0")
        }
        runCurrent()
        assertEquals(ReportSending.Sending, store.state.value.sending)

        val duplicateSend = launch {
            store.send(submitter = submitter, account = AccountId("ada"), build = "0.10.0")
        }
        runCurrent()
        assertTrue(duplicateSend.isCompleted)

        firstDump.complete(Result.success(File("/tmp/amux-dump-1")))
        advanceUntilIdle()
        assertTrue(firstSend.isCompleted)
        assertEquals(1, uploads.size)
        assertTrue(store.state.value.sending is ReportSending.Sent)

        // Dismissing and reopening mid-assembly cancels the stale upload and never overwrites the new draft.
        store.dismiss()
        uploads.clear()
        val staleDump = CompletableDeferred<Result<File>>()
        val staleCapture = ReportCapture(
            frame = FrozenFrame(png = byteArrayOf(4, 5, 6), widthDp = 400.0, heightDp = 800.0, scale = 2.0),
            dump = staleDump,
            route = "settings",
            account = AccountId("ada"),
        )
        assertTrue(store.begin { staleCapture })
        val staleSend = launch {
            store.send(submitter = submitter, account = AccountId("ada"), build = "0.10.0")
        }
        runCurrent()
        assertEquals(ReportSending.Sending, store.state.value.sending)

        store.dismiss()
        val freshCapture = ReportCapture(
            frame = FrozenFrame(png = byteArrayOf(7, 8, 9), widthDp = 400.0, heightDp = 800.0, scale = 2.0),
            dump = CompletableDeferred(Result.success(File("/tmp/amux-dump-fresh"))),
            route = "agents",
            account = AccountId("ada"),
        )
        assertTrue(store.begin { freshCapture })
        store.setNote("fresh draft")

        // Completing the dismissed capture's dump must not upload or clobber the reopened draft.
        staleDump.complete(Result.success(File("/tmp/amux-dump-stale")))
        advanceUntilIdle()
        assertTrue(staleSend.isCompleted)
        assertTrue("dismissed report must not upload after assembly", uploads.isEmpty())
        assertEquals(ReportSending.Ready, store.state.value.sending)
        assertEquals("fresh draft", store.state.value.draft.note)
        assertNull(store.state.value.sealed)
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
