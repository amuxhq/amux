package sh.amux.testsupport

import kotlinx.coroutines.delay
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonPrimitive
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.CloudError
import sh.amux.core.runtime.CloudException
import sh.amux.core.runtime.CloudService
import sh.amux.core.runtime.DeletionOutcome
import sh.amux.core.runtime.Entitlement
import sh.amux.core.runtime.EntitlementSource
import sh.amux.core.runtime.Grant
import sh.amux.core.runtime.Handover
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.PushDevice
import sh.amux.core.runtime.SignInIntent
import sh.amux.core.runtime.SignedInAccount
import sh.amux.core.runtime.WebAuthPresenter

/**
 * Deterministic scripted cloud answers for unit tests, storybook states, and
 * emulator journeys.
 */
data class ScriptedCloudState(
    val signIn: SignInOutcome = SignInOutcome.Succeeds(ADA),
    val entitlement: Entitlement = Entitlement.Active(
        grant = Grant.Purchased(EntitlementSource.Web),
        renewsAtMs = null,
    ),
    val token: String? = "scripted-connect-token",
    val relayHost: String = "relay.example",
    val relayPort: Int = 443,
    val deletion: DeletionOutcome = DeletionOutcome.Deleted,
    val purchase: PurchaseRecording = PurchaseRecording.Accepted,
    val latencyMs: Long = 0L,
) {
    sealed interface SignInOutcome {
        data class Succeeds(val account: SignedInAccount) : SignInOutcome
        data object Cancelled : SignInOutcome
        data class Refused(val reason: String) : SignInOutcome
        data object Offline : SignInOutcome
    }

    sealed interface PurchaseRecording {
        data object Accepted : PurchaseRecording
        data class Refused(val reason: String) : PurchaseRecording
        data object Offline : PurchaseRecording
    }

    companion object {
        val ADA: SignedInAccount = SignedInAccount(
            id = AccountId("ada"),
            email = "ada@example.com",
            displayName = "Ada",
        )

        val firstRun: ScriptedCloudState
            get() = ScriptedCloudState(
                signIn = SignInOutcome.Cancelled,
                entitlement = Entitlement.None,
                token = null,
            )

        val unsubscribed: ScriptedCloudState
            get() = ScriptedCloudState(entitlement = Entitlement.None)

        val granted: ScriptedCloudState
            get() = ScriptedCloudState(
                entitlement = Entitlement.Active(grant = Grant.Granted, renewsAtMs = null),
            )
    }
}

/** One recorded call made against [ScriptedCloudService]. */
sealed interface CloudCall {
    data class SignIn(val intent: SignInIntent) : CloudCall
    data class HandOver(val account: AccountId) : CloudCall
    data class ForgetSession(val account: AccountId) : CloudCall
    data class EntitlementRead(val account: AccountId) : CloudCall
    data class RecordPurchase(val account: AccountId) : CloudCall
    data class RequestDeletion(val account: AccountId, val confirmedEmail: String) : CloudCall
    data class RegisterDevice(
        val account: AccountId,
        val host: HostId,
        val device: PushDevice,
    ) : CloudCall

    data class RemoveDevice(val account: AccountId, val host: HostId) : CloudCall
}

/** Thread-safe scripted [CloudService] double that records every call. */
class ScriptedCloudService(
    initialState: ScriptedCloudState = ScriptedCloudState(),
) : CloudService {
    private val lock = Any()
    private var state: ScriptedCloudState = initialState
    private val recorded = mutableListOf<CloudCall>()

    val calls: List<CloudCall>
        get() = synchronized(lock) { recorded.toList() }

    var scripted: ScriptedCloudState
        get() = synchronized(lock) { state }
        set(value) {
            synchronized(lock) { state = value }
        }

    fun reset() {
        synchronized(lock) { recorded.clear() }
    }

    private fun record(call: CloudCall): ScriptedCloudState = synchronized(lock) {
        recorded.add(call)
        state
    }

    private suspend fun waitLatency(snapshot: ScriptedCloudState) {
        if (snapshot.latencyMs > 0L) {
            delay(snapshot.latencyMs)
        }
    }

    override suspend fun signIn(
        intent: SignInIntent,
        presenter: WebAuthPresenter,
    ): SignedInAccount {
        val snapshot = record(CloudCall.SignIn(intent))
        waitLatency(snapshot)
        return when (val outcome = snapshot.signIn) {
            is ScriptedCloudState.SignInOutcome.Succeeds -> {
                runCatching {
                    presenter.present("https://amux.sh/sign-in", "amux")
                }
                outcome.account
            }
            ScriptedCloudState.SignInOutcome.Cancelled ->
                throw CloudException(CloudError.Cancelled)
            is ScriptedCloudState.SignInOutcome.Refused ->
                throw CloudException(CloudError.Refused(outcome.reason))
            ScriptedCloudState.SignInOutcome.Offline ->
                throw CloudException(CloudError.Network("offline"))
        }
    }

    override suspend fun handOver(id: AccountId): Handover? {
        val snapshot = record(CloudCall.HandOver(id))
        val token = snapshot.token ?: return null
        return Handover(
            cloudUrl = "http://${snapshot.relayHost}:${snapshot.relayPort}",
            clientId = "cli",
            refreshToken = token,
        )
    }

    override suspend fun forgetSession(id: AccountId) {
        record(CloudCall.ForgetSession(id))
    }

    override suspend fun lend(lender: suspend (AccountId) -> String?) = Unit

    override suspend fun entitlement(id: AccountId): Entitlement {
        val snapshot = record(CloudCall.EntitlementRead(id))
        waitLatency(snapshot)
        return snapshot.entitlement
    }

    override suspend fun recordPurchase(id: AccountId, signedTransaction: String) {
        val snapshot = record(CloudCall.RecordPurchase(id))
        waitLatency(snapshot)
        when (val outcome = snapshot.purchase) {
            ScriptedCloudState.PurchaseRecording.Accepted -> {
                synchronized(lock) {
                    state = state.copy(
                        entitlement = Entitlement.Active(
                            grant = Grant.Purchased(EntitlementSource.PlayStore),
                            renewsAtMs = null,
                        ),
                    )
                }
            }
            is ScriptedCloudState.PurchaseRecording.Refused ->
                throw CloudException(CloudError.Refused(outcome.reason))
            ScriptedCloudState.PurchaseRecording.Offline ->
                throw CloudException(CloudError.Network("offline"))
        }
    }

    override suspend fun requestDeletion(
        id: AccountId,
        confirmedEmail: String,
    ): DeletionOutcome {
        val snapshot = record(CloudCall.RequestDeletion(id, confirmedEmail))
        waitLatency(snapshot)
        return snapshot.deletion
    }

    override suspend fun registerDevice(id: AccountId, host: HostId, device: PushDevice) {
        val snapshot = record(CloudCall.RegisterDevice(id, host, device))
        waitLatency(snapshot)
    }

    override suspend fun removeDevice(id: AccountId, host: HostId) {
        val snapshot = record(CloudCall.RemoveDevice(id, host))
        waitLatency(snapshot)
    }

    companion object {
        /** Fixed reference epoch matching iOS scripted lapse timestamps. */
        const val FIXED_NOW_MS: Long = 1_764_580_800_000L
    }
}

/** Deterministic [WebAuthPresenter] double that returns a callback URL or cancels. */
data class ScriptedWebAuth(
    val callbackUrl: String = "amux://callback?code=scripted",
    val outcome: Outcome = Outcome.Returns,
) : WebAuthPresenter {
    enum class Outcome {
        Returns,
        Cancelled,
    }

    override suspend fun present(url: String, callbackScheme: String): String = when (outcome) {
        Outcome.Returns -> callbackUrl
        Outcome.Cancelled -> throw CloudException(CloudError.Cancelled)
    }
}

/** Wire-friendly script descriptor used by debug Door `cloud` commands. */
data class CloudScript(
    val signIn: String = "succeeds",
    val account: String = "ada",
    val email: String = "ada@example.com",
    val displayName: String? = null,
    val reason: String = "amux.sh could not sign this account in",
    val entitlement: String = "active",
    val source: String = "web",
    val token: String? = "scripted-connect-token",
    val relayHost: String = "relay.example",
    val relayPort: Int = 443,
    val deletion: String = "deleted",
    val recordPurchase: String = "accepted",
    val purchaseReason: String = "amux.sh could not accept that purchase",
    val manageUrl: String = "https://play.google.com/store/account/subscriptions",
    val latencyMillis: Int = 0,
) {
    fun toState(): ScriptedCloudState {
        val who = SignedInAccount(
            id = AccountId(account),
            email = email,
            displayName = displayName,
        )
        val signInOutcome = when (signIn) {
            "cancelled" -> ScriptedCloudState.SignInOutcome.Cancelled
            "refused" -> ScriptedCloudState.SignInOutcome.Refused(reason)
            "offline" -> ScriptedCloudState.SignInOutcome.Offline
            else -> ScriptedCloudState.SignInOutcome.Succeeds(who)
        }
        val grant = when (source) {
            "granted" -> Grant.Granted
            "appStore" -> Grant.Purchased(EntitlementSource.AppStore)
            "playStore" -> Grant.Purchased(EntitlementSource.PlayStore)
            else -> Grant.Purchased(EntitlementSource.Web)
        }
        val entitled = when (entitlement) {
            "none" -> Entitlement.None
            "lapsed" -> Entitlement.Lapsed(
                grant = grant,
                endedAtMs = ScriptedCloudService.FIXED_NOW_MS - 86_400_000L,
            )
            else -> Entitlement.Active(grant = grant, renewsAtMs = null)
        }
        val recording = when (recordPurchase) {
            "refused" -> ScriptedCloudState.PurchaseRecording.Refused(purchaseReason)
            "network" -> ScriptedCloudState.PurchaseRecording.Offline
            else -> ScriptedCloudState.PurchaseRecording.Accepted
        }
        val deletionOutcome = if (deletion == "blockedByRenewal") {
            DeletionOutcome.BlockedByRenewal(
                source = grant.purchase ?: EntitlementSource.Web,
                manageUrl = manageUrl,
            )
        } else {
            DeletionOutcome.Deleted
        }
        return ScriptedCloudState(
            signIn = signInOutcome,
            entitlement = entitled,
            token = token,
            relayHost = relayHost,
            relayPort = relayPort,
            deletion = deletionOutcome,
            purchase = recording,
            latencyMs = latencyMillis.toLong(),
        )
    }

    companion object {
        private val json = Json { ignoreUnknownKeys = true }

        fun fromJson(raw: String): CloudScript {
            val obj = json.parseToJsonElement(raw) as? JsonObject ?: return CloudScript()
            return fromJsonObject(obj)
        }

        fun fromJsonObject(obj: JsonObject): CloudScript {
            val defaults = CloudScript()
            fun str(key: String, fallback: String): String =
                obj[key]?.takeUnless { it is JsonNull }?.jsonPrimitive?.contentOrNull ?: fallback

            val token = if ("token" in obj) {
                obj["token"]?.takeUnless { it is JsonNull }?.jsonPrimitive?.contentOrNull
            } else {
                defaults.token
            }
            return CloudScript(
                signIn = str("signIn", defaults.signIn),
                account = str("account", defaults.account),
                email = str("email", defaults.email),
                displayName = obj["displayName"]?.takeUnless { it is JsonNull }?.jsonPrimitive?.contentOrNull,
                reason = str("reason", defaults.reason),
                entitlement = str("entitlement", defaults.entitlement),
                source = str("source", defaults.source),
                token = token,
                relayHost = str("relayHost", defaults.relayHost),
                relayPort = obj["relayPort"]?.jsonPrimitive?.intOrNull ?: defaults.relayPort,
                deletion = str("deletion", defaults.deletion),
                recordPurchase = str("recordPurchase", defaults.recordPurchase),
                purchaseReason = str("purchaseReason", defaults.purchaseReason),
                manageUrl = str("manageURL", str("manageUrl", defaults.manageUrl)),
                latencyMillis = obj["latencyMillis"]?.jsonPrimitive?.intOrNull ?: defaults.latencyMillis,
            )
        }
    }
}
