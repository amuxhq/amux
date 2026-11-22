package sh.amux.core.runtime

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** One subscription plan offered by a billing storefront. */
@Serializable
data class Plan(
    val id: String,
    val period: Period,
    val price: String,
    val saving: String? = null,
) {
    @Serializable
    enum class Period {
        @SerialName("monthly")
        Monthly,

        @SerialName("yearly")
        Yearly,
        ;

        val title: String
            get() = when (this) {
                Monthly -> "Monthly"
                Yearly -> "Yearly"
            }

        val spelled: String
            get() = when (this) {
                Monthly -> "a month"
                Yearly -> "a year"
            }
    }

    companion object {
        const val MONTHLY_ID: String = "amux_pro_monthly"
        const val YEARLY_ID: String = "amux_pro_yearly"
        val identifiers: List<String> = listOf(MONTHLY_ID, YEARLY_ID)
    }
}

/** A store-signed purchase token/JWS carried opaquely to `amux.sh`. */
@Serializable
data class SignedPurchase(
    val id: String,
    val productId: String,
    val signed: String,
)

sealed interface PurchaseOutcome {
    data class Bought(val purchase: SignedPurchase) : PurchaseOutcome
    data object Pending : PurchaseOutcome
    data object Cancelled : PurchaseOutcome
    data object NothingToRestore : PurchaseOutcome
}

sealed interface StoreError {
    data class Unavailable(val message: String) : StoreError
    data class Failed(val message: String) : StoreError
}

class StoreException(val error: StoreError) : Exception(
    when (error) {
        is StoreError.Unavailable -> error.message
        is StoreError.Failed -> error.message
    },
)

/**
 * Storefront billing seam: [StubBillingStoreFront] until Google Play Billing is
 * wired, `ScriptedBillingStoreFront` in tests.
 */
interface BillingStoreFront {
    @Throws(StoreException::class)
    suspend fun plans(): List<Plan>

    @Throws(StoreException::class)
    suspend fun buy(plan: Plan): PurchaseOutcome

    @Throws(StoreException::class)
    suspend fun restore(): PurchaseOutcome

    /** Acknowledges/finishes a purchase strictly after `amux.sh` accepts it. */
    suspend fun finish(purchase: SignedPurchase)

    /** Returns any unacknowledged purchases still held by the store. */
    suspend fun unfinished(): List<SignedPurchase>

    /** Emits asynchronous purchase approvals (pending payments, renewals). */
    fun approvals(): Flow<SignedPurchase>
}

/**
 * The storefront while Google Play Billing is not wired: it reports in-app
 * purchases unavailable, so the paywall sends people to `amux.sh` to manage
 * their subscription.
 */
class StubBillingStoreFront(
    val unavailableMessage: String = DEFAULT_UNAVAILABLE_MESSAGE,
) : BillingStoreFront {
    override suspend fun plans(): List<Plan> {
        throw StoreException(StoreError.Unavailable(unavailableMessage))
    }

    override suspend fun buy(plan: Plan): PurchaseOutcome {
        throw StoreException(StoreError.Unavailable(unavailableMessage))
    }

    override suspend fun restore(): PurchaseOutcome {
        throw StoreException(StoreError.Unavailable(unavailableMessage))
    }

    override suspend fun finish(purchase: SignedPurchase) = Unit

    override suspend fun unfinished(): List<SignedPurchase> = emptyList()

    override fun approvals(): Flow<SignedPurchase> = emptyFlow()

    companion object {
        const val DEFAULT_UNAVAILABLE_MESSAGE: String =
            "In-app subscriptions are not available in this build yet. Manage your plan at amux.sh."
    }
}
