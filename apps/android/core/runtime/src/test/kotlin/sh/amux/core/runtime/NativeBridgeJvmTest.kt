package sh.amux.core.runtime

import java.nio.file.Files
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import sh.amux.push.Said
import sh.amux.push.Wants
import sh.amux.values.AccountBinding
import sh.amux.values.AgentAct
import sh.amux.values.AgentNameProblem
import sh.amux.values.Catalogue
import sh.amux.values.ChatDelta
import sh.amux.values.FleetDelta
import sh.amux.values.NewAgentChoices
import sh.amux.values.NewAgentPick
import sh.amux.values.OfferedModel
import sh.amux.values.ProfileView
import sh.amux.values.Roster
import sh.amux.values.StartConfig

class NativeBridgeJvmTest {
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
                assertTrue("expected valid wake fd", NativeBridge.runtimeWakeFd(runtimeHandle) >= 0)

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
}
