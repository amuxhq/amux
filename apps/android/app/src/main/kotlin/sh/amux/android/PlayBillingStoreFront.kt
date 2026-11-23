package sh.amux.android

import android.app.Activity
import android.content.Context
import com.android.billingclient.api.AcknowledgePurchaseParams
import com.android.billingclient.api.BillingClient
import com.android.billingclient.api.BillingClientStateListener
import com.android.billingclient.api.BillingFlowParams
import com.android.billingclient.api.BillingResult
import com.android.billingclient.api.PendingPurchasesParams
import com.android.billingclient.api.ProductDetails
import com.android.billingclient.api.Purchase
import com.android.billingclient.api.PurchasesUpdatedListener
import com.android.billingclient.api.QueryProductDetailsParams
import com.android.billingclient.api.QueryPurchasesParams
import com.android.billingclient.api.acknowledgePurchase
import com.android.billingclient.api.queryProductDetails
import com.android.billingclient.api.queryPurchasesAsync
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlin.math.roundToInt
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import sh.amux.core.runtime.BillingStoreFront
import sh.amux.core.runtime.Plan
import sh.amux.core.runtime.PurchaseOutcome
import sh.amux.core.runtime.SignedPurchase
import sh.amux.core.runtime.StoreError
import sh.amux.core.runtime.StoreException

/**
 * Production Google Play Billing implementation of [BillingStoreFront].
 *
 * Keeps all Play Billing Library types inside `:app` so `:core:runtime` stays
 * pure JVM/Robolectric-testable. Purchases are never acknowledged here during
 * [buy]; [sh.amux.core.runtime.PaywallStore] calls [finish] strictly after
 * `amux.sh` accepts the purchase token.
 */
class PlayBillingStoreFront(
    context: Context,
    private val activityProvider: () -> Activity?,
) : BillingStoreFront, PurchasesUpdatedListener {
    private data class CachedOffer(
        val productDetails: ProductDetails,
        val offerToken: String,
    )

    private object AlreadyOwnedSignal : Exception()

    private val appContext: Context = context.applicationContext
    private val connectMutex = Mutex()
    private val purchaseMutex = Mutex()
    private val cacheLock = Any()
    private val pendingLock = Any()

    private var cachedOffers: Map<String, CachedOffer> = emptyMap()
    private var pendingPurchase: CompletableDeferred<PurchaseOutcome>? = null
    private var closed: Boolean = false

    private val _approvals = MutableSharedFlow<SignedPurchase>(extraBufferCapacity = 16)

    private val billingClient: BillingClient = BillingClient.newBuilder(appContext)
        .setListener(this)
        .enablePendingPurchases(
            PendingPurchasesParams.newBuilder()
                .enableOneTimeProducts()
                .enablePrepaidPlans()
                .build(),
        )
        .enableAutoServiceReconnection()
        .build()

    override suspend fun plans(): List<Plan> {
        ensureConnected()
        val products = Plan.identifiers.map { productId ->
            QueryProductDetailsParams.Product.newBuilder()
                .setProductId(productId)
                .setProductType(BillingClient.ProductType.SUBS)
                .build()
        }
        val params = QueryProductDetailsParams.newBuilder()
            .setProductList(products)
            .build()
        val result = billingClient.queryProductDetails(params)
        if (result.billingResult.responseCode != BillingClient.BillingResponseCode.OK) {
            throw StoreException(
                mapBillingError(
                    result.billingResult.responseCode,
                    result.billingResult.debugMessage,
                ),
            )
        }

        val detailsById = result.productDetailsList.orEmpty().associateBy { it.productId }
        val nextCache = mutableMapOf<String, CachedOffer>()
        val monthlyOffer = extractOffer(
            details = detailsById[Plan.MONTHLY_ID],
            preferredBasePlanId = MONTHLY_BASE_PLAN_ID,
            cache = nextCache,
        )
        val yearlyOffer = extractOffer(
            details = detailsById[Plan.YEARLY_ID],
            preferredBasePlanId = YEARLY_BASE_PLAN_ID,
            cache = nextCache,
        )
        synchronized(cacheLock) {
            cachedOffers = nextCache.toMap()
        }
        return buildPlans(monthly = monthlyOffer, yearly = yearlyOffer)
    }

    override suspend fun buy(plan: Plan): PurchaseOutcome = purchaseMutex.withLock {
        ensureConnected()
        var offer = synchronized(cacheLock) { cachedOffers[plan.id] }
        if (offer == null) {
            plans()
            offer = synchronized(cacheLock) { cachedOffers[plan.id] }
        }
        if (offer == null) {
            throw StoreException(StoreError.Unavailable("That plan is not available on Google Play right now."))
        }
        val activity = activityProvider()
            ?: throw StoreException(StoreError.Failed("Could not open Google Play right now."))

        val flowParams = BillingFlowParams.newBuilder()
            .setProductDetailsParamsList(
                listOf(
                    BillingFlowParams.ProductDetailsParams.newBuilder()
                        .setProductDetails(offer.productDetails)
                        .setOfferToken(offer.offerToken)
                        .build(),
                ),
            )
            .build()

        val deferred = CompletableDeferred<PurchaseOutcome>()
        synchronized(pendingLock) {
            pendingPurchase = deferred
        }
        try {
            val launchResult = withContext(Dispatchers.Main.immediate) {
                billingClient.launchBillingFlow(activity, flowParams)
            }
            when (launchResult.responseCode) {
                BillingClient.BillingResponseCode.OK -> Unit
                BillingClient.BillingResponseCode.USER_CANCELED -> {
                    clearPending(deferred)
                    return PurchaseOutcome.Cancelled
                }
                BillingClient.BillingResponseCode.ITEM_ALREADY_OWNED -> {
                    clearPending(deferred)
                    return restore()
                }
                else -> {
                    clearPending(deferred)
                    throw StoreException(
                        mapBillingError(launchResult.responseCode, launchResult.debugMessage),
                    )
                }
            }
            return try {
                deferred.await()
            } catch (_: AlreadyOwnedSignal) {
                restore()
            }
        } finally {
            clearPending(deferred)
        }
    }

    override suspend fun restore(): PurchaseOutcome {
        ensureConnected()
        val purchases = querySubscriptionPurchases()
        val matching = purchases
            .filter { it.purchaseState == Purchase.PurchaseState.PURCHASED }
            .sortedBy { it.isAcknowledged }
            .firstNotNullOfOrNull { purchase ->
                toSignedPurchase(
                    orderId = purchase.orderId,
                    products = purchase.products,
                    purchaseToken = purchase.purchaseToken,
                )
            }
        return if (matching != null) {
            PurchaseOutcome.Bought(matching)
        } else {
            PurchaseOutcome.NothingToRestore
        }
    }

    override suspend fun unfinished(): List<SignedPurchase> {
        return try {
            ensureConnected()
            querySubscriptionPurchases()
                .filter { it.purchaseState == Purchase.PurchaseState.PURCHASED && !it.isAcknowledged }
                .mapNotNull { purchase ->
                    toSignedPurchase(
                        orderId = purchase.orderId,
                        products = purchase.products,
                        purchaseToken = purchase.purchaseToken,
                    )
                }
        } catch (_: StoreException) {
            emptyList()
        }
    }

    override suspend fun finish(purchase: SignedPurchase) {
        if (purchase.signed.isBlank()) return
        try {
            ensureConnected()
            val params = AcknowledgePurchaseParams.newBuilder()
                .setPurchaseToken(purchase.signed)
                .build()
            billingClient.acknowledgePurchase(params)
        } catch (_: StoreException) {
            // RevenueCat also acknowledges server-side when validating the token;
            // failing local acknowledgement after amux.sh already accepted is non-fatal.
        }
    }

    override fun approvals(): Flow<SignedPurchase> = _approvals.asSharedFlow()

    override fun onPurchasesUpdated(
        billingResult: BillingResult,
        purchases: MutableList<Purchase>?,
    ) {
        val pending = synchronized(pendingLock) {
            val current = pendingPurchase
            pendingPurchase = null
            current
        }
        if (pending != null) {
            resolvePendingPurchase(pending, billingResult, purchases)
            return
        }
        if (billingResult.responseCode != BillingClient.BillingResponseCode.OK) return
        for (purchase in purchases.orEmpty()) {
            if (purchase.purchaseState == Purchase.PurchaseState.PURCHASED && !purchase.isAcknowledged) {
                toSignedPurchase(
                    orderId = purchase.orderId,
                    products = purchase.products,
                    purchaseToken = purchase.purchaseToken,
                )?.let { _approvals.tryEmit(it) }
            }
        }
    }

    fun close() {
        closed = true
        synchronized(pendingLock) {
            pendingPurchase?.cancel()
            pendingPurchase = null
        }
        if (billingClient.isReady) {
            billingClient.endConnection()
        }
    }

    private fun resolvePendingPurchase(
        pending: CompletableDeferred<PurchaseOutcome>,
        billingResult: BillingResult,
        purchases: List<Purchase>?,
    ) {
        when (billingResult.responseCode) {
            BillingClient.BillingResponseCode.OK -> {
                val relevant = purchases.orEmpty().firstOrNull { purchase ->
                    purchase.products.any { it in Plan.identifiers }
                } ?: purchases.orEmpty().firstOrNull()
                if (relevant == null) {
                    pending.complete(PurchaseOutcome.Cancelled)
                    return
                }
                when (relevant.purchaseState) {
                    Purchase.PurchaseState.PURCHASED -> {
                        val signed = toSignedPurchase(
                            orderId = relevant.orderId,
                            products = relevant.products,
                            purchaseToken = relevant.purchaseToken,
                        )
                        if (signed != null) {
                            pending.complete(PurchaseOutcome.Bought(signed))
                        } else {
                            pending.completeExceptionally(
                                StoreException(StoreError.Failed("Google Play returned an unexpected purchase.")),
                            )
                        }
                    }
                    Purchase.PurchaseState.PENDING -> pending.complete(PurchaseOutcome.Pending)
                    else -> pending.complete(PurchaseOutcome.Cancelled)
                }
            }
            BillingClient.BillingResponseCode.USER_CANCELED -> {
                pending.complete(PurchaseOutcome.Cancelled)
            }
            BillingClient.BillingResponseCode.ITEM_ALREADY_OWNED -> {
                pending.completeExceptionally(AlreadyOwnedSignal)
            }
            else -> {
                pending.completeExceptionally(
                    StoreException(
                        mapBillingError(billingResult.responseCode, billingResult.debugMessage),
                    ),
                )
            }
        }
    }

    private fun clearPending(deferred: CompletableDeferred<PurchaseOutcome>) {
        synchronized(pendingLock) {
            if (pendingPurchase === deferred) {
                pendingPurchase = null
            }
        }
    }

    private suspend fun querySubscriptionPurchases(): List<Purchase> {
        val params = QueryPurchasesParams.newBuilder()
            .setProductType(BillingClient.ProductType.SUBS)
            .build()
        val result = billingClient.queryPurchasesAsync(params)
        if (result.billingResult.responseCode != BillingClient.BillingResponseCode.OK) {
            throw StoreException(
                mapBillingError(
                    result.billingResult.responseCode,
                    result.billingResult.debugMessage,
                ),
            )
        }
        return result.purchasesList
    }

    private suspend fun ensureConnected() {
        if (closed) {
            throw StoreException(StoreError.Unavailable("Google Play Billing is closed."))
        }
        if (billingClient.isReady) return
        connectMutex.withLock {
            if (closed) {
                throw StoreException(StoreError.Unavailable("Google Play Billing is closed."))
            }
            if (billingClient.isReady) return
            suspendCancellableCoroutine { cont ->
                billingClient.startConnection(object : BillingClientStateListener {
                    override fun onBillingSetupFinished(billingResult: BillingResult) {
                        if (!cont.isActive) return
                        if (billingResult.responseCode == BillingClient.BillingResponseCode.OK) {
                            cont.resume(Unit)
                        } else {
                            cont.resumeWithException(
                                StoreException(
                                    mapBillingError(
                                        billingResult.responseCode,
                                        billingResult.debugMessage,
                                    ),
                                ),
                            )
                        }
                    }

                    override fun onBillingServiceDisconnected() {
                        if (cont.isActive) {
                            cont.resumeWithException(
                                StoreException(
                                    StoreError.Unavailable(
                                        "Could not reach Google Play right now. Try again in a moment.",
                                    ),
                                ),
                            )
                        }
                    }
                })
            }
        }
    }

    private fun extractOffer(
        details: ProductDetails?,
        preferredBasePlanId: String,
        cache: MutableMap<String, CachedOffer>,
    ): PricedOffer? {
        if (details == null) return null
        val offers = details.subscriptionOfferDetails.orEmpty().mapNotNull { offer ->
            val recurring = offer.pricingPhases.pricingPhaseList.lastOrNull { phase ->
                phase.priceAmountMicros > 0L
            } ?: offer.pricingPhases.pricingPhaseList.lastOrNull()
                ?: return@mapNotNull null
            OfferCandidate(
                basePlanId = offer.basePlanId,
                offerId = offer.offerId,
                offerToken = offer.offerToken,
                formattedPrice = recurring.formattedPrice,
                priceAmountMicros = recurring.priceAmountMicros,
            )
        }
        val chosen = selectBasePlanOffer(offers, preferredBasePlanId) ?: return null
        cache[details.productId] = CachedOffer(
            productDetails = details,
            offerToken = chosen.offerToken,
        )
        return PricedOffer(
            productId = details.productId,
            formattedPrice = chosen.formattedPrice,
            priceAmountMicros = chosen.priceAmountMicros,
        )
    }

    internal data class OfferCandidate(
        val basePlanId: String,
        val offerId: String?,
        val offerToken: String,
        val formattedPrice: String,
        val priceAmountMicros: Long,
    )

    internal data class PricedOffer(
        val productId: String,
        val formattedPrice: String,
        val priceAmountMicros: Long,
    )

    companion object {
        internal const val MONTHLY_BASE_PLAN_ID: String = "p1m"
        internal const val YEARLY_BASE_PLAN_ID: String = "p1y"

        internal fun selectBasePlanOffer(
            offers: List<OfferCandidate>,
            preferredBasePlanId: String,
        ): OfferCandidate? {
            if (offers.isEmpty()) return null
            // Prefer the matching base plan without a promotional offer override
            // so standard recurring pricing is shown, falling back to any offer
            // on the base plan or the first offer returned by Play.
            return offers.firstOrNull { it.basePlanId == preferredBasePlanId && it.offerId == null }
                ?: offers.firstOrNull { it.basePlanId == preferredBasePlanId }
                ?: offers.firstOrNull { it.offerId == null }
                ?: offers.first()
        }

        internal fun buildPlans(
            monthly: PricedOffer?,
            yearly: PricedOffer?,
        ): List<Plan> {
            val saving = if (monthly != null && yearly != null && monthly.priceAmountMicros > 0L) {
                val twelveMonths = monthly.priceAmountMicros * 12.0
                val ratio = (twelveMonths - yearly.priceAmountMicros) / twelveMonths
                val percent = (ratio * 100.0).roundToInt()
                if (percent >= 1) "Save $percent%" else null
            } else {
                null
            }
            return buildList {
                if (monthly != null) {
                    add(
                        Plan(
                            id = monthly.productId,
                            period = Plan.Period.Monthly,
                            price = monthly.formattedPrice,
                        ),
                    )
                }
                if (yearly != null) {
                    add(
                        Plan(
                            id = yearly.productId,
                            period = Plan.Period.Yearly,
                            price = yearly.formattedPrice,
                            saving = saving,
                        ),
                    )
                }
            }
        }

        internal fun toSignedPurchase(
            orderId: String?,
            products: List<String>,
            purchaseToken: String,
        ): SignedPurchase? {
            val token = purchaseToken.trim()
            if (token.isEmpty()) return null
            val productId = products.firstOrNull { it in Plan.identifiers }
                ?: products.firstOrNull()?.takeIf { it.isNotBlank() }
                ?: return null
            return SignedPurchase(
                id = orderId?.takeIf { it.isNotBlank() } ?: token,
                productId = productId,
                signed = token,
            )
        }

        internal fun mapBillingError(responseCode: Int, debugMessage: String): StoreError =
            when (responseCode) {
                BillingClient.BillingResponseCode.BILLING_UNAVAILABLE,
                BillingClient.BillingResponseCode.FEATURE_NOT_SUPPORTED,
                -> StoreError.Unavailable(
                    "Google Play Billing is not available on this device. Manage your plan at amux.sh.",
                )
                BillingClient.BillingResponseCode.SERVICE_UNAVAILABLE,
                BillingClient.BillingResponseCode.SERVICE_DISCONNECTED,
                BillingClient.BillingResponseCode.NETWORK_ERROR,
                -> StoreError.Unavailable(
                    "Could not reach Google Play right now. Check your connection and try again.",
                )
                else -> {
                    val detail = debugMessage.trim()
                    val message = if (detail.isNotEmpty()) {
                        "Google Play could not complete that request: $detail"
                    } else {
                        "Google Play could not complete that request. Try again in a moment."
                    }
                    StoreError.Failed(message)
                }
            }
    }
}
