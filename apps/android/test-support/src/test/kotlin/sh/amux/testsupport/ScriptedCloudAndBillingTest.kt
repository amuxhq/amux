package sh.amux.testsupport

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.CloudError
import sh.amux.core.runtime.CloudException
import sh.amux.core.runtime.DeletionOutcome
import sh.amux.core.runtime.Entitlement
import sh.amux.core.runtime.EntitlementSource
import sh.amux.core.runtime.Grant
import sh.amux.core.runtime.PaywallStore
import sh.amux.core.runtime.Plan
import sh.amux.core.runtime.PurchaseOutcome
import sh.amux.core.runtime.SignInIntent

class ScriptedCloudAndBillingTest {
    @Test
    fun cloudScriptParsesNullTokenDistinctFromAbsentToken() {
        val absent = CloudScript.fromJson("""{"entitlement":"none"}""").toState()
        assertEquals("scripted-connect-token", absent.token)
        assertEquals(Entitlement.None, absent.entitlement)

        val explicitNull = CloudScript.fromJson("""{"token":null,"source":"granted"}""").toState()
        assertNull(explicitNull.token)
        assertEquals(Entitlement.Active(Grant.Granted, null), explicitNull.entitlement)

        val blocked = CloudScript.fromJson(
            """{"deletion":"blockedByRenewal","source":"appStore","manageURL":"https://apps.apple.com/account/subscriptions"}""",
        ).toState()
        assertEquals(
            DeletionOutcome.BlockedByRenewal(
                EntitlementSource.AppStore,
                "https://apps.apple.com/account/subscriptions",
            ),
            blocked.deletion,
        )
    }

    @Test
    fun scriptedCloudAndBillingVerifyConfirmBeforeFinishOrder() = runTest {
        val cloud = ScriptedCloudService(
            ScriptedCloudState(
                entitlement = Entitlement.None,
                purchase = ScriptedCloudState.PurchaseRecording.Refused("card declined"),
            ),
        )
        val billing = ScriptedBillingStoreFront()
        val paywall = PaywallStore()

        paywall.load(billing)
        assertEquals(2, paywall.snapshot.plans.size)

        val outcome = paywall.buy(billing)
        assertTrue(outcome is PurchaseOutcome.Bought)
        val purchase = (outcome as PurchaseOutcome.Bought).purchase
        assertEquals(1, billing.held.size)

        // Cloud refusal must leave the transaction unfinished so retry can resend it.
        val refused = paywall.confirm(purchase, cloud, AccountId("ada"), billing)
        assertEquals(false, refused)
        assertEquals(1, billing.held.size)
        assertEquals(
            PaywallStore.Phase.Unconfirmed(PaywallStore.Unconfirmed.Refused("card declined")),
            paywall.snapshot.phase,
        )

        // Once cloud accepts, transaction is finished and entitlement becomes active.
        cloud.scripted = cloud.scripted.copy(purchase = ScriptedCloudState.PurchaseRecording.Accepted)
        val accepted = paywall.confirm(purchase, cloud, AccountId("ada"), billing)
        assertTrue(accepted)
        assertTrue(billing.held.isEmpty())
        val entitlement = cloud.entitlement(AccountId("ada"))
        assertTrue(entitlement is Entitlement.Active)
    }

    @Test
    fun scriptedCloudRecordsSignInCallsAndFailures() = runTest {
        val cloud = ScriptedCloudService(
            ScriptedCloudState(signIn = ScriptedCloudState.SignInOutcome.Refused("suspended")),
        )
        try {
            cloud.signIn(SignInIntent.Adding, ScriptedWebAuth())
            fail("expected CloudException")
        } catch (expected: CloudException) {
            assertEquals(CloudError.Refused("suspended"), expected.error)
        }
        assertEquals(listOf<CloudCall>(CloudCall.SignIn(SignInIntent.Adding)), cloud.calls)
        assertEquals(Plan.YEARLY_ID, BillingScript().toState().plans.last().id)
    }
}
