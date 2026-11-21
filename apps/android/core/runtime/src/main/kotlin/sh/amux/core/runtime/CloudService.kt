package sh.amux.core.runtime

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * Everything this app asks of the account service (`amux.sh`).
 *
 * One interface so screens never see HTTP and tests can substitute a scripted
 * implementation cleanly.
 */
interface CloudService {
    @Throws(CloudException::class)
    suspend fun signIn(intent: SignInIntent, presenter: WebAuthPresenter): SignedInAccount

    /**
     * Hands over the refresh token obtained by a sign-in, once, so the runtime
     * can bind the account's profile and own token rotation from then on.
     */
    suspend fun handOver(id: AccountId): Handover?

    /** Drops any in-memory session held for [id] without affecting the server. */
    suspend fun forgetSession(id: AccountId)

    /** Supplies a bearer lender backed by the embedded runtime's profile. */
    suspend fun lend(lender: suspend (AccountId) -> String?)

    @Throws(CloudException::class)
    suspend fun entitlement(id: AccountId): Entitlement

    @Throws(CloudException::class)
    suspend fun recordPurchase(id: AccountId, signedTransaction: String)

    @Throws(CloudException::class)
    suspend fun requestDeletion(id: AccountId, confirmedEmail: String): DeletionOutcome

    @Throws(CloudException::class)
    suspend fun registerDevice(id: AccountId, host: HostId, device: PushDevice)

    @Throws(CloudException::class)
    suspend fun removeDevice(id: AccountId, host: HostId)
}

/** Platform push device registration payload. */
data class PushDevice(
    val token: String,
    val environment: Environment,
    val transport: String = "fcm",
) {
    enum class Environment(val wireValue: String) {
        Sandbox("sandbox"),
        Production("production"),
    }

    companion object {
        fun fromBytes(
            token: ByteArray,
            environment: Environment,
            transport: String = "fcm",
        ): PushDevice = PushDevice(
            token = hexEncode(token),
            environment = environment,
            transport = transport,
        )
    }
}

/** What a sign-in hands over to bind a profile on the embedded runtime. */
data class Handover(
    val cloudUrl: String,
    val clientId: String,
    val refreshToken: String,
)

/** Which account a browser sign-in flow is asking for. */
sealed interface SignInIntent {
    /** Adding a new account, or the phone's first account. */
    data object Adding : SignInIntent

    /** Signing back into an account already listed on this phone. */
    data class Returning(val account: SignedInAccount) : SignInIntent
}

/**
 * Browser hand-off seam (Chrome Custom Tabs in production, scripted callback
 * in tests).
 */
fun interface WebAuthPresenter {
    @Throws(CloudException::class)
    suspend fun present(url: String, callbackScheme: String): String
}

sealed interface CloudError {
    data object Cancelled : CloudError
    data object Unauthenticated : CloudError
    data class Network(val message: String) : CloudError
    data class Refused(val reason: String) : CloudError
    data object Timeout : CloudError
}

class CloudException(val error: CloudError) : Exception(
    when (error) {
        CloudError.Cancelled -> "cancelled"
        CloudError.Unauthenticated -> "unauthenticated"
        is CloudError.Network -> error.message
        is CloudError.Refused -> error.reason
        CloudError.Timeout -> "timeout"
    },
)

@Serializable
data class SignedInAccount(
    @SerialName("id") val id: AccountId,
    @SerialName("email") val email: String,
    @SerialName("displayName") val displayName: String? = null,
    @SerialName("pictureUrl") val pictureUrl: String? = null,
)

/**
 * What an account is allowed to do, as answered by `amux.sh`.
 *
 * `Active` comes strictly from the server's `pro` boolean, never inferred from
 * billing records or local clocks.
 */
@Serializable
sealed interface Entitlement {
    @Serializable
    @SerialName("none")
    data object None : Entitlement

    @Serializable
    @SerialName("active")
    data class Active(
        val grant: Grant,
        val renewsAtMs: Long? = null,
    ) : Entitlement

    @Serializable
    @SerialName("lapsed")
    data class Lapsed(
        val grant: Grant,
        val endedAtMs: Long,
    ) : Entitlement

    val summary: String
        get() = when (this) {
            is Active -> "Subscribed"
            None, is Lapsed -> "Not subscribed"
        }

    val isActive: Boolean
        get() = this is Active
}

/** Explains why an account has access (bought in a store or granted directly). */
@Serializable
sealed interface Grant {
    @Serializable
    @SerialName("purchased")
    data class Purchased(val source: EntitlementSource) : Grant

    @Serializable
    @SerialName("granted")
    data object Granted : Grant

    val purchase: EntitlementSource?
        get() = (this as? Purchased)?.source

    val named: String
        get() = when (this) {
            is Purchased -> source.named
            Granted -> "Included"
        }
}

@Serializable
enum class EntitlementSource {
    @SerialName("appStore")
    AppStore,

    @SerialName("playStore")
    PlayStore,

    @SerialName("web")
    Web,
    ;

    val named: String
        get() = when (this) {
            AppStore -> "App Store"
            PlayStore -> "Google Play"
            Web -> "amux.sh"
        }

    val place: String
        get() = when (this) {
            AppStore -> "the App Store"
            PlayStore -> "Google Play"
            Web -> "amux.sh"
        }
}

@Serializable
sealed interface DeletionOutcome {
    @Serializable
    @SerialName("deleted")
    data object Deleted : DeletionOutcome

    @Serializable
    @SerialName("blockedByRenewal")
    data class BlockedByRenewal(
        val source: EntitlementSource,
        val manageUrl: String,
    ) : DeletionOutcome
}
