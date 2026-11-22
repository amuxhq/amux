package sh.amux.core.runtime

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** Tracks the account currently awaiting confirmation for removal from this phone. */
class RemovalStore(asking: AccountId? = null) {
    private val _account = MutableStateFlow(asking)
    val account: StateFlow<AccountId?> = _account.asStateFlow()

    val current: AccountId?
        get() = _account.value

    fun ask(id: AccountId) {
        _account.value = id
    }

    fun dismiss() {
        _account.value = null
    }
}
