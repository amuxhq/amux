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
    fun selectBasePlanOfferPrefersEligibleIntroDiscountAndFallsBackToBasePlan() {
        val baseOnly = listOf(
            PlayBillingStoreFront.OfferCandidate(
                basePlanId = "p1y",
                offerId = null,
                offerToken = "tok-base",
                formattedPrice = "$99.99",
                priceAmountMicros = 99_990_000L,
            ),
            PlayBillingStoreFront.OfferCandidate(
                basePlanId = "legacy",
                offerId = "old-promo",
                offerToken = "tok-legacy",
                formattedPrice = "$99.99",
                priceAmountMicros = 99_990_000L,
                introFormattedPrice = "$79.99",
                introPriceAmountMicros = 79_990_000L,
            ),
        )
        assertEquals("tok-base", PlayBillingStoreFront.selectBasePlanOffer(baseOnly, "p1y")?.offerToken)

        val withIntro = baseOnly + PlayBillingStoreFront.OfferCandidate(
            basePlanId = "p1y",
            offerId = "intro-annual",
            offerToken = "tok-intro",
            formattedPrice = "$99.99",
            priceAmountMicros = 99_990_000L,
            introFormattedPrice = "$79.99",
            introPriceAmountMicros = 79_990_000L,
        )
        assertEquals("tok-intro", PlayBillingStoreFront.selectBasePlanOffer(withIntro, "p1y")?.offerToken)
    }

    @Test
    fun buildPlansShowsIntroductoryFirstYearPriceWhenEligible() {
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
                introFormattedPrice = "$79.99",
                introPriceAmountMicros = 79_990_000L,
            ),
        )

        assertEquals(Plan(Plan.MONTHLY_ID, Plan.Period.Monthly, "$9.99", null), plans[0])
        assertEquals(
            Plan(Plan.YEARLY_ID, Plan.Period.Yearly, "$99.99", "First year $79.99"),
            plans[1],
        )
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
