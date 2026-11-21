package sh.amux.core.runtime

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import sh.amux.words.CloudWords

/** Immutable UI snapshot of an in-flight or completed browser sign-in flow. */
data class SignInState(
    val phase: SignInStore.Phase = SignInStore.Phase.Ready,
    val intent: SignInIntent = SignInIntent.Adding,
) {
    val working: Boolean
        get() = phase == SignInStore.Phase.HandingOff

    val pending: SignInIntent?
        get() = if (working) intent else null
}

/**
 * Coordinates the single active browser sign-in attempt, account-mismatch
 * confirmation, and refusal presentation.
 */
class SignInStore(
    initialPhase: Phase = Phase.Ready,
    initialIntent: SignInIntent = SignInIntent.Adding,
) {
    sealed interface Phase {
        data object Ready : Phase
        data object HandingOff : Phase
        data class Failed(val reason: String) : Phase
        data class Mismatched(
            val wanted: SignedInAccount,
            val got: SignedInAccount,
        ) : Phase
    }

    private val _state = MutableStateFlow(SignInState(initialPhase, initialIntent))
    val state: StateFlow<SignInState> = _state.asStateFlow()

    val snapshot: SignInState
        get() = _state.value

    suspend fun signIn(
        intent: SignInIntent,
        cloud: CloudService,
        presenter: WebAuthPresenter,
        keep: suspend (SignedInAccount) -> CloudError?,
    ): SignedInAccount? {
        if (_state.value.working) return null
        _state.value = SignInState(phase = Phase.HandingOff, intent = intent)
        return try {
            val account = cloud.signIn(intent, presenter)
            if (intent is SignInIntent.Returning && intent.account.id != account.id) {
                _state.update {
                    it.copy(phase = Phase.Mismatched(wanted = intent.account, got = account))
                }
                return null
            }
            kept(account, keep)
        } catch (e: CancellationException) {
            _state.update { it.copy(phase = Phase.Ready) }
            throw e
        } catch (e: CloudException) {
            _state.update { it.copy(phase = phaseAfter(e.error)) }
            null
        }
    }

    fun dismiss() {
        _state.update { current ->
            if (current.phase is Phase.Failed) current.copy(phase = Phase.Ready) else current
        }
    }

    suspend fun keep(keep: suspend (SignedInAccount) -> CloudError?): SignedInAccount? {
        val mismatched = _state.value.phase as? Phase.Mismatched ?: return null
        _state.update { it.copy(phase = Phase.HandingOff) }
        return try {
            kept(mismatched.got, keep)
        } catch (e: CancellationException) {
            _state.update { it.copy(phase = Phase.Ready) }
            throw e
        }
    }

    suspend fun discard(cloud: CloudService) {
        val mismatched = _state.value.phase as? Phase.Mismatched ?: return
        _state.update { it.copy(phase = Phase.Ready) }
        try {
            cloud.forgetSession(mismatched.got.id)
        } catch (e: CancellationException) {
            throw e
        } catch (_: Throwable) {
            // Discarding an unkept session is best-effort.
        }
    }

    private suspend fun kept(
        account: SignedInAccount,
        keep: suspend (SignedInAccount) -> CloudError?,
    ): SignedInAccount? {
        val refused = keep(account)
        if (refused != null) {
            _state.update { it.copy(phase = phaseAfter(refused)) }
            return null
        }
        _state.update { it.copy(phase = Phase.Ready) }
        return account
    }

    companion object {
        fun phaseAfter(error: CloudError): Phase = when (error) {
            CloudError.Cancelled -> Phase.Ready
            CloudError.Unauthenticated -> Phase.Failed(CloudWords.SIGN_IN_NOT_RECOGNISED)
            is CloudError.Refused -> Phase.Failed(error.reason)
            is CloudError.Network -> Phase.Failed(error.message)
            CloudError.Timeout -> Phase.Failed(CloudWords.DID_NOT_ANSWER)
        }
    }
}
