package sh.amux.core.runtime

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

data class PaywallState(
    val plans: List<Plan> = emptyList(),
    val chosen: Plan.Period = Plan.Period.Yearly,
    val phase: PaywallStore.Phase = PaywallStore.Phase.Ready,
    val entitlement: Entitlement = Entitlement.None,
    val holding: SignedPurchase? = null,
    val manageUrl: String = CloudEndpoint.Production.accountPageUrl,
) {
    val plan: Plan?
        get() = plans.firstOrNull { it.period == chosen } ?: plans.firstOrNull()

    val entitled: Boolean
        get() = entitlement.isActive

    val grant: Grant?
        get() = when (entitlement) {
            is Entitlement.Active -> entitlement.grant
            is Entitlement.Lapsed -> entitlement.grant
            Entitlement.None -> null
        }

    val working: Boolean
        get() = phase == PaywallStore.Phase.Buying || phase == PaywallStore.Phase.Confirming
}

/**
 * Observable paywall store: reads entitlement from [CloudService], offers web
 * management, and enforces the purchase → `recordPurchase` → `finish` order.
 */
class PaywallStore(
    entitlement: Entitlement = Entitlement.None,
    plans: List<Plan> = emptyList(),
    phase: Phase = Phase.Ready,
    holding: SignedPurchase? = null,
    manageUrl: String = CloudEndpoint.Production.accountPageUrl,
) {
    sealed interface Phase {
        data object Ready : Phase
        data object Buying : Phase
        data object AwaitingApproval : Phase
        data object Confirming : Phase
        data class Unconfirmed(val why: PaywallStore.Unconfirmed) : Phase
        data class Failed(val reason: String) : Phase
        data class Entitled(val grant: Grant) : Phase
    }

    sealed interface Unconfirmed {
        data object Unreachable : Unconfirmed
        data class Refused(val reason: String) : Unconfirmed
        data object SwitchingOn : Unconfirmed

        val named: String
            get() = when (this) {
                Unreachable -> "unreachable"
                is Refused -> "refused"
                SwitchingOn -> "switching on"
            }
    }

    private val _state = MutableStateFlow(
        PaywallState(
            plans = plans,
            phase = phase,
            entitlement = entitlement,
            holding = holding,
            manageUrl = manageUrl,
        ),
    )
    val state: StateFlow<PaywallState> = _state.asStateFlow()

    val snapshot: PaywallState
        get() = _state.value

    fun choose(period: Plan.Period) {
        if (_state.value.working) return
        _state.update { it.copy(chosen = period) }
    }

    suspend fun load(store: BillingStoreFront) {
        try {
            val loaded = store.plans()
            _state.update { current ->
                val keepEntitled = current.phase is Phase.Entitled || current.entitled
                current.copy(
                    plans = loaded,
                    phase = if (loaded.isEmpty() && !keepEntitled) {
                        Phase.Failed("No subscription plans are available right now.")
                    } else {
                        current.phase
                    },
                )
            }
        } catch (e: StoreException) {
            _state.update { current ->
                if (current.phase is Phase.Entitled || current.entitled) {
                    current
                } else {
                    current.copy(phase = phaseAfter(e.error))
                }
            }
        }
    }

    suspend fun refreshEntitlement(cloud: CloudService, account: AccountId): Entitlement? {
        return try {
            val latest = cloud.entitlement(account)
            entitled(latest)
            latest
        } catch (_: CloudException) {
            null
        }
    }

    suspend fun buy(store: BillingStoreFront): PurchaseOutcome? {
        val current = _state.value
        val plan = current.plan ?: return null
        if (current.working || current.entitled) return null
        _state.update { it.copy(phase = Phase.Buying) }
        return try {
            val outcome = store.buy(plan)
            settle(outcome)
            outcome
        } catch (e: StoreException) {
            _state.update { it.copy(phase = phaseAfter(e.error)) }
            null
        }
    }

    suspend fun restore(store: BillingStoreFront): PurchaseOutcome? {
        if (_state.value.working) return null
        _state.update { it.copy(phase = Phase.Buying) }
        return try {
            val outcome = store.restore()
            settle(outcome)
            outcome
        } catch (e: StoreException) {
            _state.update { it.copy(phase = phaseAfter(e.error)) }
            null
        }
    }

    private fun settle(outcome: PurchaseOutcome) {
        when (outcome) {
            is PurchaseOutcome.Bought -> {
                _state.update { it.copy(holding = outcome.purchase, phase = Phase.Confirming) }
            }
            PurchaseOutcome.Pending -> {
                _state.update { it.copy(phase = Phase.AwaitingApproval) }
            }
            PurchaseOutcome.Cancelled -> {
                _state.update { it.copy(phase = Phase.Ready) }
            }
            PurchaseOutcome.NothingToRestore -> {
                _state.update {
                    it.copy(phase = Phase.Failed("There is nothing on this account to restore."))
                }
            }
        }
    }

    /** Posts [purchase] to [cloud], and finishes it with [store] only after [cloud] accepts. */
    suspend fun confirm(
        purchase: SignedPurchase,
        cloud: CloudService,
        account: AccountId,
        store: BillingStoreFront,
    ): Boolean {
        _state.update { it.copy(holding = purchase, phase = Phase.Confirming) }
        try {
            cloud.recordPurchase(account, purchase.signed)
        } catch (e: CloudException) {
            _state.update { it.copy(phase = Phase.Unconfirmed(unconfirmedAfter(e.error))) }
            return false
        }
        store.finish(purchase)
        _state.update { it.copy(holding = null) }
        return true
    }

    suspend fun confirmOutstanding(
        store: BillingStoreFront,
        cloud: CloudService,
        account: AccountId,
    ): Boolean {
        var taken = false
        for (purchase in store.unfinished()) {
            if (_state.value.entitled) break
            if (confirm(purchase, cloud, account, store)) {
                taken = true
            }
        }
        return taken
    }

    fun unconfirmed(why: Unconfirmed) {
        _state.update { it.copy(phase = Phase.Unconfirmed(why)) }
    }

    fun entitled(entitlement: Entitlement) {
        _state.update { current ->
            val nextPhase = when {
                entitlement is Entitlement.Active -> {
                    if (current.phase != Phase.Buying) Phase.Entitled(entitlement.grant) else current.phase
                }
                current.phase == Phase.Confirming -> Phase.Unconfirmed(Unconfirmed.SwitchingOn)
                current.phase is Phase.Entitled -> Phase.Ready
                else -> current.phase
            }
            current.copy(entitlement = entitlement, phase = nextPhase)
        }
    }

    companion object {
        fun unconfirmedAfter(error: CloudError): Unconfirmed = when (error) {
            is CloudError.Refused -> Unconfirmed.Refused(error.reason)
            is CloudError.Network,
            CloudError.Timeout,
            CloudError.Cancelled,
            CloudError.Unauthenticated,
            -> Unconfirmed.Unreachable
        }

        fun phaseAfter(error: StoreError): Phase = when (error) {
            is StoreError.Unavailable -> Phase.Failed(error.message)
            is StoreError.Failed -> Phase.Failed(error.message)
        }
    }
}
