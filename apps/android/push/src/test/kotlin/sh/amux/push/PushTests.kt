package sh.amux.push

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant
import java.time.ZoneId
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Base64
import java.util.Locale

class SealedPushTests {
    @Serializable
    private data class Fixture(
        @SerialName("private_key") val privateKey: String,
        val cases: List<Case>,
    ) {
        @Serializable
        data class Case(
            val name: String,
            val sealed: String,
            val plain: JsonElement,
        )
    }

    private val json = Json { ignoreUnknownKeys = false }

    private fun loadFixture(): Fixture {
        val stream = requireNotNull(
            javaClass.classLoader.getResourceAsStream("sealed-pushes.json"),
        ) { "missing sealed-pushes.json" }
        return stream.bufferedReader(Charsets.UTF_8).use { reader ->
            json.decodeFromString(Fixture.serializer(), reader.readText())
        }
    }

    private fun fixtureKey(fixture: Fixture): PushPrivateKey =
        PushPrivateKey(Base64.getDecoder().decode(fixture.privateKey))

    @Test
    fun everyCaseTheDaemonSealedOpensToTheJsonItWrote() {
        val fixture = loadFixture()
        val key = fixtureKey(fixture)
        assertFalse(fixture.cases.isEmpty())
        for (sample in fixture.cases) {
            val opened = SealedPush.plaintext(sample.sealed, key)
            assertNotNull(sample.name, opened)
            val parsed = json.parseToJsonElement(opened!!.toString(Charsets.UTF_8))
            assertEquals(sample.name, sample.plain, parsed)
        }
    }

    @Test
    fun everyCaseDecodes() {
        val fixture = loadFixture()
        val key = fixtureKey(fixture)
        for (sample in fixture.cases) {
            val opened = SealedPush.open(sample.sealed, key)
            assertNotNull(sample.name, opened)
            assertEquals(sample.name, "11111111-1111-1111-1111-111111111111", opened!!.host)
            assertEquals(sample.name, "22222222-2222-2222-2222-222222222222", opened.agent)
            assertEquals(sample.name, "refactor-auth", opened.name)
        }
    }

    @Test
    fun eachNoticeSaysWhatTheFleetRowSays() {
        val fixture = loadFixture()
        val key = fixtureKey(fixture)
        val zone = ZoneOffset.UTC
        val locale = Locale.US
        val now = Instant.ofEpochSecond(1_800_000_000L - 3_600L)
        val reset = DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT)
            .withLocale(locale)
            .format(Instant.ofEpochSecond(1_800_000_000L).atZone(zone))
        val expected = mapOf(
            "run" to "Wants to run deploy --prod",
            "run and more" to "Wants to run deploy --prod · 2 more",
            "change one file" to "Wants to edit src/lib.rs",
            "change many files" to "Wants to edit 4 files",
            "use a tool" to "Wants to use github create_issue",
            "question" to "Which suite?",
            "plan with a title" to "Has a plan for you to decide: Move the cache",
            "plan without one" to "Has a plan for you to decide",
            "the provider's sentence" to "Sign in to Linear to go on · 1 more",
            "signed out" to "Signed out of its provider · sign in on its device",
            "usage limit with a reset" to "Usage limit reached · resets $reset",
            "usage limit without one" to "Usage limit reached",
            "failed" to "Connection refused",
            "died" to "Stopped unexpectedly: exited with status 1",
        )
        assertEquals(expected.keys, fixture.cases.map { it.name }.toSet())
        for (sample in fixture.cases) {
            val opened = SealedPush.open(sample.sealed, key)
            assertNotNull(sample.name, opened)
            assertEquals(
                sample.name,
                expected[sample.name],
                PushWords.body(opened!!.notice, now = now, zone = zone, locale = locale),
            )
        }
    }

    @Test
    fun aKeyItWasNotSealedToOpensNothing() {
        val sample = loadFixture().cases.first()
        assertNull(SealedPush.open(sample.sealed, PushPrivateKey.generate()))
    }

    @Test
    fun whatIsNotSealedOpensNothing() {
        val key = fixtureKey(loadFixture())
        assertNull(SealedPush.open("not base64!", key))
        assertNull(SealedPush.open(Base64.getEncoder().encodeToString(ByteArray(16) { 1 }), key))
        assertNull(SealedPush.open(Base64.getEncoder().encodeToString(ByteArray(80) { 1 }), key))
        // Low-order X25519 encapsulated key (all zeros) must be rejected.
        assertNull(SealedPush.open(Base64.getEncoder().encodeToString(ByteArray(64)), key))
    }

    @Test
    fun aPayloadOpensWithTheKeyOfTheProfileItNames() {
        val fixture = loadFixture()
        val keys = InMemoryPushKeyStore(
            mapOf("11111111-aaaa-1111-1111-111111111111" to fixtureKey(fixture)),
        )
        val sealed = fixture.cases.first().sealed
        fun payload(to: String): Map<String, Any?> =
            mapOf("aps" to emptyMap<String, Any>(), "amux" to mapOf("to" to to, "sealed" to sealed))

        assertNotNull(
            "a host id's case is not part of it",
            SealedPush.open(payload("11111111-AAAA-1111-1111-111111111111"), keys),
        )
        assertNull(SealedPush.open(payload("33333333-3333-3333-3333-333333333333"), keys))
        assertNull(SealedPush.open(mapOf("aps" to emptyMap<String, Any>()), keys))
    }

    @Test
    fun aKeyThatCannotBeReadLeavesThePushClosed() {
        val sealed = loadFixture().cases.first().sealed
        val payload = mapOf("amux" to mapOf("to" to "any", "sealed" to sealed))
        val locked = object : PushKeyStore {
            override fun key(host: String): PushPrivateKey? = throw PushKeyFailure("keystore locked")
            override fun makeKey(host: String): PushPrivateKey = throw PushKeyFailure("keystore locked")
            override fun removeKey(host: String) = throw PushKeyFailure("keystore locked")
        }
        assertNull(SealedPush.open(payload, locked))
    }

    @Test
    fun hpkeSealAndOpenRoundTripWithGeneratedKeys() {
        val store = InMemoryPushKeyStore()
        val key = store.makeKey("HOST-1")
        assertEquals(key, store.key("host-1"))
        val plain = """{"host":"h","agent":"a","name":"demo","notice":"SignedOut"}"""
        val sealedBytes = Hpke.sealBase(
            recipientPublicKey = key.publicKey,
            info = "amux push 1".toByteArray(Charsets.UTF_8),
            plaintext = plain.toByteArray(Charsets.UTF_8),
        )
        val sealedB64 = Base64.getEncoder().encodeToString(sealedBytes)
        val opened = SealedPush.openForProfile("host-1", sealedB64, store)
        assertEquals(OpenedPush("h", "a", "demo", Said.SignedOut), opened)

        // Flipping a byte in ciphertext invalidates Poly1305 authentication.
        val tampered = sealedBytes.copyOf()
        tampered[tampered.lastIndex] = (tampered[tampered.lastIndex].toInt() xor 0x01).toByte()
        assertNull(SealedPush.open(Base64.getEncoder().encodeToString(tampered), key))

        // Replacing or removing keys works as specified.
        val replaced = store.makeKey("host-1")
        assertNotEquals(key, replaced)
        assertNull(SealedPush.openForProfile("host-1", sealedB64, store))
        store.removeKey("HOST-1")
        assertNull(store.key("host-1"))
        store.removeKey("never-existed")
    }

    @Test
    fun privateKeyMakesDefensiveCopiesAndRedactsToString() {
        val raw = ByteArray(32) { 0x41 }
        val key = PushPrivateKey(raw)
        raw[0] = 0x00
        assertArrayEquals(ByteArray(32) { 0x41 }, key.rawRepresentation)
        val copy = key.rawRepresentation
        copy[0] = 0x02
        assertArrayEquals(ByteArray(32) { 0x41 }, key.rawRepresentation)
        assertFalse(key.toString().contains("65"))
    }
}

class PushAlertTests {
    private val opened = OpenedPush(
        host = "11111111-1111-1111-1111-111111111111",
        agent = "22222222-2222-2222-2222-222222222222",
        name = "refactor-auth",
        notice = Said.Ask(key = "k", wants = Wants.Run(command = "deploy"), why = "", more = 0),
    )

    @Test
    fun anOpenedPushBecomesTheAgentsNameOverWhatItWants() {
        val alert = PushAlert.from(opened)
        assertEquals("refactor-auth", alert.title)
        assertEquals("Wants to run deploy", alert.body)
        assertEquals(
            "11111111-1111-1111-1111-111111111111/22222222-2222-2222-2222-222222222222",
            alert.threadId,
        )
        assertEquals(opened.host, alert.extras["host"])
        assertEquals(opened.agent, alert.extras["agent"])
        assertNull(alert.extras["sealed"])
    }

    @Test
    fun anAgentWithNoNameKeepsTheFallbackTitle() {
        val alert = PushAlert.from(opened.copy(name = ""), fallbackTitle = "amux")
        assertEquals("amux", alert.title)
    }

    @Test
    fun aNotificationGivesTheProvidersReasonBelowWhatItWants() {
        val alert = PushAlert.from(
            OpenedPush(
                host = "h",
                agent = "a",
                name = "n",
                notice = Said.Ask(
                    key = "k",
                    wants = Wants.Run(command = "rm -rf target"),
                    why = "Deletes files\nmore",
                    more = 0,
                ),
            ),
        )
        assertEquals("Wants to run rm -rf target\nDeletes files", alert.body)
    }
}

class PushWordsTests {
    @Test
    fun anAskWithNothingToSayStillSaysItNeedsYou() {
        assertEquals(
            "Needs you",
            PushWords.body(Said.Ask(key = "k", wants = Wants.Other(server = "", text = ""), why = "", more = 0)),
        )
        assertEquals(
            "Wants to run ls",
            PushWords.body(Said.Ask(key = "k", wants = Wants.Run(command = "ls"), why = "", more = 0)),
        )
    }

    @Test
    fun aRowNamesTheProviderAndAPushCannot() {
        assertEquals("Signed out of Codex", PushWords.body(Said.SignedOut, provider = "Codex"))
        assertEquals("Signed out of its provider · sign in on its device", PushWords.body(Said.SignedOut))
    }

    @Test
    fun aLimitAlreadyResetSaysNoTime() {
        val now = Instant.ofEpochSecond(1_000)
        assertEquals("Usage limit reached", PushWords.body(Said.UsageLimit(resetsAtMs = 500_000L), now = now))
    }

    @Test
    fun aLimitDaysAwaySaysItsWeekday() {
        val zone: ZoneId = ZoneOffset.UTC
        val locale = Locale.US
        val now = Instant.ofEpochSecond(1_800_000_000L)
        val later = now.plusSeconds(3L * 86_400L)
        val body = PushWords.body(
            Said.UsageLimit(resetsAtMs = later.toEpochMilli()),
            now = now,
            zone = zone,
            locale = locale,
        )
        val weekday = DateTimeFormatter.ofPattern("EEE", locale).format(later.atZone(zone))
        assertEquals("Usage limit reached · resets $weekday", body)
    }

    @Test
    fun aFailureAndADeathWithNoWordsStillSaySomething() {
        assertEquals("Its turn failed", PushWords.body(Said.Failed(message = "")))
        assertEquals("Stopped unexpectedly", PushWords.body(Said.Died(detail = "")))
    }

    @Test
    fun accessAndChangeVariantsAreWordedClearly() {
        assertEquals(
            "Wants access: Needs network access",
            PushWords.body(Said.Ask("k", Wants.Access("Needs network access\nmore"), "", 0)),
        )
        assertEquals(
            "Wants to create src/new.kt",
            PushWords.body(Said.Ask("k", Wants.Change("src/new.kt", 1, created = true), "", 0)),
        )
        assertEquals(
            "Wants to write src/file.kt",
            PushWords.body(Said.Ask("k", Wants.Change("src/file.kt", 1, created = null), "", 0)),
        )
        assertTrue(PushWords.reason(Said.SignedOut) == null)
    }
}
