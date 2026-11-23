package sh.amux.core.runtime

import java.nio.file.Files
import java.util.UUID
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.AccountBinding
import sh.amux.values.AccountView
import sh.amux.values.HostView
import sh.amux.values.ProfileView
import sh.amux.values.Reach
import sh.amux.values.RelayLink
import sh.amux.values.RouteKind
import sh.amux.values.SignInOfferOutcome
import sh.amux.values.SignInOfferView

class AccountRegistryTest {
    // Writes land before the next line, so a reload reads them.
    private fun registry(file: java.io.File): AccountRegistry = AccountRegistry(
        file = file,
        makeStores = { StoreBundle(account = it, scope = CoroutineScope(Dispatchers.Unconfined)) },
        ioDispatcher = Dispatchers.Unconfined,
        persistDelayMs = 0L,
    )

    private fun profileView(
        id: String,
        subject: String = "",
        email: String = "",
        name: String = "",
        binding: AccountBinding = if (subject.isEmpty()) AccountBinding.Unbound else AccountBinding.SignedIn,
        pro: Boolean? = null,
    ): ProfileView = ProfileView(
        id = id,
        label = id,
        subject = subject,
        account = AccountView(
            binding = binding,
            email = email,
            name = name,
            picture = "",
            relay = RelayLink.Connected,
            pro = pro,
        ),
    )

    @Test
    fun selectsSwitchesPersistsAndLeavesAccountsCleanly() {
        val dir = Files.createTempDirectory("amux-registry-test").toFile()
        try {
            val file = dir.resolve("accounts.json")
            val registry = registry(file)

            val unbound = profileView(id = "p0")
            val ada = profileView(id = "p1", subject = "ada", email = "ada@example.com", name = "Ada")
            val grace = profileView(
                id = "p2",
                subject = "grace",
                email = "grace@example.com",
                name = "Grace",
                binding = AccountBinding.SignedOut,
            )

            // Starts on unbound profile.
            registry.show(listOf(unbound))
            assertNull(registry.selected)
            assertEquals("p0", registry.profile)

            // Awaited selection takes effect as soon as the profile appears.
            registry.select(AccountId("ada"))
            registry.show(listOf(ada, grace))
            assertEquals(AccountId("ada"), registry.selected)
            assertEquals("p1", registry.profile)

            // Update cached hosts/attention and entitlement -> persisted to disk.
            registry.saw(hosts = 2, attention = 1, forAccount = AccountId("ada"))
            registry.entitlement(
                Entitlement.Active(Grant.Purchased(EntitlementSource.Web), null),
                forAccount = AccountId("ada"),
            )
            assertEquals("2 devices", registry.selectedAccount?.line)

            // Late answer for non-selected account is dropped.
            assertNull(registry.accept("stale", forAccount = AccountId("grace")))
            assertEquals(1, registry.dropped)
            assertEquals("fresh", registry.accept("fresh", forAccount = AccountId("ada")))

            // Reload from disk preserves selected account and cached summary.
            val reloaded = registry(file)
            reloaded.show(listOf(ada, grace))
            assertEquals(AccountId("ada"), reloaded.selected)
            assertEquals(2, reloaded.selectedAccount?.hosts)
            assertTrue(reloaded.selectedAccount?.entitlement is Entitlement.Active)

            // Leaving selected account switches to remaining account.
            reloaded.leave(AccountId("ada"))
            assertEquals(AccountId("grace"), reloaded.selected)
            assertEquals("Signed out", reloaded.selectedAccount?.line)
        } finally {
            dir.deleteRecursively()
        }
    }

    @Test
    fun signInStoreMismatchDeletionStoreAndSignInOffersBehaveDeterministically() = runTest {
        val wanted = SignedInAccount(AccountId("ada"), "ada@example.com", "Ada")
        val got = SignedInAccount(AccountId("bob"), "bob@example.com", "Bob")
        var forgotten: AccountId? = null
        val fakeCloud = object : CloudService {
            override suspend fun signIn(intent: SignInIntent, presenter: WebAuthPresenter) = got
            override suspend fun handOver(id: AccountId): Handover? = null
            override suspend fun forgetSession(id: AccountId) { forgotten = id }
            override suspend fun lend(lender: suspend (AccountId) -> String?) = Unit
            override suspend fun entitlement(id: AccountId): Entitlement = Entitlement.None
            override suspend fun recordPurchase(id: AccountId, purchase: SignedPurchase) = Unit
            override suspend fun requestDeletion(id: AccountId, confirmedEmail: String): DeletionOutcome =
                DeletionOutcome.BlockedByRenewal(EntitlementSource.Web, "https://amux.sh/portal")
            override suspend fun registerDevice(id: AccountId, host: HostId, device: PushDevice) = Unit
            override suspend fun removeDevice(id: AccountId, host: HostId) = Unit
        }

        // Mismatch flow + discard forgets unkept session.
        val signInStore = SignInStore()
        val returned = signInStore.signIn(
            intent = SignInIntent.Returning(wanted),
            cloud = fakeCloud,
            presenter = { _, _ -> "amux://callback?code=1" },
            keep = { null },
        )
        assertNull(returned)
        assertEquals(SignInStore.Phase.Mismatched(wanted, got), signInStore.snapshot.phase)
        signInStore.discard(fakeCloud)
        assertEquals(SignInStore.Phase.Ready, signInStore.snapshot.phase)
        assertEquals(got.id, forgotten)

        // RemovalStore asks and dismisses.
        val removal = RemovalStore()
        removal.ask(wanted.id)
        assertEquals(wanted.id, removal.current)
        removal.dismiss()
        assertNull(removal.current)

        // DeletionStore confirms case-insensitively and preserves typed text across blocked -> askAgain.
        val deletion = DeletionStore()
        deletion.ask(wanted.id)
        deletion.setTyped("  ADA@EXAMPLE.COM ")
        assertTrue(deletion.confirms("ada@example.com"))
        assertFalse(deletion.confirms("bob@example.com"))
        val outcome = deletion.delete(fakeCloud)
        assertEquals(
            DeletionOutcome.BlockedByRenewal(EntitlementSource.Web, "https://amux.sh/portal"),
            outcome,
        )
        assertEquals(
            DeletionStore.Phase.Blocked(EntitlementSource.Web, "https://amux.sh/portal"),
            deletion.snapshot.phase,
        )
        deletion.askAgain()
        assertEquals(DeletionStore.Phase.Asking, deletion.snapshot.phase)
        assertEquals("  ADA@EXAMPLE.COM ", deletion.snapshot.typed)

        // SignInOffers prioritizes received offers, then newly paired signed-out hosts.
        val offers = SignInOffers()
        val deskId = HostId(UUID.fromString("12345678-1234-1234-1234-123456789abc"))
        val host = HostView(
            hostId = deskId.bytes,
            link = 0L,
            sameNetwork = false,
            name = "desk",
            local = false,
            trusted = true,
            candidate = false,
            reach = Reach.Online(RouteKind.Direct),
            addrs = emptyList(),
            relayOnly = false,
            pairingOpen = false,
            current = true,
            providers = emptyList(),
            signedIn = false,
        )
        val hostsState = HostsState(
            hosts = listOf(host),
            account = AccountView(
                binding = AccountBinding.SignedIn,
                email = "ada@example.com",
                name = "Ada",
                picture = "",
                relay = RelayLink.Connected,
            ),
        )
        offers.paired(deskId)
        val offering = offers.prompt(hostsState)
        assertEquals(SignInPrompt.Offering(host, "ada@example.com"), offering)
        assertTrue(offers.firstShown(offering!!))
        assertFalse(offers.firstShown(offering))

        val incoming = SignInOfferView(deskId.bytes, "desk", "bob@example.com", "Bob")
        offers.listed(listOf(incoming))
        assertEquals(SignInPrompt.Offered(incoming), offers.prompt(hostsState))
        offers.answers(deskId.bytes)
        offers.answered(Result.success(SignInOfferOutcome.Unreachable), deskId.bytes)
        assertEquals(SignInOffers.Failure.Unreachable("desk"), offers.snapshot.failure)
        offers.listed(emptyList())
        assertNull(offers.snapshot.failure)
        offers.notOffering(deskId)
        assertNull(offers.prompt(hostsState))
    }
}
