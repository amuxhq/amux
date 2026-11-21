package sh.amux.core.runtime

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import sh.amux.values.AccountBinding
import sh.amux.values.HostView
import sh.amux.values.OfferRole
import sh.amux.values.SignInOfferOutcome
import sh.amux.values.SignInOfferView

/** One account offered across a pairing, either direction. */
sealed interface SignInPrompt {
    data class Offered(val offer: SignInOfferView) : SignInPrompt
    data class Offering(val host: HostView, val email: String) : SignInPrompt

    val role: OfferRole
        get() = when (this) {
            is Offered -> OfferRole.Receiver
            is Offering -> OfferRole.Offerer
        }

    val hostBytes: List<Int>
        get() = when (this) {
            is Offered -> offer.hostId
            is Offering -> host.hostId
        }
}

data class SignInOffersState(
    val received: List<SignInOfferView> = emptyList(),
    val answering: List<Int>? = null,
    val failure: SignInOffers.Failure? = null,
)

/** Tracks incoming and outgoing one-tap account offers across paired hosts. */
class SignInOffers {
    sealed interface Failure {
        data class Unreachable(val hostName: String) : Failure
        data class Refused(val reason: String) : Failure
    }

    private val _state = MutableStateFlow(SignInOffersState())
    val state: StateFlow<SignInOffersState> = _state.asStateFlow()

    val snapshot: SignInOffersState
        get() = _state.value

    private var failedFor: List<Int>? = null
    private val pairedHosts = mutableListOf<HostId>()
    private val settledHosts = mutableSetOf<HostId>()
    private val reportedHosts = mutableSetOf<List<Int>>()

    fun listed(offers: List<SignInOfferView>) {
        val clearFailure = failedFor != null && offers.none { it.hostId == failedFor }
        if (clearFailure) {
            failedFor = null
        }
        _state.update { current ->
            current.copy(
                received = offers,
                failure = if (clearFailure) null else current.failure,
            )
        }
    }

    fun paired(host: HostId) {
        if (host !in pairedHosts) {
            pairedHosts.add(host)
        }
    }

    fun prompt(hosts: HostsState): SignInPrompt? {
        _state.value.received.firstOrNull()?.let { return SignInPrompt.Offered(it) }
        val account = hosts.account
        if (account == null || account.binding != AccountBinding.SignedIn || account.email.isEmpty()) {
            return null
        }
        for (id in pairedHosts) {
            if (id in settledHosts) continue
            val host = hosts.host(id)
            if (host != null && host.signedIn == false) {
                return SignInPrompt.Offering(host = host, email = account.email)
            }
        }
        return null
    }

    fun firstShown(prompt: SignInPrompt): Boolean = reportedHosts.add(prompt.hostBytes)

    fun answers(hostId: List<Int>) {
        _state.update { it.copy(answering = hostId, failure = null) }
    }

    fun answered(outcome: Result<SignInOfferOutcome>, fromHostId: List<Int>) {
        outcome.fold(
            onSuccess = { value ->
                when (value) {
                    is SignInOfferOutcome.SignedIn,
                    SignInOfferOutcome.Declined,
                    -> {
                        failedFor = null
                        _state.update { it.copy(answering = null, failure = null) }
                    }
                    SignInOfferOutcome.Unreachable -> {
                        failedFor = fromHostId
                        val name = _state.value.received
                            .firstOrNull { it.hostId == fromHostId }
                            ?.hostName
                            .orEmpty()
                        _state.update {
                            it.copy(answering = null, failure = Failure.Unreachable(name))
                        }
                    }
                }
            },
            onFailure = { err ->
                failedFor = fromHostId
                _state.update {
                    it.copy(answering = null, failure = Failure.Refused(err.failureDescription))
                }
            },
        )
    }

    fun offered(outcome: Result<Unit>, to: HostId) {
        @Suppress("UNUSED_VARIABLE")
        val ignored = outcome
        settledHosts.add(to)
        _state.update { it.copy(answering = null) }
    }

    fun notOffering(host: HostId) {
        settledHosts.add(host)
    }

    fun dismissFailure() {
        failedFor = null
        _state.update { it.copy(failure = null) }
    }
}
