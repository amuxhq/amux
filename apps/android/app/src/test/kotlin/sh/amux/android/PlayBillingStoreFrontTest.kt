package sh.amux.android

import com.android.billingclient.api.BillingClient
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.core.runtime.Plan
import sh.amux.core.runtime.SignedPurchase
import sh.amux.core.runtime.StoreError

class PlayBillingStoreFrontTest {
    @Test
    fun selectBasePlanOfferPrefersMatchingBasePlanWithoutPromoOverride() {
        val offers = listOf(
            PlayBillingStoreFront.OfferCandidate(
                basePlanId = "p1m",
                offerId = "intro-trial",
                offerToken = "tok-trial",
                formattedPrice = "$10.00",
                priceAmountMicros = 10_000_000L,
            ),
            PlayBillingStoreFront.OfferCandidate(
                basePlanId = "p1m",
                offerId = null,
                offerToken = "tok-base",
                formattedPrice = "$10.00",
                priceAmountMicros = 10_000_000L,
            ),
            PlayBillingStoreFront.OfferCandidate(
                basePlanId = "legacy",
                offerId = null,
                offerToken = "tok-legacy",
                formattedPrice = "$12.00",
                priceAmountMicros = 12_000_000L,
            ),
        )

        val chosen = PlayBillingStoreFront.selectBasePlanOffer(offers, "p1m")
        assertEquals("tok-base", chosen?.offerToken)
    }

    @Test
    fun buildPlansOrdersMonthlyThenYearlyAndCalculatesSavingPercentage() {
        val plans = PlayBillingStoreFront.buildPlans(
            monthly = PlayBillingStoreFront.PricedOffer(
                productId = Plan.MONTHLY_ID,
                formattedPrice = "$9.99",
                priceAmountMicros = 9_990_000L,
            ),
            yearly = PlayBillingStoreFront.PricedOffer(
                productId = Plan.YEARLY_ID,
                formattedPrice = "$99.99",
                priceAmountMicros = 99_990_000L,
            ),
        )

        assertEquals(2, plans.size)
        assertEquals(Plan(Plan.MONTHLY_ID, Plan.Period.Monthly, "$9.99", null), plans[0])
        assertEquals(Plan(Plan.YEARLY_ID, Plan.Period.Yearly, "$99.99", "Save 17%"), plans[1])
    }

    @Test
    fun buildPlansOmitsSavingWhenYearlyIsNotCheaper() {
        val plans = PlayBillingStoreFront.buildPlans(
            monthly = PlayBillingStoreFront.PricedOffer(
                productId = Plan.MONTHLY_ID,
                formattedPrice = "$10.00",
                priceAmountMicros = 10_000_000L,
            ),
            yearly = PlayBillingStoreFront.PricedOffer(
                productId = Plan.YEARLY_ID,
                formattedPrice = "$120.00",
                priceAmountMicros = 120_000_000L,
            ),
        )

        assertNull(plans.last().saving)
    }

    @Test
    fun toSignedPurchaseSelectsKnownPlanProductAndFallsBackToTokenForId() {
        val withOrder = PlayBillingStoreFront.toSignedPurchase(
            orderId = "GPA.1234-5678",
            products = listOf("other", Plan.YEARLY_ID),
            purchaseToken = "tok-abc",
        )
        assertEquals(
            SignedPurchase(
                id = "GPA.1234-5678",
                productId = Plan.YEARLY_ID,
                signed = "tok-abc",
            ),
            withOrder,
        )

        val withoutOrder = PlayBillingStoreFront.toSignedPurchase(
            orderId = null,
            products = listOf(Plan.MONTHLY_ID),
            purchaseToken = "tok-def",
        )
        assertEquals(
            SignedPurchase(
                id = "tok-def",
                productId = Plan.MONTHLY_ID,
                signed = "tok-def",
            ),
            withoutOrder,
        )

        assertNull(PlayBillingStoreFront.toSignedPurchase("GPA.1", listOf(Plan.MONTHLY_ID), "  "))
        assertNull(PlayBillingStoreFront.toSignedPurchase("GPA.1", emptyList(), "tok-xyz"))
    }

    @Test
    fun mapBillingErrorDistinguishesUnavailableFromFailed() {
        assertTrue(
            PlayBillingStoreFront.mapBillingError(
                BillingClient.BillingResponseCode.BILLING_UNAVAILABLE,
                "",
            ) is StoreError.Unavailable,
        )
        assertTrue(
            PlayBillingStoreFront.mapBillingError(
                BillingClient.BillingResponseCode.NETWORK_ERROR,
                "",
            ) is StoreError.Unavailable,
        )
        val failed = PlayBillingStoreFront.mapBillingError(
            BillingClient.BillingResponseCode.ERROR,
            "play error",
        )
        assertTrue(failed is StoreError.Failed)
        assertTrue((failed as StoreError.Failed).message.contains("play error"))
    }
}
