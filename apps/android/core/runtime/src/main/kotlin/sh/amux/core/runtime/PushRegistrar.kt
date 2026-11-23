package sh.amux.core.runtime

/** One account as push notification registration sees it. */
data class PushAccount(
    val account: AccountId,
    val profile: String,
    /** `null` when entitlement is not yet known (neither register nor withdraw). */
    val wanted: Boolean?,
) {
    companion object {
        fun wanted(signedIn: Boolean, pro: Boolean?, entitlement: Entitlement): Boolean? {
            if (!signedIn) return false
            if (pro != null) return pro
            return when (entitlement) {
                is Entitlement.Active -> true
                is Entitlement.Lapsed -> false
                Entitlement.None -> null
            }
        }
    }
}

/**
 * Push notification registration seam.
 *
 * Kept behind a clean interface so FCM integration can plug in later without
 * leaking Firebase dependencies into v1.
 */
interface PushRegistrar {
    fun updateAccounts(accounts: List<PushAccount>)
    fun updatePermitted(permitted: Boolean)
    fun updateToken(token: ByteArray)
    suspend fun withdraw(account: AccountId)
    suspend fun settled()
}

/** The [PushRegistrar] when Firebase is unavailable (a build without `google-services.json`, or a test): it registers nothing. */
class NoOpPushRegistrar : PushRegistrar {
    override fun updateAccounts(accounts: List<PushAccount>) = Unit
    override fun updatePermitted(permitted: Boolean) = Unit
    override fun updateToken(token: ByteArray) = Unit
    override suspend fun withdraw(account: AccountId) = Unit
    override suspend fun settled() = Unit
}
