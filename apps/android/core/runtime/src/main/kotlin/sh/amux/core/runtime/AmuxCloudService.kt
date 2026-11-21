package sh.amux.core.runtime

import java.io.IOException
import java.net.SocketTimeoutException
import java.net.URI
import java.net.URLDecoder
import java.net.URLEncoder
import java.nio.charset.StandardCharsets
import java.security.MessageDigest
import java.security.SecureRandom
import java.time.Instant
import java.time.OffsetDateTime
import java.util.Base64
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import okhttp3.Call
import okhttp3.Callback
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

/** Where the account service lives and how this app identifies itself to it. */
data class CloudEndpoint(
    val baseUrl: String,
    val clientId: String,
    val callbackUrl: String,
    val scopes: List<String>,
) {
    private val normalizedBase: String = baseUrl.trimEnd('/')

    val host: String
        get() = try {
            URI(normalizedBase).host?.takeIf { it.isNotBlank() } ?: normalizedBase
        } catch (_: Exception) {
            normalizedBase
        }

    val callbackScheme: String
        get() = try {
            URI(callbackUrl).scheme?.takeIf { it.isNotBlank() } ?: "amux"
        } catch (_: Exception) {
            "amux"
        }

    val supportUrl: String get() = "$normalizedBase/support"
    val accountPageUrl: String get() = "$normalizedBase/account"
    val authorizeUrl: String get() = "$normalizedBase/connect/authorize"
    val tokenUrl: String get() = "$normalizedBase/connect/token"
    val userinfoUrl: String get() = "$normalizedBase/connect/userinfo"
    val graphQlUrl: String get() = "$normalizedBase/api/graphql"
    val purchasesUrl: String get() = "$normalizedBase/api/purchases"
    val accountUrl: String get() = "$normalizedBase/api/account"
    val stripePortalUrl: String get() = "$normalizedBase/api/billing/stripe/portal"
    fun deviceUrl(host: HostId): String = "$normalizedBase/api/devices/$host"

    companion object {
        private val signedInScopes = listOf("openid", "profile", "email", "offline_access", "api")

        val Production: CloudEndpoint = CloudEndpoint(
            baseUrl = "https://amux.sh",
            clientId = "mobile",
            callbackUrl = "amux://callback",
            scopes = signedInScopes,
        )
    }
}

data class CloudHttpRequest(
    val method: String,
    val url: String,
    val headers: Map<String, String> = emptyMap(),
    val body: ByteArray? = null,
    val contentType: String? = null,
)

data class CloudHttpResponse(
    val statusCode: Int,
    val body: ByteArray,
)

/** HTTP transport seam so [AmuxCloudService] can be unit-tested without a network. */
fun interface CloudTransport {
    suspend fun send(request: CloudHttpRequest): CloudHttpResponse
}

/** Production [CloudTransport] backed by OkHttp async [Call.enqueue]. */
class OkHttpCloudTransport(
    private val client: OkHttpClient = OkHttpClient(),
) : CloudTransport {
    override suspend fun send(request: CloudHttpRequest): CloudHttpResponse {
        val builder = Request.Builder().url(request.url)
        for ((name, value) in request.headers) {
            builder.header(name, value)
        }
        val mediaType = request.contentType?.toMediaType()
        val requestBody = when {
            request.body != null -> request.body.toRequestBody(mediaType)
            request.method in METHODS_REQUIRING_BODY -> ByteArray(0).toRequestBody(mediaType)
            else -> null
        }
        builder.method(request.method, requestBody)
        val call = client.newCall(builder.build())
        return suspendCancellableCoroutine { cont ->
            cont.invokeOnCancellation { call.cancel() }
            call.enqueue(object : Callback {
                override fun onFailure(call: Call, e: IOException) {
                    if (cont.isActive) {
                        cont.resumeWithException(e)
                    }
                }

                override fun onResponse(call: Call, response: Response) {
                    response.use { res ->
                        val bytes = try {
                            res.body.bytes()
                        } catch (e: IOException) {
                            if (cont.isActive) cont.resumeWithException(e)
                            return
                        }
                        if (cont.isActive) {
                            cont.resume(CloudHttpResponse(res.code, bytes))
                        }
                    }
                }
            })
        }
    }

    companion object {
        private val METHODS_REQUIRING_BODY = setOf("POST", "PUT", "PATCH")
    }
}

/**
 * Production implementation of [CloudService] matching the `amux.sh` HTTP and
 * OAuth PKCE contract (`docs/CLOUD.md`).
 */
class AmuxCloudService(
    val endpoint: CloudEndpoint = CloudEndpoint.Production,
    private val transport: CloudTransport = OkHttpCloudTransport(),
    private val nowMs: () -> Long = System::currentTimeMillis,
) : CloudService {
    private data class Session(
        val accessToken: String,
        var refreshToken: String?,
        val expiresAtMs: Long,
    )

    private val mutex = Mutex()
    private val sessions = mutableMapOf<AccountId, Session>()
    private var lender: (suspend (AccountId) -> String?)? = null

    override suspend fun handOver(id: AccountId): Handover? = mutex.withLock {
        val session = sessions[id] ?: return null
        val refresh = session.refreshToken ?: return null
        session.refreshToken = null
        Handover(
            cloudUrl = endpoint.baseUrl,
            clientId = endpoint.clientId,
            refreshToken = refresh,
        )
    }

    override suspend fun forgetSession(id: AccountId) {
        mutex.withLock {
            sessions.remove(id)
        }
    }

    override suspend fun lend(lender: suspend (AccountId) -> String?) {
        mutex.withLock {
            this.lender = lender
        }
    }

    // --- Signing in --------------------------------------------------------

    override suspend fun signIn(
        intent: SignInIntent,
        presenter: WebAuthPresenter,
    ): SignedInAccount {
        val verifier = randomToken()
        val state = randomToken()
        val authUrl = authorizeUrl(verifier = verifier, state = state, intent = intent)
        val callback = presenter.present(authUrl, endpoint.callbackScheme)
        val code = parseCallbackCode(callback, expectedState = state)
        val issued = exchange(
            mapOf(
                "grant_type" to "authorization_code",
                "code" to code,
                "redirect_uri" to endpoint.callbackUrl,
                "client_id" to endpoint.clientId,
                "code_verifier" to verifier,
            ),
        )
        val who = fetchUserInfo(issued.accessToken)
        val id = AccountId(who.sub)
        val ttlSeconds = (issued.expiresIn ?: 3600).toLong()
        mutex.withLock {
            sessions[id] = Session(
                accessToken = issued.accessToken,
                refreshToken = issued.refreshToken,
                expiresAtMs = nowMs() + ttlSeconds * 1_000L,
            )
        }
        return SignedInAccount(
            id = id,
            email = who.email.orEmpty(),
            displayName = who.name,
            pictureUrl = who.picture?.takeIf { it.isNotBlank() },
        )
    }

    /** Builds the PKCE authorization URL for the browser sign-in flow. */
    fun authorizeUrl(verifier: String, state: String, intent: SignInIntent): String {
        val params = mutableListOf(
            "client_id" to endpoint.clientId,
            "response_type" to "code",
            "redirect_uri" to endpoint.callbackUrl,
            "scope" to endpoint.scopes.joinToString(" "),
            "code_challenge" to challenge(verifier),
            "code_challenge_method" to "S256",
            "state" to state,
            "prompt" to "select_account",
        )
        if (intent is SignInIntent.Returning) {
            params += "login_hint" to intent.account.email
        }
        val query = params.joinToString("&") { (k, v) ->
            "${percentEncode(k)}=${percentEncode(v)}"
        }
        return "${endpoint.authorizeUrl}?$query"
    }

    // --- Entitlement & Purchases -------------------------------------------

    override suspend fun entitlement(id: AccountId): Entitlement {
        val body = AmuxJson.json.encodeToString(GraphQlRequest(ACCESS_QUERY))
            .toByteArray(StandardCharsets.UTF_8)
        val answer: GraphQlAnswer<MePayload> = askJson(
            CloudHttpRequest(
                method = "POST",
                url = endpoint.graphQlUrl,
                body = body,
                contentType = JSON_CONTENT_TYPE,
            ),
            accountId = id,
        )
        val access = answer.data?.me?.access ?: return Entitlement.None
        return access.toEntitlement()
    }

    override suspend fun recordPurchase(id: AccountId, signedTransaction: String) {
        val body = AmuxJson.json.encodeToString(PurchaseBody(signedTransaction))
            .toByteArray(StandardCharsets.UTF_8)
        val response = sendAuthorized(
            CloudHttpRequest(
                method = "POST",
                url = endpoint.purchasesUrl,
                body = body,
                contentType = JSON_CONTENT_TYPE,
            ),
            accountId = id,
        )
        if (response.statusCode !in 200..299) {
            throw CloudException(refusal(response.statusCode, response.body))
        }
    }

    // --- Push device registration ------------------------------------------

    override suspend fun registerDevice(id: AccountId, host: HostId, device: PushDevice) {
        val body = AmuxJson.json.encodeToString(
            DeviceBody(
                transport = device.transport,
                environment = device.environment.wireValue,
                token = device.token,
            ),
        ).toByteArray(StandardCharsets.UTF_8)
        val response = sendAuthorized(
            CloudHttpRequest(
                method = "PUT",
                url = endpoint.deviceUrl(host),
                body = body,
                contentType = JSON_CONTENT_TYPE,
            ),
            accountId = id,
        )
        if (response.statusCode !in 200..299) {
            throw CloudException(refusal(response.statusCode, response.body))
        }
    }

    override suspend fun removeDevice(id: AccountId, host: HostId) {
        val response = sendAuthorized(
            CloudHttpRequest(
                method = "DELETE",
                url = endpoint.deviceUrl(host),
            ),
            accountId = id,
        )
        if (response.statusCode !in 200..299) {
            throw CloudException(refusal(response.statusCode, response.body))
        }
    }

    // --- Account deletion --------------------------------------------------

    override suspend fun requestDeletion(
        id: AccountId,
        confirmedEmail: String,
    ): DeletionOutcome {
        val who = fetchUserInfo(bearer(id))
        val typed = confirmedEmail.trim()
        val expected = who.email.orEmpty()
        if (!typed.equals(expected, ignoreCase = true) || typed.isEmpty()) {
            throw CloudException(CloudError.Refused("that is not this account’s address"))
        }
        val response = sendAuthorized(
            CloudHttpRequest(
                method = "DELETE",
                url = endpoint.accountUrl,
            ),
            accountId = id,
        )
        return when (response.statusCode) {
            in 200..299 -> {
                mutex.withLock { sessions.remove(id) }
                DeletionOutcome.Deleted
            }
            401 -> throw CloudException(CloudError.Unauthenticated)
            409 -> {
                val blocked = decodeJson<BlockedPayload>(response.body)
                when (blocked.provider) {
                    "revenuecat" -> DeletionOutcome.BlockedByRenewal(
                        source = EntitlementSource.AppStore,
                        manageUrl = APP_STORE_SUBSCRIPTIONS_URL,
                    )
                    "google_play", "play_store" -> DeletionOutcome.BlockedByRenewal(
                        source = EntitlementSource.PlayStore,
                        manageUrl = PLAY_STORE_SUBSCRIPTIONS_URL,
                    )
                    else -> DeletionOutcome.BlockedByRenewal(
                        source = EntitlementSource.Web,
                        manageUrl = billingPortal(id),
                    )
                }
            }
            else -> throw CloudException(refusal(response.statusCode, response.body))
        }
    }

    private suspend fun billingPortal(id: AccountId): String {
        val session = try {
            askJson<SessionUrlPayload>(
                CloudHttpRequest(
                    method = "POST",
                    url = endpoint.stripePortalUrl,
                    body = "{}".toByteArray(StandardCharsets.UTF_8),
                    contentType = JSON_CONTENT_TYPE,
                ),
                accountId = id,
            )
        } catch (_: CloudException) {
            null
        }
        return session?.url?.takeIf { it.isNotBlank() } ?: endpoint.accountPageUrl
    }

    // --- Tokens & HTTP helpers ---------------------------------------------

    private suspend fun bearer(id: AccountId): String {
        val cached = mutex.withLock {
            val session = sessions[id]
            if (session != null &&
                session.accessToken.isNotEmpty() &&
                session.expiresAtMs > nowMs() + TOKEN_SLACK_MS
            ) {
                session.accessToken
            } else {
                null
            }
        }
        if (cached != null) return cached
        val currentLender = mutex.withLock { lender }
            ?: throw CloudException(CloudError.Unauthenticated)
        return currentLender(id) ?: throw CloudException(CloudError.Unauthenticated)
    }

    private suspend fun exchange(fields: Map<String, String>): IssuedToken {
        val formBody = encodeForm(fields).toByteArray(StandardCharsets.UTF_8)
        val response = sendRaw(
            CloudHttpRequest(
                method = "POST",
                url = endpoint.tokenUrl,
                body = formBody,
                contentType = FORM_CONTENT_TYPE,
            ),
        )
        if (response.statusCode !in 200..299) {
            throw CloudException(refusal(response.statusCode, response.body))
        }
        return decodeJson(response.body)
    }

    private suspend fun fetchUserInfo(bearer: String): UserInfoPayload {
        val response = sendRaw(
            CloudHttpRequest(
                method = "GET",
                url = endpoint.userinfoUrl,
                headers = mapOf("Authorization" to "Bearer $bearer"),
            ),
        )
        if (response.statusCode !in 200..299) {
            throw CloudException(refusal(response.statusCode, response.body))
        }
        return decodeJson(response.body)
    }

    private suspend inline fun <reified T> askJson(
        request: CloudHttpRequest,
        accountId: AccountId,
    ): T {
        val response = sendAuthorized(request, accountId)
        if (response.statusCode !in 200..299) {
            throw CloudException(refusal(response.statusCode, response.body))
        }
        return decodeJson(response.body)
    }

    private suspend fun sendAuthorized(
        request: CloudHttpRequest,
        accountId: AccountId,
    ): CloudHttpResponse {
        val token = bearer(accountId)
        val headers = request.headers + ("Authorization" to "Bearer $token")
        return sendRaw(request.copy(headers = headers))
    }

    private suspend fun sendRaw(request: CloudHttpRequest): CloudHttpResponse {
        return try {
            transport.send(request)
        } catch (e: CancellationException) {
            throw e
        } catch (_: SocketTimeoutException) {
            throw CloudException(CloudError.Timeout)
        } catch (e: CloudException) {
            throw e
        } catch (t: Throwable) {
            throw CloudException(CloudError.Network(t.message ?: t.toString()))
        }
    }

    private inline fun <reified T> decodeJson(body: ByteArray): T {
        return try {
            AmuxJson.json.decodeFromString<T>(body.toString(StandardCharsets.UTF_8))
        } catch (_: Throwable) {
            throw CloudException(
                CloudError.Refused("amux.sh answered in a shape this app does not know"),
            )
        }
    }

    // --- Wire DTOs ---------------------------------------------------------

    @Serializable
    private data class IssuedToken(
        @SerialName("access_token") val accessToken: String,
        @SerialName("refresh_token") val refreshToken: String? = null,
        @SerialName("expires_in") val expiresIn: Int? = null,
    )

    @Serializable
    private data class UserInfoPayload(
        @SerialName("sub") val sub: String,
        @SerialName("email") val email: String? = null,
        @SerialName("name") val name: String? = null,
        @SerialName("picture") val picture: String? = null,
    )

    @Serializable
    private data class PurchaseBody(
        @SerialName("signed_transaction") val signedTransaction: String,
    )

    @Serializable
    private data class DeviceBody(
        @SerialName("transport") val transport: String,
        @SerialName("environment") val environment: String,
        @SerialName("token") val token: String,
    )

    @Serializable
    private data class RefusedPayload(
        @SerialName("error") val error: String? = null,
        @SerialName("error_description") val errorDescription: String? = null,
    )

    @Serializable
    private data class BlockedPayload(
        @SerialName("error") val error: String? = null,
        @SerialName("provider") val provider: String? = null,
    )

    @Serializable
    private data class SessionUrlPayload(
        @SerialName("url") val url: String,
    )

    @Serializable
    private data class GraphQlRequest(
        @SerialName("query") val query: String,
    )

    @Serializable
    private data class GraphQlAnswer<T>(
        @SerialName("data") val data: T? = null,
    )

    @Serializable
    private data class MePayload(
        @SerialName("me") val me: AccountNode? = null,
    ) {
        @Serializable
        data class AccountNode(
            @SerialName("access") val access: AccessNode? = null,
        )

        @Serializable
        data class AccessNode(
            @SerialName("pro") val pro: Boolean,
            @SerialName("until") val until: String? = null,
            @SerialName("grant") val grant: GrantNode? = null,
        ) {
            fun toEntitlement(): Entitlement {
                val parsedGrant = grant?.toGrant()
                val untilMs = until?.let(::parseIsoTimestampMs)
                if (pro) {
                    val renewsAtMs = if (grant?.willRenew == true) {
                        grant.entitledUntil?.let(::parseIsoTimestampMs) ?: untilMs
                    } else {
                        null
                    }
                    return Entitlement.Active(
                        grant = parsedGrant ?: Grant.Granted,
                        renewsAtMs = renewsAtMs,
                    )
                }
                return if (parsedGrant != null && untilMs != null) {
                    Entitlement.Lapsed(grant = parsedGrant, endedAtMs = untilMs)
                } else {
                    Entitlement.None
                }
            }
        }

        @Serializable
        data class GrantNode(
            @SerialName("__typename") val typeName: String,
            @SerialName("provider") val provider: String? = null,
            @SerialName("willRenew") val willRenew: Boolean? = null,
            @SerialName("entitledUntil") val entitledUntil: String? = null,
        ) {
            fun toGrant(): Grant {
                if (typeName != "Purchased" || provider == null) return Grant.Granted
                val source = when (provider) {
                    "REVENUE_CAT" -> EntitlementSource.AppStore
                    else -> EntitlementSource.Web
                }
                return Grant.Purchased(source)
            }
        }
    }

    companion object {
        const val APP_STORE_SUBSCRIPTIONS_URL: String = "https://apps.apple.com/account/subscriptions"
        const val PLAY_STORE_SUBSCRIPTIONS_URL: String =
            "https://play.google.com/store/account/subscriptions"

        internal const val ACCESS_QUERY: String =
            "{ me { access { pro until grant { __typename " +
                "... on Purchased { provider willRenew entitledUntil } } } } }"

        private const val JSON_CONTENT_TYPE = "application/json; charset=utf-8"
        private const val FORM_CONTENT_TYPE = "application/x-www-form-urlencoded; charset=utf-8"
        private const val TOKEN_SLACK_MS = 60_000L

        private val secureRandom = SecureRandom()

        fun randomToken(): String {
            val bytes = ByteArray(32)
            secureRandom.nextBytes(bytes)
            return base64UrlEncode(bytes)
        }

        fun challenge(verifier: String): String {
            val digest = MessageDigest.getInstance("SHA-256")
                .digest(verifier.toByteArray(StandardCharsets.UTF_8))
            return base64UrlEncode(digest)
        }

        internal fun base64UrlEncode(bytes: ByteArray): String =
            Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)

        @Throws(CloudException::class)
        fun parseCallbackCode(callbackUrl: String, expectedState: String): String {
            val query = try {
                URI(callbackUrl).rawQuery.orEmpty()
            } catch (_: Exception) {
                ""
            }
            val items = parseQuery(query)
            val error = items["error"]
            if (error != null) {
                throw CloudException(CloudError.Refused(items["error_description"] ?: error))
            }
            if (items["state"] != expectedState) {
                throw CloudException(CloudError.Refused("that sign-in answered a different request"))
            }
            val code = items["code"]
            if (code.isNullOrEmpty()) {
                throw CloudException(CloudError.Refused("amux.sh sent no sign-in back"))
            }
            return code
        }

        internal fun refusal(statusCode: Int, body: ByteArray): CloudError {
            val raw = body.toString(StandardCharsets.UTF_8)
            val parsed = try {
                AmuxJson.json.decodeFromString<RefusedPayload>(raw)
            } catch (_: Throwable) {
                null
            }
            if (statusCode == 401) return CloudError.Unauthenticated
            if (statusCode == 403 && parsed?.error == "payment_required") {
                return CloudError.Refused("this account has no subscription")
            }
            val reason = parsed?.errorDescription
                ?: parsed?.error
                ?: raw.takeIf { it.isNotBlank() }
                ?: "amux.sh refused the request"
            return CloudError.Refused(reason)
        }

        internal fun encodeForm(fields: Map<String, String>): String =
            fields.keys.sorted().joinToString("&") { key ->
                "${percentEncode(key)}=${percentEncode(fields.getValue(key))}"
            }

        internal fun parseIsoTimestampMs(text: String): Long? = try {
            Instant.parse(text).toEpochMilli()
        } catch (_: Exception) {
            try {
                OffsetDateTime.parse(text).toInstant().toEpochMilli()
            } catch (_: Exception) {
                null
            }
        }

        private fun percentEncode(value: String): String =
            URLEncoder.encode(value, StandardCharsets.UTF_8.name())
                .replace("+", "%20")
                .replace("*", "%2A")
                .replace("%7E", "~")

        private fun parseQuery(rawQuery: String): Map<String, String> {
            if (rawQuery.isEmpty()) return emptyMap()
            val result = LinkedHashMap<String, String>()
            for (pair in rawQuery.split('&')) {
                if (pair.isEmpty()) continue
                val eq = pair.indexOf('=')
                val rawKey = if (eq >= 0) pair.substring(0, eq) else pair
                val rawVal = if (eq >= 0) pair.substring(eq + 1) else ""
                val key = URLDecoder.decode(rawKey, StandardCharsets.UTF_8.name())
                val value = URLDecoder.decode(rawVal, StandardCharsets.UTF_8.name())
                result.putIfAbsent(key, value)
            }
            return result
        }
    }
}
