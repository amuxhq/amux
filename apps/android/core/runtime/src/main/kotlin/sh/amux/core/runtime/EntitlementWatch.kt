package sh.amux.core.runtime

/**
 * Keeps the active account's entitlement synchronized with `amux.sh` on
 * foreground and Settings entry (throttled to once per [SPACING_MS]), and
 * immediately after a purchase (`force = true`).
 *
 * Notifies the profile's relay link via [relink] whenever the answer changed,
 * was forced, or disagrees with the relay link's last reported `pro` tier.
 */
class EntitlementWatch(
    private val registry: AccountRegistry,
    private val read: suspend (AccountId) -> Entitlement?,
    private val relink: suspend (AccountId) -> Boolean,
    private val nowMs: () -> Long = System::currentTimeMillis,
) {
    private var lastReadAccount: AccountId? = null
    private var lastReadAtMs: Long = Long.MIN_VALUE

    suspend fun look(force: Boolean = false): Boolean {
        val id = registry.selected ?: return false
        val at = nowMs()
        if (!force && lastReadAccount == id && (at - lastReadAtMs) < SPACING_MS) {
            return true
        }
        val before = registry.selectedAccount?.entitlement
        val answer = read(id) ?: return false
        val kept = registry.accept(answer, forAccount = id) ?: return false

        lastReadAccount = id
        lastReadAtMs = at
        registry.entitlement(kept, forAccount = id)

        if (force || kept != before || relayDisagrees(kept)) {
            relink(id)
        }
        return true
    }

    private fun relayDisagrees(entitlement: Entitlement): Boolean {
        val relayPro = registry.stores?.hosts?.snapshot?.account?.pro ?: return false
        return entitlement.isActive != relayPro
    }

    companion object {
        const val SPACING_MS: Long = 30_000L
    }
}
