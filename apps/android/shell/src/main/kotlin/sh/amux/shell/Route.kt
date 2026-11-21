package sh.amux.shell

import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.PairingInvitation
import sh.amux.values.AgentKey

/**
 * The two top-level bottom navigation destinations.
 */
enum class Tab(val wireName: String) {
    Agents("agents"),
    Hosts("hosts"),
}

/**
 * Full-screen routes pushed onto a tab's back stack.
 */
sealed interface Route {
    data class Conversation(val agent: AgentKey) : Route

    /** An agent's review, pushed over its chat. */
    data class Review(val agent: AgentKey) : Route
    data class Host(val host: HostId) : Route
    data object Settled : Route
    data object Settings : Route
    data object NewAgent : Route
    data class Paywall(val from: Tab) : Route

    /** Preferred owning tab when pushed from outside a tab context. */
    val preferredTab: Tab?
        get() = when (this) {
            is Conversation, is Review, Settled -> Tab.Agents
            is Host -> Tab.Hosts
            is Paywall -> from
            Settings, NewAgent -> null
        }

    val routeName: String
        get() = when (this) {
            is Conversation -> "conversation"
            is Review -> "review"
            is Host -> "host"
            Settled -> "settled"
            Settings -> "settings"
            NewAgent -> "new-agent"
            is Paywall -> "paywall"
        }
}

/**
 * Modal pairing sheet raised over whichever tab asked for it.
 */
sealed interface PairingModal {
    data class Code(val host: HostId) : PairingModal
    data class Link(val invitation: PairingInvitation) : PairingModal

    val modalName: String
        get() = when (this) {
            is Code -> "pin"
            is Link -> "pair-confirm"
        }
}
