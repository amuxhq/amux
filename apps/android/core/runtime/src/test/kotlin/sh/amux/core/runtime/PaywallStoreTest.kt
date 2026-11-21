package sh.amux.core.runtime

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PaywallStoreTest {
    @Test
    fun loadPreservesEntitledPhaseWhenBillingStoreFrontIsUnavailable() = runTest {
        val entitlement = Entitlement.Active(
            grant = Grant.Purchased(EntitlementSource.Web),
            renewsAtMs = 1_800_000_000_000L,
        )
        val store = PaywallStore()
        store.entitled(entitlement)
        assertEquals(PaywallStore.Phase.Entitled(Grant.Purchased(EntitlementSource.Web)), store.snapshot.phase)

        store.load(StubBillingStoreFront())

        assertEquals(
            PaywallStore.Phase.Entitled(Grant.Purchased(EntitlementSource.Web)),
            store.snapshot.phase,
        )
        assertTrue(store.snapshot.entitled)
        assertEquals(entitlement, store.snapshot.entitlement)
    }

    @Test
    fun loadFailsWhenBillingStoreFrontIsUnavailableAndNotEntitled() = runTest {
        val store = PaywallStore()
        store.load(StubBillingStoreFront())
        assertTrue(store.snapshot.phase is PaywallStore.Phase.Failed)
        assertFalse(store.snapshot.entitled)
    }

    @Test
    fun switchingFromEntitledToNoneOrLapsedResetsEntitledPhaseToReady() {
        val store = PaywallStore()
        store.entitled(Entitlement.Active(Grant.Granted))
        assertEquals(PaywallStore.Phase.Entitled(Grant.Granted), store.snapshot.phase)

        // Switching to a free account resets stale Entitled phase back to Ready.
        store.entitled(Entitlement.None)
        assertEquals(PaywallStore.Phase.Ready, store.snapshot.phase)
        assertFalse(store.snapshot.entitled)

        // Switching from Pro to a lapsed account also resets Entitled phase back to Ready.
        store.entitled(Entitlement.Active(Grant.Purchased(EntitlementSource.Web)))
        assertTrue(store.snapshot.phase is PaywallStore.Phase.Entitled)
        store.entitled(
            Entitlement.Lapsed(
                grant = Grant.Purchased(EntitlementSource.Web),
                endedAtMs = 1_700_000_000_000L,
            ),
        )
        assertEquals(PaywallStore.Phase.Ready, store.snapshot.phase)
        assertFalse(store.snapshot.entitled)
    }
}
