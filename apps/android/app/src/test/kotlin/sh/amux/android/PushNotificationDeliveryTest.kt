package sh.amux.android

import android.Manifest
import android.app.NotificationManager
import java.io.File
import java.nio.file.Files
import java.util.Base64
import java.util.UUID
import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import sh.amux.core.runtime.AgentKey
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.NoOpNetworkWatcher
import sh.amux.core.runtime.StubBillingStoreFront
import sh.amux.push.Hpke
import sh.amux.push.InMemoryPushKeyStore
import sh.amux.push.OpenedPush
import sh.amux.push.PushKeyFailure
import sh.amux.push.PushPrivateKey
import sh.amux.push.Said
import sh.amux.shell.Route
import sh.amux.testsupport.ScriptedCloudService
import sh.amux.testsupport.ScriptedWebAuth

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class PushNotificationDeliveryTest {
    private val info = "amux push 1".toByteArray(Charsets.UTF_8)

    private fun grantNotificationPermission() {
        val app = RuntimeEnvironment.getApplication()
        shadowOf(app).grantPermissions(Manifest.permission.POST_NOTIFICATIONS)
    }

    private fun seal(push: OpenedPush, recipientPublicKey: ByteArray): String {
        val plaintext = Json.encodeToString(OpenedPush.serializer(), push).toByteArray(Charsets.UTF_8)
        val sealedBytes = Hpke.sealBase(
            recipientPublicKey = recipientPublicKey,
            info = info,
            plaintext = plaintext,
        )
        return Base64.getEncoder().encodeToString(sealedBytes)
    }

    @Test
    fun decryptsSealedPushAndDeduplicatesRetriesByPushId() {
        grantNotificationPermission()
        val app = RuntimeEnvironment.getApplication()
        val manager = app.getSystemService(NotificationManager::class.java)
        val shadowManager = shadowOf(manager)

        val host = "11111111-2222-3333-4444-555555555555"
        val agent = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        val key = PushPrivateKey.generate()
        val keys = InMemoryPushKeyStore(mapOf(host to key))

        val opened = OpenedPush(
            host = host,
            agent = agent,
            name = "Worker",
            notice = Said.SignedOut,
        )
        val data = mapOf(
            "to" to host,
            "sealed" to seal(opened, key.publicKey),
            "id" to "0123456789abcdef0123456789abcdef",
        )

        val first = PushNotificationDelivery.deliver(app, data, keys)
        val retry = PushNotificationDelivery.deliver(app, data, keys)

        assertEquals(PushNotificationDelivery.Outcome.Posted, first)
        assertEquals(PushNotificationDelivery.Outcome.Posted, retry)
        assertEquals(1, shadowManager.allNotifications.size)

        val posted = shadowManager.allNotifications.single()
        assertEquals("Worker", posted.extras.getString("android.title"))
        assertEquals(host, posted.extras.getString(PushNotificationDelivery.EXTRA_PUSH_HOST))
        assertEquals(agent, posted.extras.getString(PushNotificationDelivery.EXTRA_PUSH_AGENT))
        assertNotNull(posted.contentIntent)
    }

    @Test
    fun fallsBackToPlainAlertWhenKeyCannotDecrypt() {
        grantNotificationPermission()
        val app = RuntimeEnvironment.getApplication()
        val manager = app.getSystemService(NotificationManager::class.java)
        val shadowManager = shadowOf(manager)

        val outcome = PushNotificationDelivery.deliver(
            context = app,
            data = mapOf(
                "to" to "11111111-2222-3333-4444-555555555555",
                "sealed" to "bm90LXZhbGlkLWhwa2U=",
                "id" to "fedcba9876543210fedcba9876543210",
            ),
            keys = InMemoryPushKeyStore(),
        )

        assertEquals(PushNotificationDelivery.Outcome.PostedFallback, outcome)
        val posted = shadowManager.allNotifications.single()
        assertEquals("amux", posted.extras.getString("android.title"))
        assertEquals("An agent needs you", posted.extras.getString("android.text"))
    }

    @Test
    fun suppressesNotificationWhenUserIsAlreadyViewingThatChat() {
        grantNotificationPermission()
        val app = RuntimeEnvironment.getApplication()
        val manager = app.getSystemService(NotificationManager::class.java)
        val shadowManager = shadowOf(manager)

        val host = "11111111-2222-3333-4444-555555555555"
        val agent = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        val key = PushPrivateKey.generate()
        val keys = InMemoryPushKeyStore(mapOf(host to key))

        val outcome = PushNotificationDelivery.deliver(
            context = app,
            data = mapOf(
                "to" to host,
                "sealed" to seal(OpenedPush(host, agent, "Worker", Said.SignedOut), key.publicKey),
                "id" to "11111111111111111111111111111111",
            ),
            keys = keys,
            isViewingChat = { h, a -> h == host && a == agent },
        )

        assertEquals(PushNotificationDelivery.Outcome.SuppressedViewingChat, outcome)
        assertTrue(shadowManager.allNotifications.isEmpty())
    }

    @Test
    fun androidPushKeyStoreRoundTripsAndRemovesKeysWithInjectedCipher() {
        val dir = Files.createTempDirectory("amux-push-keys-test").toFile()
        try {
            val xorCipher = object : AndroidPushKeyStore.KeyCipher {
                override fun encrypt(plaintext: ByteArray): ByteArray =
                    ByteArray(plaintext.size) { i -> (plaintext[i].toInt() xor 0x5A).toByte() }

                override fun decrypt(blob: ByteArray): ByteArray =
                    ByteArray(blob.size) { i -> (blob[i].toInt() xor 0x5A).toByte() }
            }
            val store = AndroidPushKeyStore(dir, xorCipher)
            val host = "11111111-2222-3333-4444-555555555555"
            val created = store.makeKey(host)

            assertEquals(created, store.key(host.uppercase()))
            store.removeKey(host)
            assertEquals(null, store.key(host))

            // Corrupted ciphertext must raise PushKeyFailure rather than returning null.
            File(dir, "$host.key").writeBytes(byteArrayOf(1, 2, 3))
            val failed = runCatching { store.key(host) }.exceptionOrNull()
            assertTrue(failed is PushKeyFailure)
        } finally {
            dir.deleteRecursively()
        }
    }

    @Test
    fun notificationTapRoutesToConversationOrFleet() {
        val tempRoot = Files.createTempDirectory("amux-tap-test").toFile()
        val app = RuntimeEnvironment.getApplication()
        val comp = Composition(
            context = app,
            overrides = CompositionOverrides(
                cloud = ScriptedCloudService(),
                billing = StubBillingStoreFront(),
                authPresenter = ScriptedWebAuth(),
                useSystemDiscovery = false,
                useSystemPush = false,
                networkWatch = NoOpNetworkWatcher,
                autoStart = false,
                attachProcessLifecycle = false,
                installationDir = File(tempRoot, "install"),
                accountsFile = File(tempRoot, "accounts.json"),
                prefsName = "amux_tap_${tempRoot.name}",
            ),
        )
        try {
            val host = "11111111-2222-3333-4444-555555555555"
            val agent = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
            comp.handleNotificationTap(host = host, agent = agent)
            shadowOf(app.mainLooper).idle()

            val expected = Route.Conversation(
                AgentKey(HostId.parse(host)!!, UUID.fromString(agent)),
            )
            assertEquals(expected, comp.router.snapshot.topRoute)
            assertTrue(comp.isViewingChat(host, agent))

            comp.handleNotificationTap(host = null, agent = null)
            shadowOf(app.mainLooper).idle()
            assertEquals(null, comp.router.snapshot.topRoute)
        } finally {
            comp.shutdown()
            tempRoot.deleteRecursively()
        }
    }
}
