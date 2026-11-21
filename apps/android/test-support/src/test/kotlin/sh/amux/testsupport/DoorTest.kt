package sh.amux.testsupport

import kotlinx.serialization.SerializationException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class DoorTest {
    @Test
    fun roundTripsAllRequestVariants() {
        val requests = listOf(
            DoorRequest.Ping,
            DoorRequest.Ready,
            DoorRequest.Query,
            DoorRequest.Tap(identifier = "tab.hosts"),
            DoorRequest.Tap(identifier = "pairing.confirm", after = 0.25),
            DoorRequest.Type(identifier = "pairing.digits", text = "123456"),
            DoorRequest.Back,
            DoorRequest.PairByCode(host = "d6e6b93c-114e-4d20-b29a-6fac10bfb78d", pin = "418207"),
            DoorRequest.Pair(qr = "https://amux.sh/p#abc"),
            DoorRequest.Discovered(
                found = listOf(
                    DoorFound(
                        hostId = "d6e6b93c-114e-4d20-b29a-6fac10bfb78d",
                        name = "desk",
                        addrs = listOf("10.0.2.2:53990"),
                        scope = "phone-reach",
                        pairingOpen = true,
                    ),
                ),
            ),
            DoorRequest.Open(screen = "hosts"),
            DoorRequest.Open(screen = "conversation", fixture = "desk-work"),
            DoorRequest.Settle,
            DoorRequest.Capture(),
            DoorRequest.Capture(path = "screen.png"),
            DoorRequest.Connect(
                relay = "http://10.0.2.2:53212",
                token = "refresh-ada",
                user = "ada",
                tier = "pro",
            ),
            DoorRequest.Cloud(
                cloud = CloudScript(
                    signIn = "refused",
                    reason = "Account locked.",
                    entitlement = "none",
                    token = null,
                ),
            ),
            DoorRequest.Appearance("dark"),
            DoorRequest.FontScale(1.3f),
            DoorRequest.AwaitReconciled(15.0),
            DoorRequest.AwaitAgent(agent = "desk-work", seconds = 30.0),
            DoorRequest.Shutdown,
        )

        for (request in requests) {
            val encoded = DoorCodec.encodeRequest(request)
            assertTrue("NDJSON line must not contain newlines: $encoded", '\n' !in encoded)
            val decoded = DoorCodec.decodeRequest(encoded)
            assertEquals("round-trip mismatch for $request", request, decoded)
        }
    }

    @Test
    fun acceptsShortAliasesInRequests() {
        assertEquals(
            DoorRequest.Tap(identifier = "hosts.row.desk"),
            DoorCodec.decodeRequest("""{"kind":"tap","id":"hosts.row.desk"}"""),
        )
        assertEquals(
            DoorRequest.Type(identifier = "pairing.digits", text = "654321"),
            DoorCodec.decodeRequest("""{"kind":"type","id":"pairing.digits","text":"654321"}"""),
        )
        assertEquals(
            DoorRequest.PairByCode(host = "desk", pin = "111222"),
            DoorCodec.decodeRequest("""{"kind":"pairByCode","host":"desk","code":"111222"}"""),
        )
        assertEquals(
            DoorRequest.Pair(qr = "https://amux.sh/p#xyz"),
            DoorCodec.decodeRequest("""{"kind":"pair","link":"https://amux.sh/p#xyz"}"""),
        )
        assertEquals(
            DoorRequest.Open(screen = "hosts"),
            DoorCodec.decodeRequest("""{"kind":"open","route":"hosts"}"""),
        )
    }

    @Test
    fun roundTripsAllReplyVariants() {
        val replies = listOf(
            DoorReply.Ack,
            DoorReply.Ready(pid = 4242, reconciled = true),
            DoorReply.State(
                VisibleState(
                    screen = "hosts",
                    fontScale = 1.0f,
                    appearance = "light",
                    elements = listOf(
                        VisibleElement(
                            identifier = "hosts.row.desk",
                            label = "desk, macOS · On this network",
                            value = null,
                            frame = VisibleFrame(x = 16.0, y = 120.0, width = 380.0, height = 56.0),
                            enabled = true,
                            selected = false,
                        ),
                    ),
                    reconciled = true,
                    unconfirmed = 0,
                ),
            ),
            DoorReply.Captured(
                path = "/data/user/0/sh.amux.android/cache/door/screen.png",
                width = 1080,
                height = 2424,
                scale = 2.625f,
            ),
            DoorReply.Captured(
                path = null,
                width = 1080,
                height = 2424,
                scale = 2.625f,
                base64 = "iVBORw0KGgo=",
            ),
            DoorReply.Paired(host = "desk"),
            DoorReply.Error(message = "no element named missing"),
        )

        for (reply in replies) {
            val encoded = DoorCodec.encodeReply(reply)
            assertTrue("NDJSON reply must not contain newlines: $encoded", '\n' !in encoded)
            val decoded = DoorCodec.decodeReply(encoded)
            assertEquals("round-trip mismatch for $reply", reply, decoded)
        }
    }

    @Test
    fun rejectsUnknownRequestKind() {
        try {
            DoorCodec.decodeRequest("""{"kind":"doesNotExist"}""")
            fail("expected SerializationException")
        } catch (expected: SerializationException) {
            assertTrue(expected.message.orEmpty().contains("doesNotExist"))
        }
    }

    @Test
    fun roundTripsStartConfig() {
        val config = DoorStartConfig(
            dataDir = "door-run",
            deviceName = "amux-android-1",
            discoveryScope = "phone-reach",
            lan = true,
            lanBind = "0.0.0.0:0",
            relayTcp = "10.0.2.2:53213",
            relayQuic = "10.0.2.2:53214",
            relayRoot = "3082",
            scriptedCloud = true,
            useSystemDiscovery = false,
            freshStart = true,
        )
        val encoded = DoorCodec.encodeStartConfig(config)
        val decoded = DoorCodec.decodeStartConfig(encoded)
        assertEquals(config, decoded)
    }
}
