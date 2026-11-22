package sh.amux.core.runtime

import java.util.UUID
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.push.InMemoryPushKeyStore
import sh.amux.push.PushKeyFailure
import sh.amux.push.PushKeyStore
import sh.amux.push.PushPrivateKey

class FcmPushRegistrarTest {
    private class RecordingCloud : CloudService {
        val registered = mutableListOf<Triple<AccountId, HostId, PushDevice>>()
        val removed = mutableListOf<Pair<AccountId, HostId>>()
        var failRegister: Boolean = false

        override suspend fun signIn(intent: SignInIntent, presenter: WebAuthPresenter): SignedInAccount =
            error("unused")

        override suspend fun handOver(id: AccountId): Handover? = null
        override suspend fun forgetSession(id: AccountId) = Unit
        override suspend fun lend(lender: suspend (AccountId) -> String?) = Unit
        override suspend fun entitlement(id: AccountId): Entitlement = Entitlement.None
        override suspend fun recordPurchase(id: AccountId, signedTransaction: String) = Unit
        override suspend fun requestDeletion(id: AccountId, confirmedEmail: String): DeletionOutcome =
            DeletionOutcome.Deleted

        override suspend fun registerDevice(id: AccountId, host: HostId, device: PushDevice) {
            if (failRegister) throw CloudException(CloudError.Network("offline"))
            registered.add(Triple(id, host, device))
        }

        override suspend fun removeDevice(id: AccountId, host: HostId) {
            removed.add(id to host)
        }
    }

    private val account = AccountId("sub-1")
    private val hostId = HostId(UUID.fromString("11111111-2222-3333-4444-555555555555"))

    @Test
    fun createsKeyAdvertisesPublicKeyAndRegistersFcmTokenWhenWantedAndPermitted() = runTest {
        val keys = InMemoryPushKeyStore()
        val cloud = RecordingCloud()
        val said = mutableListOf<Pair<String, List<Int>?>>()

        val registrar = FcmPushRegistrar(
            keys = keys,
            cloud = cloud,
            environment = PushDevice.Environment.Production,
            scope = this,
            host = { hostId },
            say = { profile, pub ->
                said.add(profile to pub)
                true
            },
        )

        registrar.updateAccounts(listOf(PushAccount(account, "prof-1", wanted = true)))
        registrar.updatePermitted(true)
        registrar.updateToken("fcm-token:abc123".toByteArray(Charsets.UTF_8))
        registrar.settled()

        val created = keys.key(hostId.toString())
        assertNotNull(created)
        assertEquals(1, said.size)
        assertEquals("prof-1", said.single().first)
        assertEquals(created!!.publicKey.map { it.toInt() and 0xFF }, said.single().second)

        val (regAccount, regHost, regDevice) = cloud.registered.single()
        assertEquals(account, regAccount)
        assertEquals(hostId, regHost)
        assertEquals("fcm", regDevice.transport)
        assertEquals(PushDevice.Environment.Production, regDevice.environment)
        assertEquals("fcm-token:abc123", regDevice.token)
    }

    @Test
    fun replaysHeldKeyWhileEntitlementIsStillUnknown() = runTest {
        val existing = PushPrivateKey.generate()
        val keys = InMemoryPushKeyStore(mapOf(hostId.toString() to existing))
        val cloud = RecordingCloud()
        val said = mutableListOf<Pair<String, List<Int>?>>()

        val registrar = FcmPushRegistrar(
            keys = keys,
            cloud = cloud,
            environment = PushDevice.Environment.Production,
            scope = this,
            host = { hostId },
            say = { profile, pub ->
                said.add(profile to pub)
                true
            },
        )

        registrar.updateAccounts(listOf(PushAccount(account, "prof-1", wanted = null)))
        registrar.settled()

        assertEquals(existing.publicKey.map { it.toInt() and 0xFF }, said.single().second)
        assertTrue(cloud.registered.isEmpty())
    }

    @Test
    fun withdrawRemovesKeyUnsaysRuntimeAndDeletesDeviceRow() = runTest {
        val keys = InMemoryPushKeyStore()
        val cloud = RecordingCloud()
        val said = mutableListOf<Pair<String, List<Int>?>>()

        val registrar = FcmPushRegistrar(
            keys = keys,
            cloud = cloud,
            environment = PushDevice.Environment.Production,
            scope = this,
            host = { hostId },
            say = { profile, pub ->
                said.add(profile to pub)
                true
            },
        )

        registrar.updateAccounts(listOf(PushAccount(account, "prof-1", wanted = true)))
        registrar.updatePermitted(true)
        registrar.updateToken("fcm-tok-1".toByteArray(Charsets.UTF_8))
        registrar.settled()

        registrar.withdraw(account)

        assertNull(keys.key(hostId.toString()))
        assertNull(said.last().second)
        assertEquals(listOf(account to hostId), cloud.removed)
    }

    @Test
    fun transientKeystoreFailureDoesNotOverwriteOrWithdraw() = runTest {
        val failingKeys = object : PushKeyStore {
            var makeCalled = false
            var removeCalled = false

            override fun key(host: String): PushPrivateKey? {
                throw PushKeyFailure("locked before first unlock")
            }

            override fun makeKey(host: String): PushPrivateKey {
                makeCalled = true
                return PushPrivateKey.generate()
            }

            override fun removeKey(host: String) {
                removeCalled = true
            }
        }
        val cloud = RecordingCloud()
        val registrar = FcmPushRegistrar(
            keys = failingKeys,
            cloud = cloud,
            environment = PushDevice.Environment.Production,
            scope = this,
            host = { hostId },
            say = { _, _ -> true },
        )

        registrar.updateAccounts(listOf(PushAccount(account, "prof-1", wanted = true)))
        registrar.updatePermitted(true)
        registrar.updateToken("fcm-tok-1".toByteArray(Charsets.UTF_8))
        registrar.settled()

        assertEquals(false, failingKeys.makeCalled)
        assertEquals(false, failingKeys.removeCalled)
        assertTrue(cloud.registered.isEmpty())
    }

    @Test
    fun tokenRotationReRegistersWithoutReplacingKey() = runTest {
        val keys = InMemoryPushKeyStore()
        val cloud = RecordingCloud()
        val registrar = FcmPushRegistrar(
            keys = keys,
            cloud = cloud,
            environment = PushDevice.Environment.Sandbox,
            scope = this,
            host = { hostId },
            say = { _, _ -> true },
        )

        registrar.updateAccounts(listOf(PushAccount(account, "prof-1", wanted = true)))
        registrar.updatePermitted(true)
        registrar.updateToken("fcm-tok-1".toByteArray(Charsets.UTF_8))
        registrar.settled()
        val firstKey = keys.key(hostId.toString())

        registrar.updateToken("fcm-tok-2".toByteArray(Charsets.UTF_8))
        registrar.settled()

        assertEquals(firstKey, keys.key(hostId.toString()))
        assertEquals(listOf("fcm-tok-1", "fcm-tok-2"), cloud.registered.map { it.third.token })
    }

    @Test
    fun aPassThatFinishesWhereItStartsLeavesNothingToWaitFor() {
        // The app's scope dispatches immediately on the main thread, so a
        // pass with nothing to suspend on runs to its end inside settle();
        // waiting for it afterwards must return, as signing out waits on it.
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        val registrar = FcmPushRegistrar(
            keys = InMemoryPushKeyStore(),
            cloud = RecordingCloud(),
            environment = PushDevice.Environment.Production,
            scope = scope,
            host = { null },
            say = { _, _ -> true },
        )
        try {
            runBlocking {
                withTimeout(5_000L) {
                    registrar.updateAccounts(listOf(PushAccount(account, "prof-1", wanted = false)))
                    registrar.withdraw(account)
                    registrar.settled()
                }
            }
        } finally {
            scope.cancel()
        }
    }
}
