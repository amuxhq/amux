package sh.amux.core.runtime

import java.nio.file.Files
import java.util.concurrent.CompletableFuture
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Assume.assumeFalse
import org.junit.Test
import sh.amux.push.Said
import sh.amux.push.Wants
import sh.amux.values.AccountBinding
import sh.amux.values.AgentAct
import sh.amux.values.AgentNameProblem
import sh.amux.values.Catalogue
import sh.amux.values.ChatDelta
import sh.amux.values.FleetDelta
import sh.amux.values.Found
import sh.amux.values.NewAgentChoices
import sh.amux.values.NewAgentPick
import sh.amux.values.OfferedModel
import sh.amux.values.ProfileView
import sh.amux.values.Roster
import sh.amux.values.StartConfig

class NativeBridgeJvmTest {
    private val isWindows: Boolean =
        System.getProperty("os.name").orEmpty().lowercase().contains("win")

    @Test
    fun versionIncludesDebugTools() {
        val version = NativeBridge.version()
        assertTrue("expected non-empty version with +debug-tools, got '$version'", version.endsWith("+debug-tools"))
    }

    @Test
    fun attachmentLimitMatchesWireContract() {
        assertEquals(64L * 1024L * 1024L, NativeBridge.attachmentLimit())
    }

    @Test
    fun statelessHelpersAndSerdeRoundTripsMatchRustBridge() {
        assertEquals("null", NativeBridge.agentNameProblem("fix-login"))
        val emptyProblemJson = NativeBridge.agentNameProblem("")
        assertNotNull(emptyProblemJson)
        val emptyProblem = AmuxJson.json.decodeFromString<AgentNameProblem>(emptyProblemJson!!)
        assertEquals(AgentNameProblem.Empty, emptyProblem)

        assertTrue(NativeBridge.pastesAsAttachment("line\n".repeat(8)))
        assertFalse(NativeBridge.pastesAsAttachment("two\nlines"))

        // Round-trip NewAgentPick.Model(null) against Rust app-bridge new_agent_pick.
        val catalogue = Catalogue(
            models = listOf(
                OfferedModel(
                    value = "claude-sonnet",
                    displayName = "Sonnet",
                    description = "Balanced model",
                    efforts = listOf("medium", "high"),
                    resolvedModel = "claude-sonnet-4",
                    defaultEffort = "high",
                ),
            ),
            commands = emptyList(),
            permissions = emptyList(),
            modes = emptyList(),
            skills = emptyList(),
        )
        val chosen = NewAgentChoices(model = "claude-sonnet", effort = "high")
        val pick = NewAgentPick.Model(null)
        val pickJson = AmuxJson.json.encodeToString<NewAgentPick>(pick)
        assertEquals("""{"Model":null}""", pickJson)
        assertEquals(pick, AmuxJson.json.decodeFromString<NewAgentPick>(pickJson))

        val updatedJson = NativeBridge.newAgentPick(
            AmuxJson.json.encodeToString(catalogue),
            AmuxJson.json.encodeToString(chosen),
            pickJson,
        )
        assertNotNull("Rust new_agent_pick should accept Kotlin-serialized NewAgentPick.Model(null)", updatedJson)
        val updated = AmuxJson.json.decodeFromString<NewAgentChoices>(updatedJson!!)
        assertNull(updated.model)

        // Round-trip AgentAct, ChatDelta, FleetDelta, StartConfig, and Said.
        val acts: List<AgentAct> = listOf(AgentAct.Close, AgentAct.Rename("renamed"))
        for (act in acts) {
            val encoded = AmuxJson.json.encodeToString(act)
            assertEquals(act, AmuxJson.json.decodeFromString<AgentAct>(encoded))
        }

        val chatDelta = ChatDelta(chat = 7L, rows = emptyList())
        assertEquals(chatDelta, AmuxJson.json.decodeFromString<ChatDelta>(AmuxJson.json.encodeToString(chatDelta)))

        val fleetDelta = FleetDelta(hosts = emptyList(), view = null)
        assertEquals(fleetDelta, AmuxJson.json.decodeFromString<FleetDelta>(AmuxJson.json.encodeToString(fleetDelta)))

        val startConfig = StartConfig(dataDir = "/tmp/amux", deviceName = "Pixel Test", lan = false)
        assertEquals(startConfig, AmuxJson.json.decodeFromString<StartConfig>(AmuxJson.json.encodeToString(startConfig)))

        val saidValues: List<Said> = listOf(
            Said.SignedOut,
            Said.UsageLimit(resetsAtMs = 1_700_000_000_000L),
            Said.Ask(key = "ask-1", wants = Wants.Run(command = "cargo test"), why = "run tests", more = 0),
        )
        for (said in saidValues) {
            val encoded = AmuxJson.json.encodeToString(said)
            assertEquals(said, AmuxJson.json.decodeFromString<Said>(encoded))
        }
    }

    @Test
    fun invalidStartConfigThrowsBridgeException() {
        try {
            NativeBridge.runtimeStart("not valid json")
            fail("expected BridgeException")
        } catch (expected: BridgeException) {
            assertTrue(expected.message.orEmpty().isNotBlank())
        }
    }

    @Test
    fun embeddedRuntimeCoreLoopAndFleetOpeningRoundTrip() {
        assumeFalse("wake_fd watcher requires Unix", isWindows)
        val tempDir = Files.createTempDirectory("amux-android-jni-test").toFile()
        try {
            val initialProfiles = CompletableFuture<List<ProfileView>>()
            CoreLoop(
                onProfilesMoved = { profiles ->
                    if (!initialProfiles.isDone && profiles.isNotEmpty()) {
                        initialProfiles.complete(profiles)
                    }
                },
            ).use { loop ->
                val config = StartConfig(
                    dataDir = tempDir.absolutePath,
                    deviceName = "Android JVM Test",
                    lan = false,
                    telemetry = false,
                    autoReports = false,
                )
                val runtimeHandle = loop.start(config)
                assertTrue("expected positive runtime handle", runtimeHandle > 0L)

                val profiles = initialProfiles.get(10, TimeUnit.SECONDS)
                assertEquals("fresh installation should host one default profile", 1, profiles.size)
                val defaultProfile = profiles.single()
                assertTrue(defaultProfile.id.isNotBlank())
                assertEquals(AccountBinding.Unbound, defaultProfile.account.binding)

                val profileHandle = loop.blocking {
                    NativeBridge.profileOpen(runtimeHandle, defaultProfile.id)
                }
                assertTrue("expected positive profile handle", profileHandle > 0L)
                try {
                    val fleetId = NativeBridge.profileFleetId(profileHandle)
                    assertTrue("expected positive fleet id", fleetId > 0L)

                    val openingJson = NativeBridge.fleetOpening(profileHandle)
                    assertNotNull("fleetOpening should return JSON", openingJson)
                    val fleetDelta = AmuxJson.json.decodeFromString<FleetDelta>(openingJson!!)
                    val hosts = fleetDelta.hosts
                    assertNotNull("fleetOpening should include hosts", hosts)
                    assertTrue(
                        "hosts should include local device",
                        hosts!!.any { it.name == "Android JVM Test" },
                    )
                    assertNotNull("fleetOpening should include FleetView", fleetDelta.view)

                    // Exercise an async act routed through wake_fd + CoreLoop watcher.
                    val rosterAnswerJson = loop.submitAct {
                        NativeBridge.profileRoster(profileHandle)
                    }.get(10, TimeUnit.SECONDS)
                    val rosterEnvelope = AmuxJson.json.parseToJsonElement(rosterAnswerJson).jsonObject
                    val ok = rosterEnvelope["Ok"]
                    assertNotNull("expected Ok roster answer, got $rosterAnswerJson", ok)
                    val roster = AmuxJson.json.decodeFromJsonElement<Roster>(ok!!)
                    assertEquals("Android JVM Test", roster.identity.name)
                    assertTrue("expected non-empty host id", roster.identity.hostId.isNotEmpty())
                    assertTrue("expected non-empty fingerprint", roster.identity.fingerprint.isNotBlank())
                    assertTrue("fresh profile should have no paired peers", roster.peers.isEmpty())
                } finally {
                    loop.blocking {
                        NativeBridge.profileClose(profileHandle)
                    }
                }
            }
        } finally {
            tempDir.deleteRecursively()
        }
    }

    @Test
    fun watchFleetReceivesWakeDrivenFleetDelta() {
        assumeFalse("wake_fd watcher requires Unix", isWindows)
        val tempDir = Files.createTempDirectory("amux-android-fleet-delta").toFile()
        try {
            val initialProfiles = CompletableFuture<List<ProfileView>>()
            CoreLoop(
                onProfilesMoved = { profiles ->
                    if (!initialProfiles.isDone && profiles.isNotEmpty()) {
                        initialProfiles.complete(profiles)
                    }
                },
            ).use { loop ->
                val runtimeHandle = loop.start(
                    StartConfig(
                        dataDir = tempDir.absolutePath,
                        deviceName = "Fleet Delta Test",
                        lan = false,
                        telemetry = false,
                        autoReports = false,
                    ),
                )
                val defaultProfile = initialProfiles.get(10, TimeUnit.SECONDS).single()
                val profileHandle = loop.blocking {
                    NativeBridge.profileOpen(runtimeHandle, defaultProfile.id)
                }
                val fleetId = NativeBridge.profileFleetId(profileHandle)
                assertNotNull("fleetOpening establishes delta baseline", NativeBridge.fleetOpening(profileHandle))
                val received = CompletableFuture<FleetDelta>()
                loop.watchFleet(fleetId) { event ->
                    if (
                        event is FleetEvent.Fleet &&
                        !received.isDone &&
                        event.delta.hosts?.any { it.name == "Desk Mac" && it.candidate && it.pairingOpen } == true
                    ) {
                        received.complete(event.delta)
                    }
                }

                // Feed a discovered LAN peer and prove the wake-fd watcher delivers a FleetEvent.Fleet delta.
                val foundJson = AmuxJson.json.encodeToString(
                    listOf(
                        Found(
                            hostId = List(16) { 7 },
                            name = "Desk Mac",
                            addrs = listOf("127.0.0.1:41999"),
                            scope = "",
                            pairingOpen = true,
                        ),
                    ),
                )
                loop.blocking {
                    NativeBridge.runtimeDiscovered(runtimeHandle, foundJson)
                }

                val delta = received.get(10, TimeUnit.SECONDS)
                val hosts = delta.hosts
                assertNotNull("wake-driven FleetEvent.Fleet should carry updated hosts", hosts)
                assertTrue(
                    "discovered host should appear in FleetEvent.Fleet delta",
                    hosts!!.any { it.name == "Desk Mac" && it.candidate && it.pairingOpen },
                )
            }
        } finally {
            tempDir.deleteRecursively()
        }
    }

    @Test
    fun submitActThrowingStartCompletesExceptionallyPromptlyAndPostStopReturnsStopped() {
        assumeFalse("wake_fd watcher requires Unix", isWindows)
        val tempDir = Files.createTempDirectory("amux-android-act-throw").toFile()
        try {
            val loop = CoreLoop()
            loop.use {
                it.start(
                    StartConfig(
                        dataDir = tempDir.absolutePath,
                        deviceName = "Act Throw Test",
                        lan = false,
                        telemetry = false,
                        autoReports = false,
                    ),
                )

                val failing = it.submitAct {
                    throw IllegalStateException("start boom")
                }
                try {
                    failing.get(5, TimeUnit.SECONDS)
                    fail("expected ExecutionException")
                } catch (expected: ExecutionException) {
                    assertTrue(
                        "expected IllegalStateException cause, got ${expected.cause}",
                        expected.cause is IllegalStateException,
                    )
                    assertEquals("start boom", expected.cause?.message)
                }
            }

            // After stop(), submitAct must resolve immediately with STOPPED_ANSWER instead of throwing RejectedExecutionException.
            val afterStop = loop.submitAct { 42L }.get(5, TimeUnit.SECONDS)
            assertEquals(CoreLoop.STOPPED_ANSWER, afterStop)
        } finally {
            tempDir.deleteRecursively()
        }
    }

    @Test
    fun asyncDispatchAppliesDeltaStrictlyBeforeActFutureCompletes() {
        assumeFalse("wake_fd watcher requires Unix", isWindows)
        val tempDir = Files.createTempDirectory("amux-android-delta-order").toFile()
        val uiExecutor = Executors.newSingleThreadExecutor { runnable ->
            Thread(runnable, "test-ui").apply { isDaemon = true }
        }
        try {
            val initialProfiles = CompletableFuture<List<ProfileView>>()
            val gateActive = AtomicBoolean(false)
            val dispatchQueued = CountDownLatch(1)
            val releaseDispatch = CountDownLatch(1)
            val events = CopyOnWriteArrayList<String>()
            val latestProfiles = AtomicReference<List<ProfileView>>(emptyList())

            CoreLoop(
                dispatch = { block ->
                    uiExecutor.execute {
                        if (gateActive.compareAndSet(true, false)) {
                            dispatchQueued.countDown()
                            releaseDispatch.await(5, TimeUnit.SECONDS)
                        }
                        block()
                    }
                },
                onProfilesMoved = { profiles ->
                    latestProfiles.set(profiles)
                    if (!initialProfiles.isDone && profiles.isNotEmpty()) {
                        initialProfiles.complete(profiles)
                    }
                    if (profiles.size == 2) {
                        events.add("profiles:2")
                    }
                },
            ).use { loop ->
                loop.start(
                    StartConfig(
                        dataDir = tempDir.absolutePath,
                        deviceName = "Delta Order Test",
                        lan = false,
                        telemetry = false,
                        autoReports = false,
                    ),
                )
                assertEquals(1, initialProfiles.get(10, TimeUnit.SECONDS).size)

                gateActive.set(true)
                val createFuture = loop.submitAct { handle ->
                    NativeBridge.runtimeCreateProfile(handle)
                }
                val observedOrder = createFuture.thenApply { json ->
                    events.add("answer")
                    latestProfiles.get().size to json
                }

                // Once the async UI dispatcher receives the step block (and pauses at the gate),
                // the act future MUST NOT have completed yet.
                assertTrue("expected step dispatch to arrive on UI thread", dispatchQueued.await(10, TimeUnit.SECONDS))
                assertFalse(
                    "act future must not complete before queued UI dispatch applies deltas",
                    createFuture.isDone,
                )

                releaseDispatch.countDown()
                val (profileCountAtAnswer, answerJson) = observedOrder.get(10, TimeUnit.SECONDS)
                assertEquals(2, profileCountAtAnswer)
                assertTrue("expected Ok answer for createProfile, got $answerJson", answerJson.contains("\"Ok\""))
                val firstProfiles2 = events.indexOf("profiles:2")
                val answerIndex = events.indexOf("answer")
                assertTrue("expected profiles:2 before answer, got $events", firstProfiles2 in 0 until answerIndex)
            }
        } finally {
            uiExecutor.shutdownNow()
            tempDir.deleteRecursively()
        }
    }

    @Test
    fun stopCalledInsideCoreCallbackCompletesPromptlyWithoutDeadlock() {
        assumeFalse("wake_fd watcher requires Unix", isWindows)
        val tempDir = Files.createTempDirectory("amux-android-core-stop").toFile()
        try {
            val initialProfiles = CompletableFuture<List<ProfileView>>()
            val stopDurationMs = CompletableFuture<Long>()
            val loopRef = AtomicReference<CoreLoop>()

            val loop = CoreLoop(
                onProfilesMoved = { profiles ->
                    if (!initialProfiles.isDone && profiles.isNotEmpty()) {
                        initialProfiles.complete(profiles)
                    }
                },
            )
            loopRef.set(loop)
            loop.use {
                val runtimeHandle = it.start(
                    StartConfig(
                        dataDir = tempDir.absolutePath,
                        deviceName = "Core Stop Test",
                        lan = false,
                        telemetry = false,
                        autoReports = false,
                    ),
                )
                val defaultProfile = initialProfiles.get(10, TimeUnit.SECONDS).single()
                val profileHandle = it.blocking {
                    NativeBridge.profileOpen(runtimeHandle, defaultProfile.id)
                }
                val fleetId = NativeBridge.profileFleetId(profileHandle)
                assertNotNull(NativeBridge.fleetOpening(profileHandle))

                it.watchFleet(fleetId) { event ->
                    if (event is FleetEvent.Fleet && !stopDurationMs.isDone) {
                        val startedAt = System.nanoTime()
                        loopRef.get().stop()
                        val elapsedMs = TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - startedAt)
                        stopDurationMs.complete(elapsedMs)
                    }
                }

                val foundJson = AmuxJson.json.encodeToString(
                    listOf(
                        Found(
                            hostId = List(16) { 9 },
                            name = "Stop Trigger Host",
                            addrs = listOf("127.0.0.1:41998"),
                            scope = "",
                            pairingOpen = true,
                        ),
                    ),
                )
                it.blocking {
                    NativeBridge.runtimeDiscovered(runtimeHandle, foundJson)
                }

                val elapsedMs = stopDurationMs.get(10, TimeUnit.SECONDS)
                assertTrue(
                    "stop() from inside a watcher-driven core callback should not hit the 5s watcher join timeout (took ${elapsedMs}ms)",
                    elapsedMs < 2_000L,
                )
            }
        } finally {
            tempDir.deleteRecursively()
        }
    }

    @Test
    fun watcherSurfacesCallbackErrorsThroughOnError() {
        assumeFalse("wake_fd watcher requires Unix", isWindows)
        val tempDir = Files.createTempDirectory("amux-android-watcher-error").toFile()
        try {
            val initialProfiles = CompletableFuture<List<ProfileView>>()
            val reportedError = CompletableFuture<Throwable>()
            CoreLoop(
                onProfilesMoved = { profiles ->
                    if (!initialProfiles.isDone && profiles.isNotEmpty()) {
                        initialProfiles.complete(profiles)
                    }
                },
                onError = { error ->
                    if (!reportedError.isDone) {
                        reportedError.complete(error)
                    }
                },
            ).use { loop ->
                val runtimeHandle = loop.start(
                    StartConfig(
                        dataDir = tempDir.absolutePath,
                        deviceName = "Watcher Error Test",
                        lan = false,
                        telemetry = false,
                        autoReports = false,
                    ),
                )
                val defaultProfile = initialProfiles.get(10, TimeUnit.SECONDS).single()
                val profileHandle = loop.blocking {
                    NativeBridge.profileOpen(runtimeHandle, defaultProfile.id)
                }
                val fleetId = NativeBridge.profileFleetId(profileHandle)
                assertNotNull(NativeBridge.fleetOpening(profileHandle))
                loop.watchFleet(fleetId) { event ->
                    if (event is FleetEvent.Fleet) {
                        throw IllegalStateException("listener failed")
                    }
                }

                val foundJson = AmuxJson.json.encodeToString(
                    listOf(
                        Found(
                            hostId = List(16) { 11 },
                            name = "Error Trigger Host",
                            addrs = listOf("127.0.0.1:41997"),
                            scope = "",
                            pairingOpen = true,
                        ),
                    ),
                )
                loop.blocking {
                    NativeBridge.runtimeDiscovered(runtimeHandle, foundJson)
                }

                val error = reportedError.get(10, TimeUnit.SECONDS)
                assertTrue("expected IllegalStateException, got $error", error is IllegalStateException)
                assertEquals("listener failed", error.message)
            }
        } finally {
            tempDir.deleteRecursively()
        }
    }
}
