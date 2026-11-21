package sh.amux.core.runtime

import sh.amux.values.AgentKey
import sh.amux.values.Filing
import sh.amux.values.FleetRow
import sh.amux.values.HostView
import sh.amux.values.PairedPeer
import sh.amux.values.Platform
import sh.amux.values.Reach
import sh.amux.values.RouteKind

/**
 * Where the hosts screen lists a host: by the route it is reached over,
 * else whether the relay sees it.
 */
enum class HostGroup(val wireName: String) {
    OnThisNetwork("on-this-network"),
    ThroughTheRelay("through-the-relay"),
    Away("away"),
    Offline("offline"),
    ;

    companion object {
        val all: List<HostGroup> = listOf(OnThisNetwork, ThroughTheRelay, Away, Offline)
    }
}

val Reach.online: Boolean
    get() = this is Reach.Online

val Platform.displayName: String?
    get() = when (this) {
        Platform.Macos -> "macOS"
        Platform.Linux -> "Linux"
        Platform.Windows -> "Windows"
        Platform.Ios -> "iOS"
        Platform.Unspecified -> null
    }

val HostView.id: HostId?
    get() = HostId.fromBytes(hostId)

val HostView.online: Boolean
    get() = reach.online

/**
 * Where the hosts screen lists this host, from how the runtime says this phone
 * reaches it.
 */
val HostView.group: HostGroup
    get() {
        if (candidate) {
            return if (addrs.isEmpty()) HostGroup.ThroughTheRelay else HostGroup.OnThisNetwork
        }
        return when (val current = reach) {
            is Reach.Online -> {
                if (current.value == RouteKind.Direct) {
                    HostGroup.OnThisNetwork
                } else {
                    HostGroup.ThroughTheRelay
                }
            }
            Reach.Away -> HostGroup.Away
            is Reach.Offline -> HostGroup.Offline
        }
    }

/** Found on this network with an open pairing window: a PIN can work now. */
val HostView.windowOpen: Boolean
    get() = candidate && pairingOpen && addrs.isNotEmpty()

/** Found on this network without an open pairing window. */
val HostView.windowClosed: Boolean
    get() = candidate && !pairingOpen && addrs.isNotEmpty()

val PairedPeer.host: HostId?
    get() = HostId.fromBytes(hostId)

val FleetRow.id: AgentKey
    get() = card.agent

val FleetRow.hostId: HostId?
    get() = card.agent.hostId

val FleetRow.closed: Boolean
    get() = card.filing == Filing.Closed

val FleetRow.lastActivityMs: Long
    get() = card.phaseSinceMs
