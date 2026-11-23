package sh.amux.core.runtime

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import sh.amux.push.PushKeyFailure
import sh.amux.push.PushKeyStore
import sh.amux.push.PushPrivateKey

/**
 * Keeps each profile's FCM push notifications as they should be: an X25519 key
 * in [keys], its public key advertised to the embedded runtime via [say], and
 * a device row at `amux.sh` via [cloud] (`transport = "fcm"`), or none of the
 * three.
 *
 * Mirrors iOS `PushRegistrar.swift`: a key held in [keys] says a profile was
 * registered across launches, so what is already held is advertised to the
 * runtime even while entitlement is still unknown (`wanted == null`), preventing
 * paired machines from forgetting the key on launch. Every failure is retried
 * on the next foreground resume (`updatePermitted`) or account/token update.
 *
 * Confined to [scope]'s main dispatcher.
 */
class FcmPushRegistrar(
    private val keys: PushKeyStore,
    private val cloud: CloudService,
    private val environment: PushDevice.Environment,
    private val scope: CoroutineScope,
    private val host: suspend (String) -> HostId?,
    private val say: suspend (String, List<Int>?) -> Boolean,
) : PushRegistrar {
    private var accounts: List<PushAccount> = emptyList()
    private var permitted: Boolean? = null
    private var token: String? = null
    private val leaving = mutableSetOf<AccountId>()
    private val said = mutableMapOf<String, List<Int>>()
    private val registered = mutableMapOf<String, String>()
    private val hosts = mutableMapOf<String, HostId>()

    private var settling: Job? = null
    private var unsettled: Boolean = false

    override fun updateAccounts(accounts: List<PushAccount>) {
        val changed = accounts != this.accounts
        this.accounts = accounts
        leaving.retainAll { id ->
            accounts.any { it.account == id && it.wanted != false }
        }
        if (changed || hasPendingWork()) {
            settle()
        }
    }

    override fun updatePermitted(permitted: Boolean) {
        this.permitted = permitted
        settle()
    }

    override fun updateToken(token: ByteArray) {
        val decoded = token.toString(Charsets.UTF_8).trim()
        if (decoded.isEmpty() || decoded == this.token) return
        this.token = decoded
        settle()
    }

    override suspend fun withdraw(account: AccountId) {
        leaving.add(account)
        settle()
        settled()
    }

    override suspend fun settled() {
        while (true) {
            val active = settling ?: return
            active.join()
        }
    }

    private fun hasPendingWork(): Boolean {
        val currentToken = token
        return accounts.any { account ->
            when (wanted(account)) {
                true -> said[account.profile] == null ||
                    (currentToken != null && registered[account.profile] != currentToken)
                false -> said[account.profile] != null || registered[account.profile] != null
                null -> said[account.profile] == null
            }
        }
    }

    private fun settle() {
        unsettled = true
        if (settling != null) return
        // Started only once it is held in [settling]: on an immediate main
        // dispatcher a pass with nothing to wait for runs to its end inside
        // launch, and its finally would clear [settling] before the
        // assignment then held a finished job there for good, so that
        // [settled] spun on the main thread forever.
        val job = scope.launch(start = CoroutineStart.LAZY) {
            try {
                while (unsettled) {
                    unsettled = false
                    val snapshot = accounts.toList()
                    for (account in snapshot) {
                        settle(account)
                    }
                }
            } finally {
                settling = null
            }
        }
        settling = job
        job.start()
    }

    private suspend fun settle(account: PushAccount) {
        val hostId = hostIdOf(account.profile) ?: return
        val hostText = hostId.toString()
        // If Keystore cannot be read right now (e.g. before first unlock),
        // do not generate a new key that would strand paired machines.
        val held: PushPrivateKey? = try {
            keys.key(hostText)
        } catch (_: PushKeyFailure) {
            return
        }

        when (wanted(account)) {
            null -> {
                if (held != null) {
                    tell(account.profile, held)
                }
            }
            true -> {
                val key = held ?: try {
                    keys.makeKey(hostText)
                } catch (_: PushKeyFailure) {
                    return
                }
                tell(account.profile, key)
                register(account, hostId)
            }
            false -> {
                if (held != null || said.containsKey(account.profile) || registered.containsKey(account.profile)) {
                    withdrawProfile(account, hostId)
                }
            }
        }
    }

    private fun wanted(account: PushAccount): Boolean? {
        if (account.account in leaving || account.wanted == false || permitted == false) {
            return false
        }
        return if (permitted == null) null else account.wanted
    }

    private suspend fun hostIdOf(profile: String): HostId? {
        hosts[profile]?.let { return it }
        val found = host(profile) ?: return null
        hosts[profile] = found
        return found
    }

    private suspend fun tell(profile: String, key: PushPrivateKey) {
        val bytes = key.publicKey.map { it.toInt() and 0xFF }
        if (said[profile] == bytes) return
        if (say(profile, bytes)) {
            said[profile] = bytes
        }
    }

    private suspend fun register(account: PushAccount, hostId: HostId) {
        val currentToken = token ?: return
        if (registered[account.profile] == currentToken) return
        val device = PushDevice(
            token = currentToken,
            environment = environment,
            transport = TRANSPORT_FCM,
        )
        try {
            cloud.registerDevice(account.account, hostId, device)
            registered[account.profile] = currentToken
        } catch (c: CancellationException) {
            throw c
        } catch (_: Throwable) {
            // Retried on the next foreground resume or state update.
        }
    }

    private suspend fun withdrawProfile(account: PushAccount, hostId: HostId) {
        say(account.profile, null)
        said.remove(account.profile)
        registered.remove(account.profile)
        try {
            keys.removeKey(hostId.toString())
        } catch (_: PushKeyFailure) {
            // Proceed to remove the server row even if local cleanup failed.
        }
        try {
            cloud.removeDevice(account.account, hostId)
        } catch (c: CancellationException) {
            throw c
        } catch (_: Throwable) {
            // With the key gone and unsaid, no host can seal new pushes.
        }
    }

    companion object {
        const val TRANSPORT_FCM: String = "fcm"
    }
}
