package sh.amux.core.runtime

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

data class DeletionState(
    val account: AccountId? = null,
    val typed: String = "",
    val phase: DeletionStore.Phase = DeletionStore.Phase.Asking,
) {
    val working: Boolean
        get() = phase == DeletionStore.Phase.Working

    /** Case-insensitive trimmed email match confirming deletion. */
    fun confirms(email: String): Boolean {
        val written = typed.trim()
        return written.isNotEmpty() && written.equals(email, ignoreCase = true)
    }
}

/**
 * Coordinates the account deletion confirmation dialog, typed email check,
 * and billing-renewal 409 block recovery.
 */
class DeletionStore(
    asking: AccountId? = null,
    typed: String = "",
    phase: Phase = Phase.Asking,
) {
    sealed interface Phase {
        data object Asking : Phase
        data object Working : Phase
        data class Blocked(
            val source: EntitlementSource,
            val manageUrl: String,
        ) : Phase

        data object Deleted : Phase
        data class Failed(val reason: String) : Phase
    }

    private val _state = MutableStateFlow(DeletionState(asking, typed, phase))
    val state: StateFlow<DeletionState> = _state.asStateFlow()

    val snapshot: DeletionState
        get() = _state.value

    fun ask(id: AccountId) {
        _state.value = DeletionState(account = id, typed = "", phase = Phase.Asking)
    }

    fun setTyped(text: String) {
        _state.update { it.copy(typed = text) }
    }

    /** Returns to the confirmation prompt while keeping what the user typed. */
    fun askAgain() {
        _state.update { current ->
            if (current.account != null && !current.working) {
                current.copy(phase = Phase.Asking)
            } else {
                current
            }
        }
    }

    fun dismiss() {
        _state.value = DeletionState()
    }

    fun confirms(email: String): Boolean = _state.value.confirms(email)

    suspend fun delete(
        cloud: CloudService,
        forget: ((AccountId) -> Unit)? = null,
    ): DeletionOutcome? {
        val current = _state.value
        val id = current.account ?: return null
        if (current.working) return null
        _state.update { it.copy(phase = Phase.Working) }
        return try {
            when (val outcome = cloud.requestDeletion(id, current.typed)) {
                DeletionOutcome.Deleted -> {
                    _state.value = DeletionState(account = null, typed = "", phase = Phase.Deleted)
                    forget?.invoke(id)
                    outcome
                }
                is DeletionOutcome.BlockedByRenewal -> {
                    _state.update {
                        it.copy(phase = Phase.Blocked(outcome.source, outcome.manageUrl))
                    }
                    outcome
                }
            }
        } catch (e: CancellationException) {
            _state.update { it.copy(phase = Phase.Asking) }
            throw e
        } catch (e: CloudException) {
            _state.update { it.copy(phase = phaseAfter(e.error)) }
            null
        }
    }

    companion object {
        fun phaseAfter(error: CloudError): Phase = when (error) {
            CloudError.Cancelled -> Phase.Asking
            CloudError.Unauthenticated -> Phase.Failed("amux.sh no longer recognises this account")
            is CloudError.Refused -> Phase.Failed(error.reason)
            is CloudError.Network -> Phase.Failed(error.message)
            CloudError.Timeout -> Phase.Failed("amux.sh did not answer")
        }
    }
}
