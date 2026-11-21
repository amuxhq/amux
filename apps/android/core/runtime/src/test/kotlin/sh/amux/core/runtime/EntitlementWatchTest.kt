package sh.amux.core.runtime

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.AccountBinding
import sh.amux.values.AccountView
import sh.amux.values.ProfileView
import sh.amux.values.RelayLink

class EntitlementWatchTest {
    private fun profile(subject: String, pro: Boolean?): ProfileView = ProfileView(
        id = "prof-$subject",
        label = subject,
        subject = subject,
        account = AccountView(
            binding = AccountBinding.SignedIn,
            email = "$subject@example.com",
            name = subject,
            picture = "",
            relay = RelayLink.Connected,
            pro = pro,
        ),
    )

    @Test
    fun throttlesWithin30sForcesImmediatelyAndRelinksOnChangeOrRelayDisagreement() = runTest {
        var now = 100_000L
        var cloudAnswer: Entitlement = Entitlement.Active(Grant.Purchased(EntitlementSource.Web), null)
        var reads = 0
        var relinks = 0

        val registry = AccountRegistry(makeStores = { StoreBundle(account = it, scope = this) })
        val ada = AccountId("ada")
        registry.show(listOf(profile("ada", pro = false)))
        registry.stores?.showAccount(profile("ada", pro = false).account)

        val watch = EntitlementWatch(
            registry = registry,
            read = {
                reads += 1
                cloudAnswer
            },
            relink = {
                relinks += 1
                true
            },
            nowMs = { now },
        )

        // First look reads cloud, updates registry, and relinks because entitlement changed.
        assertTrue(watch.look(force = false))
        assertEquals(1, reads)
        assertEquals(1, relinks)
        assertEquals(cloudAnswer, registry.selectedAccount?.entitlement)

        // Within 30s -> throttled, no network read.
        now += 15_000L
        assertTrue(watch.look(force = false))
        assertEquals(1, reads)
        assertEquals(1, relinks)

        // Force bypasses throttle (after purchase) and always relinks.
        assertTrue(watch.look(force = true))
        assertEquals(2, reads)
        assertEquals(2, relinks)

        // After 30s with unchanged entitlement, relay still says pro=false -> disagrees -> relinks!
        now += EntitlementWatch.SPACING_MS
        assertTrue(watch.look(force = false))
        assertEquals(3, reads)
        assertEquals(3, relinks)

        // Once relay agrees (pro=true) and entitlement unchanged -> reads without relinking.
        registry.stores?.showAccount(profile("ada", pro = true).account)
        now += EntitlementWatch.SPACING_MS
        assertTrue(watch.look(force = false))
        assertEquals(4, reads)
        assertEquals(3, relinks)

        // Switching away mid-read drops stale answer.
        val bob = AccountId("bob")
        registry.show(listOf(profile("ada", pro = true), profile("bob", pro = false)))
        val switchingWatch = EntitlementWatch(
            registry = registry,
            read = {
                registry.select(bob)
                Entitlement.None
            },
            relink = {
                relinks += 1
                true
            },
            nowMs = { now },
        )
        assertFalse(switchingWatch.look(force = true))
        assertEquals(3, relinks)
    }
}
