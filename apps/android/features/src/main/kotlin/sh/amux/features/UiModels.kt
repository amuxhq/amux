package sh.amux.features

import androidx.compose.runtime.Immutable
import sh.amux.values.AgentKey
import sh.amux.values.FleetFilter
import sh.amux.values.FleetRow
import sh.amux.values.FleetSection
import sh.amux.values.HostView
import sh.amux.values.PendingPair
import sh.amux.values.Platform
import sh.amux.values.Reach
import sh.amux.values.RelayLink
import sh.amux.values.RouteKind
import sh.amux.values.SectionKind

/** Which agents [AgentsHome] displays: the live fleet or the settled archive page. */
enum class HomeScope {
    Fleet,
    Closed,
}

/** Reach group matching how Computers lists paired hosts. */
enum class HostReachGroup {
    OnThisNetwork,
    ThroughTheRelay,
    Away,
    Offline,
}

fun HostView.reachGroup(): HostReachGroup {
    if (candidate) {
        return if (addrs.isEmpty()) HostReachGroup.ThroughTheRelay else HostReachGroup.OnThisNetwork
    }
    return when (val current = reach) {
        is Reach.Online -> {
            if (current.value == RouteKind.Direct) {
                HostReachGroup.OnThisNetwork
            } else {
                HostReachGroup.ThroughTheRelay
            }
        }
        Reach.Away -> HostReachGroup.Away
        is Reach.Offline -> HostReachGroup.Offline
    }
}

val HostView.isOnline: Boolean
    get() = reach is Reach.Online

val HostView.hasOpenWindow: Boolean
    get() = candidate && pairingOpen && addrs.isNotEmpty()

val HostView.hasClosedWindow: Boolean
    get() = candidate && !pairingOpen && addrs.isNotEmpty()

val Platform.label: String?
    get() = when (this) {
        Platform.Macos -> "macOS"
        Platform.Linux -> "Linux"
        Platform.Windows -> "Windows"
        Platform.Ios -> "iOS"
        Platform.Android -> "Android"
        Platform.Unspecified -> null
    }

/** One account entry rendered in the top-bar avatar and account switcher sheet. */
@Immutable
data class AccountUi(
    val id: String,
    val name: String,
    val email: String,
    val line: String,
    val signedIn: Boolean,
    val selected: Boolean,
) {
    val initials: String
        get() = AccountInitials.from(name = name, email = email)
}

object AccountInitials {
    fun from(name: String?, email: String?): String {
        val source = name?.trim()?.takeIf { it.isNotEmpty() }
            ?: email?.substringBefore('@')?.trim().orEmpty()
        if (source.isEmpty()) return "?"
        val words = source.split(Regex("[\\s._-]+")).filter { it.isNotEmpty() }
        return when {
            words.size >= 2 -> "${words[0].first()}${words[1].first()}".uppercase()
            else -> source.take(2).uppercase()
        }
    }
}

/** Pure UI state rendered by [AgentsHome]. */
@Immutable
data class AgentsHomeState(
    val loaded: Boolean = true,
    val rows: List<FleetRow> = emptyList(),
    val sections: List<FleetSection> = emptyList(),
    val needYou: Int = 0,
    val working: Int = 0,
    val machines: List<HostView> = emptyList(),
    val discoveredHosts: List<HostView> = emptyList(),
    val filter: FleetFilter = FleetFilter(words = "", only = null),
    val orderedAtMs: Long = 0L,
    val accounts: List<AccountUi> = emptyList(),
    val selectedAccount: AccountUi? = null,
    val accountPro: Boolean? = null,
    val relay: RelayLink = RelayLink.Off,
    val relayTrouble: String? = null,
    val heldAwayHost: String? = null,
    val unreachableHost: String? = null,
    val offerNotifications: Boolean = false,
    val signingIn: Boolean = false,
    val scope: HomeScope = HomeScope.Fleet,
    val networkSight: NetworkSight = NetworkSight.Looking,
) {
    val signedIn: Boolean
        get() = accounts.any { it.signedIn }

    val filtering: Boolean
        get() = filter.words.trim().isNotEmpty() || filter.only != null

    val closedSection: FleetSection?
        get() = sections.firstOrNull { it.kind == SectionKind.Closed }

    val liveSections: List<FleetSection>
        get() = sections.filter { it.kind != SectionKind.Closed && it.rows.isNotEmpty() }

    val offersNewAgent: Boolean
        get() = scope == HomeScope.Fleet && machines.isNotEmpty()
}

/**
 * Whether this phone can look for computers on its network, for a screen
 * that would otherwise say it is looking for ever.
 */
enum class NetworkSight {
    Looking,

    /** No Wi-Fi or Ethernet: there is no network near the phone to look on. */
    NoLocalNetwork,

    /** The system refused to look. */
    BrowseFailed,
}

/** Actions emitted by [AgentsHome] without touching navigation or networking. */
sealed interface HomeAction {
    data class Open(val agent: AgentKey) : HomeAction
    data class Filter(val filter: FleetFilter) : HomeAction
    data class ToggleFamily(val head: AgentKey) : HomeAction
    data class Pin(val agent: AgentKey) : HomeAction
    data class Unfile(val agent: AgentKey) : HomeAction
    data class Settle(val agent: AgentKey) : HomeAction
    data class CancelSettle(val agent: AgentKey) : HomeAction
    data class Rename(val agent: AgentKey, val name: String) : HomeAction
    data class Delete(val agent: AgentKey, val discardingChanges: Boolean = false) : HomeAction
    data object ShowSettled : HomeAction
    data object NewAgent : HomeAction
    data object OpenAccountMenu : HomeAction
    data class SwitchAccount(val accountId: String) : HomeAction
    data object AddAccount : HomeAction
    data object SignIn : HomeAction
    data class SignInAgain(val accountId: String) : HomeAction
    data object Subscribe : HomeAction
    data object OpenSettings : HomeAction
    data class Pair(val hostId: List<Int>) : HomeAction
    data object SendInstallLink : HomeAction
    data object OpenWifiSettings : HomeAction
    data object LookAgain : HomeAction
    data object TurnOnNotifications : HomeAction
    data object DismissNotifications : HomeAction
    data object OpenHosts : HomeAction
    data object Refresh : HomeAction
    data object Back : HomeAction
}

/** Pure UI state rendered by [HostsTab]. */
@Immutable
data class HostsTabState(
    val hosts: List<HostView> = emptyList(),
    val discovered: List<HostView> = emptyList(),
    val accountPro: Boolean? = null,
    val awayHostName: String? = null,
    val connectingHostIds: Set<List<Int>> = emptySet(),
    val foundButUnreachableIds: Set<List<Int>> = emptySet(),
    val wentOfflineAtMs: Map<List<Int>, Long> = emptyMap(),
    val nowMs: Long = 0L,
) {
    fun isConnecting(host: HostView): Boolean = host.hostId in connectingHostIds
}

/** Actions emitted by [HostsTab]. */
sealed interface HostsAction {
    data class Open(val hostId: List<Int>) : HostsAction
    data class Pair(val hostId: List<Int>) : HostsAction
    data object Subscribe : HostsAction
}

/** Phase rendered inside [PairingSheetContent]. */
@Immutable
sealed interface PairingPhaseUi {
    data object Entering : PairingPhaseUi
    data class Checking(val viaLink: Boolean = false) : PairingPhaseUi
    data class Confirming(
        val pending: PendingPair,
        val platformName: String? = null,
    ) : PairingPhaseUi

    data class Trusted(val name: String) : PairingPhaseUi
    data class Refused(
        val message: String? = null,
        val viaLink: Boolean = false,
    ) : PairingPhaseUi

    data object NeedsSubscription : PairingPhaseUi
    data class NeedsAccount(val signingIn: Boolean = false) : PairingPhaseUi
}

/** Pure UI state rendered by [PairingSheetContent]. */
@Immutable
data class PairingSheetState(
    val digits: String = "",
    val phase: PairingPhaseUi = PairingPhaseUi.Entering,
    val machine: HostView? = null,
    val offerWindow: String = "5 minutes",
    val nowMs: Long = 0L,
) {
    val takingDigits: Boolean
        get() = phase is PairingPhaseUi.Entering ||
            (phase is PairingPhaseUi.Refused && !phase.viaLink)
}

/** Actions emitted by [PairingSheetContent]. */
sealed interface PairingAction {
    data class Digits(val typed: String) : PairingAction
    data class Confirm(val pending: PendingPair) : PairingAction
    data class Abandon(val pending: PendingPair) : PairingAction
    data object Cancel : PairingAction
    data object SignIn : PairingAction
    data object Subscribe : PairingAction
}
