package sh.amux.core.runtime

import java.net.URI
import java.nio.charset.StandardCharsets
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class AmuxCloudTest {
    private fun jsonResponse(code: Int, body: String): CloudHttpResponse =
        CloudHttpResponse(code, body.toByteArray(StandardCharsets.UTF_8))

    @Test
    fun pkceChallengeAndAuthorizeUrlFollowCloudContract() {
        // RFC 7636 Appendix B test vector.
        val verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        assertEquals(
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            AmuxCloudService.challenge(verifier),
        )

        val cloud = AmuxCloudService(transport = { error("no network") })
        assertEquals("https://amux.sh/privacy-policy/", CloudEndpoint.Production.privacyUrl)
        assertEquals(
            "https://staging.amux.sh/privacy-policy/",
            CloudEndpoint.Production.copy(baseUrl = "https://staging.amux.sh/").privacyUrl,
        )
        val account = SignedInAccount(AccountId("u1"), "ada+phone@example.com")
        val url = cloud.authorizeUrl(
            verifier = verifier,
            state = "state-123",
            intent = SignInIntent.Returning(account),
        )
        assertTrue(url.startsWith("https://amux.sh/connect/authorize?"))
        assertTrue(url.contains("client_id=mobile"))
        assertTrue(url.contains("response_type=code"))
        assertTrue(url.contains("redirect_uri=amux%3A%2F%2Fcallback"))
        assertTrue(url.contains("code_challenge_method=S256"))
        assertTrue(url.contains("prompt=select_account"))
        // `+` in email MUST be encoded as `%2B`, never left unencoded.
        assertTrue("expected %2B in login_hint, got $url", url.contains("login_hint=ada%2Bphone%40example.com"))

        // Callback parsing verifies state and errors.
        assertEquals(
            "abc",
            AmuxCloudService.parseCallbackCode("amux://callback?code=abc&state=state-123", "state-123"),
        )
        try {
            AmuxCloudService.parseCallbackCode("amux://callback?code=abc&state=wrong", "state-123")
            fail("expected state mismatch")
        } catch (e: CloudException) {
            assertTrue(e.error is CloudError.Refused)
        }
    }

    @Test
    fun signInHandOverAndGraphQlAccessParsingCoverPurchasedGrantAndUnknownTypename() = runTest {
        val requests = mutableListOf<CloudHttpRequest>()
        var graphQlBody = ""
        val cloud = AmuxCloudService(
            transport = { req ->
                requests.add(req)
                when {
                    req.url.endsWith("/connect/token") -> jsonResponse(
                        200,
                        """{"access_token":"tok-1","refresh_token":"ref-1","expires_in":3600}""",
                    )
                    req.url.endsWith("/connect/userinfo") -> jsonResponse(
                        200,
                        """{"sub":"acct-1","email":"ada@example.com","name":"Ada","picture":"https://amux.sh/p.png"}""",
                    )
                    req.url.endsWith("/api/graphql") -> jsonResponse(200, graphQlBody)
                    req.url.endsWith("/api/purchases") -> jsonResponse(202, "{}")
                    else -> jsonResponse(404, "{}")
                }
            },
        )

        val signedIn = cloud.signIn(SignInIntent.Adding) { authUrl, scheme ->
            assertEquals("amux", scheme)
            val state = URI(authUrl).rawQuery.split('&')
                .first { it.startsWith("state=") }
                .substringAfter('=')
            "amux://callback?code=auth-code&state=$state"
        }
        assertEquals(AccountId("acct-1"), signedIn.id)
        assertEquals("ada@example.com", signedIn.email)
        assertEquals("https://amux.sh/p.png", signedIn.pictureUrl)

        // handOver returns refresh token exactly once.
        val handover = cloud.handOver(signedIn.id)
        assertEquals(Handover("https://amux.sh", "mobile", "ref-1"), handover)
        assertNull(cloud.handOver(signedIn.id))

        // 1. Purchased via REVENUE_CAT renewing.
        graphQlBody = """
            {"data":{"me":{"access":{
              "pro":true,
              "until":"2026-12-01T00:00:00Z",
              "grant":{"__typename":"Purchased","provider":"REVENUE_CAT","willRenew":true,"entitledUntil":"2026-11-30T00:00:00Z"}
            }}}}
        """.trimIndent()
        val rc = cloud.entitlement(signedIn.id)
        assertEquals(
            Entitlement.Active(
                grant = Grant.Purchased(EntitlementSource.AppStore),
                renewsAtMs = AmuxCloudService.parseIsoTimestampMs("2026-11-30T00:00:00Z"),
            ),
            rc,
        )

        // 2. Purchased via Stripe with willRenew=false -> renewsAtMs is null.
        graphQlBody = """
            {"data":{"me":{"access":{
              "pro":true,
              "until":"2026-12-01T00:00:00Z",
              "grant":{"__typename":"Purchased","provider":"STRIPE","willRenew":false,"entitledUntil":"2026-12-01T00:00:00Z"}
            }}}}
        """.trimIndent()
        assertEquals(
            Entitlement.Active(grant = Grant.Purchased(EntitlementSource.Web), renewsAtMs = null),
            cloud.entitlement(signedIn.id),
        )

        // 3. Unknown future __typename reads cleanly as Grant.Granted.
        graphQlBody = """
            {"data":{"me":{"access":{
              "pro":true,
              "until":null,
              "grant":{"__typename":"EmployeeGrant"}
            }}}}
        """.trimIndent()
        assertEquals(
            Entitlement.Active(grant = Grant.Granted, renewsAtMs = null),
            cloud.entitlement(signedIn.id),
        )

        // 4. Lapsed subscription when pro=false and until present.
        graphQlBody = """
            {"data":{"me":{"access":{
              "pro":false,
              "until":"2026-01-01T00:00:00Z",
              "grant":{"__typename":"Purchased","provider":"STRIPE","willRenew":false}
            }}}}
        """.trimIndent()
        assertEquals(
            Entitlement.Lapsed(
                grant = Grant.Purchased(EntitlementSource.Web),
                endedAtMs = AmuxCloudService.parseIsoTimestampMs("2026-01-01T00:00:00Z")!!,
            ),
            cloud.entitlement(signedIn.id),
        )

        // 5. Purchase recording accepts 202.
        cloud.recordPurchase(signedIn.id, "signed.jws.payload")
        assertTrue(requests.any { it.url.endsWith("/api/purchases") && it.method == "POST" })
    }

    @Test
    fun requestDeletionHandles409RevenueCatStripePortalAndFallback() = runTest {
        var deleteResponse = jsonResponse(409, """{"error":"active_subscription","provider":"revenuecat"}""")
        var portalResponse = jsonResponse(200, """{"url":"https://billing.stripe.com/session/123"}""")

        val cloud = AmuxCloudService(
            transport = { req ->
                when {
                    req.url.endsWith("/connect/userinfo") -> jsonResponse(
                        200,
                        """{"sub":"acct-1","email":"ada@example.com"}""",
                    )
                    req.url.endsWith("/api/account") && req.method == "DELETE" -> deleteResponse
                    req.url.endsWith("/api/billing/stripe/portal") -> portalResponse
                    else -> jsonResponse(500, "{}")
                }
            },
        )
        cloud.lend { "lent-bearer" }

        // RevenueCat 409 routes to App Store subscriptions page.
        val rcBlocked = cloud.requestDeletion(AccountId("acct-1"), "ADA@EXAMPLE.COM")
        assertEquals(
            DeletionOutcome.BlockedByRenewal(
                EntitlementSource.AppStore,
                AmuxCloudService.APP_STORE_SUBSCRIPTIONS_URL,
            ),
            rcBlocked,
        )

        // Stripe 409 asks billing portal for a one-time link.
        deleteResponse = jsonResponse(409, """{"error":"active_subscription","provider":"stripe"}""")
        val stripeBlocked = cloud.requestDeletion(AccountId("acct-1"), "ada@example.com")
        assertEquals(
            DeletionOutcome.BlockedByRenewal(
                EntitlementSource.Web,
                "https://billing.stripe.com/session/123",
            ),
            stripeBlocked,
        )

        // Portal failure falls back to amux.sh/account.
        portalResponse = jsonResponse(500, """{"error":"unavailable"}""")
        val fallbackBlocked = cloud.requestDeletion(AccountId("acct-1"), "ada@example.com")
        assertEquals(
            DeletionOutcome.BlockedByRenewal(
                EntitlementSource.Web,
                "https://amux.sh/account",
            ),
            fallbackBlocked,
        )

        // Payment required 403 maps to standard wording.
        assertEquals(
            CloudError.Refused("this account has no subscription"),
            AmuxCloudService.refusal(
                403,
                """{"error":"payment_required"}""".toByteArray(StandardCharsets.UTF_8),
            ),
        )

        // Wrong email refused before DELETE is sent.
        try {
            cloud.requestDeletion(AccountId("acct-1"), "wrong@example.com")
            fail("expected email refusal")
        } catch (e: CloudException) {
            assertNotNull(e.error as? CloudError.Refused)
        }
    }
}
