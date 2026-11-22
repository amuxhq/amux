package sh.amux.testsupport

import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonPrimitive
import sh.amux.core.runtime.BillingStoreFront
import sh.amux.core.runtime.Plan
import sh.amux.core.runtime.PurchaseOutcome
import sh.amux.core.runtime.SignedPurchase
import sh.amux.core.runtime.StoreError
import sh.amux.core.runtime.StoreException

/** Scripted storefront outcomes for deterministic paywall tests and journeys. */
data class ScriptedBillingState(
    val plans: List<Plan> = OFFERED_PLANS,
    val purchase: Outcome = Outcome.Bought,
    val restore: Outcome = Outcome.NothingToRestore,
    val latencyMs: Long = 0L,
) {
    sealed interface Outcome {
        data object Bought : Outcome
        data object Pending : Outcome
        data object Cancelled : Outcome
        data object NothingToRestore : Outcome
        data class Fails(val reason: String) : Outcome
    }

    companion object {
        val OFFERED_PLANS: List<Plan> = listOf(
            Plan(id = Plan.MONTHLY_ID, period = Plan.Period.Monthly, price = "£7.99"),
            Plan(
                id = Plan.YEARLY_ID,
                period = Plan.Period.Yearly,
                price = "£79.99",
                saving = "2 months free",
            ),
        )

        const val SIGNED_TRANSACTION: String = "scripted.signed.transaction"
    }
}

/** One recorded call made against [ScriptedBillingStoreFront]. */
sealed interface BillingCall {
    data object Plans : BillingCall
    data class Buy(val productId: String) : BillingCall
    data object Restore : BillingCall
    data class Finish(val purchaseId: String) : BillingCall
    data object Unfinished : BillingCall
}

/** Thread-safe scripted [BillingStoreFront] double that tracks held transactions. */
class ScriptedBillingStoreFront(
    initialState: ScriptedBillingState = ScriptedBillingState(),
) : BillingStoreFront {
    private val lock = Any()
    private var state: ScriptedBillingState = initialState
    private val recorded = mutableListOf<BillingCall>()
    private val holding = mutableListOf<SignedPurchase>()
    private val approvalFlow = MutableSharedFlow<SignedPurchase>(extraBufferCapacity = 16)

    val calls: List<BillingCall>
        get() = synchronized(lock) { recorded.toList() }

    val held: List<SignedPurchase>
        get() = synchronized(lock) { holding.toList() }

    var scripted: ScriptedBillingState
        get() = synchronized(lock) { state }
        set(value) {
            synchronized(lock) { state = value }
        }

    fun reset() {
        synchronized(lock) {
            recorded.clear()
            holding.clear()
        }
    }

    private fun record(call: BillingCall): ScriptedBillingState = synchronized(lock) {
        recorded.add(call)
        state
    }

    private suspend fun waitLatency(snapshot: ScriptedBillingState) {
        if (snapshot.latencyMs > 0L) {
            delay(snapshot.latencyMs)
        }
    }

    override suspend fun plans(): List<Plan> {
        val snapshot = record(BillingCall.Plans)
        waitLatency(snapshot)
        return snapshot.plans
    }

    override suspend fun buy(plan: Plan): PurchaseOutcome {
        val snapshot = record(BillingCall.Buy(plan.id))
        waitLatency(snapshot)
        return resolveOutcome(snapshot.purchase, plan.id)
    }

    override suspend fun restore(): PurchaseOutcome {
        val snapshot = record(BillingCall.Restore)
        waitLatency(snapshot)
        return resolveOutcome(snapshot.restore, Plan.YEARLY_ID)
    }

    override suspend fun finish(purchase: SignedPurchase) {
        synchronized(lock) {
            recorded.add(BillingCall.Finish(purchase.id))
            holding.removeAll { it.id == purchase.id }
        }
    }

    override suspend fun unfinished(): List<SignedPurchase> = synchronized(lock) {
        recorded.add(BillingCall.Unfinished)
        holding.toList()
    }

    override fun approvals(): Flow<SignedPurchase> = approvalFlow.asSharedFlow()

    /** Emits an asynchronous store approval (for example, pending payment completion). */
    fun approve(planId: String = Plan.YEARLY_ID) {
        val purchase = SignedPurchase(
            id = "scripted-approval",
            productId = planId,
            signed = ScriptedBillingState.SIGNED_TRANSACTION,
        )
        synchronized(lock) {
            holding.add(purchase)
        }
        approvalFlow.tryEmit(purchase)
    }

    private fun resolveOutcome(
        outcome: ScriptedBillingState.Outcome,
        planId: String,
    ): PurchaseOutcome = when (outcome) {
        ScriptedBillingState.Outcome.Bought -> {
            val purchase = SignedPurchase(
                id = "scripted-transaction",
                productId = planId,
                signed = ScriptedBillingState.SIGNED_TRANSACTION,
            )
            synchronized(lock) { holding.add(purchase) }
            PurchaseOutcome.Bought(purchase)
        }
        ScriptedBillingState.Outcome.Pending -> PurchaseOutcome.Pending
        ScriptedBillingState.Outcome.Cancelled -> PurchaseOutcome.Cancelled
        ScriptedBillingState.Outcome.NothingToRestore -> PurchaseOutcome.NothingToRestore
        is ScriptedBillingState.Outcome.Fails -> throw StoreException(StoreError.Failed(outcome.reason))
    }
}

/** Wire-friendly script descriptor used by debug Door `store` commands. */
data class BillingScript(
    val selling: Boolean = true,
    val purchase: String = "bought",
    val restore: String = "nothingToRestore",
    val reason: String = "Google Play could not complete this purchase",
    val approve: Boolean = false,
    val latencyMillis: Int = 0,
) {
    fun toState(): ScriptedBillingState = ScriptedBillingState(
        plans = if (selling) ScriptedBillingState.OFFERED_PLANS else emptyList(),
        purchase = parseOutcome(purchase),
        restore = parseOutcome(restore),
        latencyMs = latencyMillis.toLong(),
    )

    private fun parseOutcome(raw: String): ScriptedBillingState.Outcome = when (raw) {
        "pending" -> ScriptedBillingState.Outcome.Pending
        "cancelled" -> ScriptedBillingState.Outcome.Cancelled
        "nothingToRestore" -> ScriptedBillingState.Outcome.NothingToRestore
        "fails" -> ScriptedBillingState.Outcome.Fails(reason)
        else -> ScriptedBillingState.Outcome.Bought
    }

    companion object {
        private val json = Json { ignoreUnknownKeys = true }

        fun fromJson(raw: String): BillingScript {
            val obj = json.parseToJsonElement(raw) as? JsonObject ?: return BillingScript()
            return fromJsonObject(obj)
        }

        fun fromJsonObject(obj: JsonObject): BillingScript {
            val defaults = BillingScript()
            fun str(key: String, fallback: String): String =
                obj[key]?.takeUnless { it is JsonNull }?.jsonPrimitive?.contentOrNull ?: fallback

            return BillingScript(
                selling = obj["selling"]?.jsonPrimitive?.booleanOrNull ?: defaults.selling,
                purchase = str("purchase", defaults.purchase),
                restore = str("restore", defaults.restore),
                reason = str("reason", defaults.reason),
                approve = obj["approve"]?.jsonPrimitive?.booleanOrNull ?: defaults.approve,
                latencyMillis = obj["latencyMillis"]?.jsonPrimitive?.intOrNull ?: defaults.latencyMillis,
            )
        }
    }
}
