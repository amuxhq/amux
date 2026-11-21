package sh.amux.core.runtime

import java.io.File
import java.io.IOException
import java.util.logging.Level
import java.nio.charset.StandardCharsets
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.SerializationException
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import sh.amux.values.AccountBinding
import sh.amux.values.ProfileView
import sh.amux.words.AccountWords

/** One signed-in or signed-out account listed on this phone. */
@Serializable
data class AccountEntry(
    val account: SignedInAccount,
    val signedIn: Boolean = true,
    val entitlement: Entitlement = Entitlement.None,
    val hosts: Int? = null,
    val attention: Int? = null,
    val profile: String = "",
) {
    val id: AccountId
        get() = account.id

    val name: String
        get() = account.displayName ?: account.email

    val line: String
        get() = AccountWords.line(signedIn = signedIn, computers = hosts, email = account.email)
}

data class AccountRegistryState(
    val accounts: List<AccountEntry> = emptyList(),
    val selected: AccountId? = null,
    val unbound: String? = null,
) {
    val selectedAccount: AccountEntry?
        get() = accounts.firstOrNull { it.id == selected }

    /** The profile currently on screen: the selected account's, else the unbound profile. */
    val profile: String?
        get() = selected?.let { id -> accounts.firstOrNull { it.id == id }?.profile } ?: unbound
}

/**
 * Tracks every account profile on this phone, which account is currently on
 * screen, and cached per-account host/attention counts and entitlements.
 *
 * Read and changed on the main thread. What it remembers is written to [file]
 * on [ioDispatcher] once changes pause for [persistDelayMs], since counts can
 * move on every fleet delta.
 */
class AccountRegistry(
    private val file: File? = null,
    private val makeStores: (AccountId) -> StoreBundle,
    ioDispatcher: CoroutineDispatcher = Dispatchers.IO,
    private val persistDelayMs: Long = PERSIST_DELAY_MS,
) {
    @Serializable
    private data class Seen(
        val hosts: Int? = null,
        val attention: Int? = null,
        val entitlement: Entitlement = Entitlement.None,
    )

    @Serializable
    private data class Keyed(
        val account: AccountId,
        val seen: Seen,
    )

    @Serializable
    private data class Remembered(
        val selected: AccountId? = null,
        val seen: List<Keyed> = emptyList(),
    )

    private val seen = mutableMapOf<AccountId, Seen>()
    private var awaited: AccountId? = null

    private val writer = CoroutineScope(SupervisorJob() + ioDispatcher)
    private val writing = Mutex()
    private var pendingWrite: Job? = null

    private val _state = MutableStateFlow(AccountRegistryState())
    val state: StateFlow<AccountRegistryState> = _state.asStateFlow()

    private val _stores = MutableStateFlow<StoreBundle?>(null)
    val storesFlow: StateFlow<StoreBundle?> = _stores.asStateFlow()

    val stores: StoreBundle?
        get() = _stores.value

    val snapshot: AccountRegistryState
        get() = _state.value

    val accounts: List<AccountEntry>
        get() = _state.value.accounts

    val selected: AccountId?
        get() = _state.value.selected

    val selectedAccount: AccountEntry?
        get() = _state.value.selectedAccount

    val unbound: String?
        get() = _state.value.unbound

    val profile: String?
        get() = _state.value.profile

    var dropped: Int = 0
        private set

    var switching: ((AccountId?) -> Unit)? = null
    var changed: (() -> Unit)? = null

    init {
        val loaded = file?.takeIf { it.isFile }?.let { path ->
            try {
                AmuxJson.json.decodeFromString<Remembered>(path.readText(StandardCharsets.UTF_8))
            } catch (_: IOException) {
                null
            } catch (_: SerializationException) {
                null
            }
        }
        if (loaded != null) {
            for (item in loaded.seen) {
                seen.putIfAbsent(item.account, item.seen)
            }
            _state.value = AccountRegistryState(selected = loaded.selected)
            _stores.value = loaded.selected?.let(::buildBundle)
        } else {
            persist()
        }
    }

    /** Applies the latest profile list from the embedded runtime. */
    fun show(profiles: List<ProfileView>) {
        val beforeProfile = profile
        val mappedAccounts = profiles
            .filter { it.subject.isNotEmpty() }
            .map { view ->
                val id = AccountId(view.subject)
                val cached = seen[id] ?: Seen()
                AccountEntry(
                    account = SignedInAccount(
                        id = id,
                        email = view.account.email,
                        displayName = view.account.name.ifEmpty { null },
                        pictureUrl = view.account.picture.ifEmpty { null },
                    ),
                    signedIn = view.account.binding != AccountBinding.SignedOut,
                    entitlement = cached.entitlement,
                    hosts = cached.hosts,
                    attention = cached.attention,
                    profile = view.id,
                )
            }
        val nextUnbound = profiles.firstOrNull { it.subject.isEmpty() }?.id

        val listedIds = mappedAccounts.map { it.id }.toSet()
        var removedStale = false
        val iterator = seen.keys.iterator()
        while (iterator.hasNext()) {
            if (iterator.next() !in listedIds) {
                iterator.remove()
                removedStale = true
            }
        }

        _state.update { it.copy(accounts = mappedAccounts, unbound = nextUnbound) }
        if (removedStale) {
            persist()
        }

        val wanted = awaited
        if (wanted != null && mappedAccounts.any { it.id == wanted }) {
            awaited = null
            if (_state.value.selected != wanted) {
                place(wanted)
                return
            }
        }

        val currentSelected = _state.value.selected
        val needsReplacement = if (currentSelected != null) {
            mappedAccounts.none { it.id == currentSelected }
        } else {
            nextUnbound == null
        }

        when {
            needsReplacement -> place(nextAccount())
            profile != beforeProfile -> {
                switching?.invoke(_state.value.selected)
                changed?.invoke()
            }
            else -> changed?.invoke()
        }
    }

    fun select(id: AccountId) {
        awaited = null
        if (_state.value.selected == id) return
        if (_state.value.accounts.none { it.id == id }) {
            awaited = id
            return
        }
        place(id)
    }

    /** Moves away from [id] before its profile is deleted from the runtime. */
    fun leave(id: AccountId) {
        if (awaited == id) awaited = null
        if (_state.value.selected != id) return
        place(nextAccount(leaving = id))
    }

    fun entitlement(entitlement: Entitlement, forAccount: AccountId) {
        val current = seen[forAccount] ?: Seen()
        if (current.entitlement == entitlement) return
        seen[forAccount] = current.copy(entitlement = entitlement)
        _state.update { state ->
            state.copy(
                accounts = state.accounts.map { entry ->
                    if (entry.id == forAccount) entry.copy(entitlement = entitlement) else entry
                },
            )
        }
        persist()
        changed?.invoke()
    }

    fun saw(hosts: Int, attention: Int, forAccount: AccountId) {
        val current = seen[forAccount]
        if (current?.hosts == hosts && current.attention == attention) return
        seen[forAccount] = (current ?: Seen()).copy(hosts = hosts, attention = attention)
        _state.update { state ->
            state.copy(
                accounts = state.accounts.map { entry ->
                    if (entry.id == forAccount) {
                        entry.copy(hosts = hosts, attention = attention)
                    } else {
                        entry
                    }
                },
            )
        }
        persist()
    }

    /** Keeps a late async answer only if [forAccount] is still the selected account. */
    fun <T> accept(value: T, forAccount: AccountId): T? {
        if (forAccount != _state.value.selected) {
            dropped += 1
            return null
        }
        return value
    }

    private fun nextAccount(leaving: AccountId? = null): AccountId? {
        val remaining = _state.value.accounts.filter { it.id != leaving }
        return remaining.firstOrNull { it.signedIn }?.id ?: remaining.firstOrNull()?.id
    }

    private fun place(id: AccountId?) {
        if (_state.value.selected != id) {
            _stores.value?.hosts?.stopWaiting()
            _state.update { it.copy(selected = id) }
            _stores.value = id?.let(::buildBundle)
            persist()
        }
        switching?.invoke(_state.value.selected)
        changed?.invoke()
    }

    private fun buildBundle(id: AccountId): StoreBundle {
        val bundle = makeStores(id)
        bundle.saw = { hosts, attention -> saw(hosts, attention, forAccount = id) }
        return bundle
    }

    private fun persist() {
        val target = file ?: return
        val keyed = seen.entries
            .map { (account, value) -> Keyed(account, value) }
            .sortedBy { it.account.value }
        val json = AmuxJson.json.encodeToString(
            Remembered(selected = _state.value.selected, seen = keyed),
        )
        pendingWrite?.cancel()
        pendingWrite = writer.launch {
            delay(persistDelayMs)
            // One write at a time, so an older one never lands over a newer.
            writing.withLock { write(target, json) }
        }
    }

    private fun write(target: File, json: String) {
        try {
            target.parentFile?.mkdirs()
            val tmp = File(target.parentFile, "${target.name}.tmp")
            tmp.writeText(json, StandardCharsets.UTF_8)
            if (!tmp.renameTo(target)) {
                target.writeText(json, StandardCharsets.UTF_8)
                tmp.delete()
            }
        } catch (e: Exception) {
            // The next change writes everything again. Until then a lost write
            // costs which account opens first next launch and the counts an
            // account row shows before its fleet opens; the runtime holds the
            // accounts themselves. Nothing is let through: the writer scope
            // has no handler, so a throw here would end the app.
            runtimeLog.log(Level.WARNING, "accounts file not written: ${e.message ?: e}", e)
        }
    }

    companion object {
        /** How long changes must pause before the remembered accounts are written. */
        const val PERSIST_DELAY_MS: Long = 500L
    }
}
