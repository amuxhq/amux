package sh.amux.shell

import sh.amux.core.runtime.AccountRegistryState
import sh.amux.core.runtime.FleetState
import sh.amux.core.runtime.HostsState
import sh.amux.core.runtime.HostsStore
import sh.amux.core.runtime.PairingInvitation
import sh.amux.core.runtime.PairingState
import sh.amux.core.runtime.PairingStore
import sh.amux.features.AccountUi
import sh.amux.features.AgentsHomeState
import sh.amux.features.HomeScope
import sh.amux.features.HostsTabState
import sh.amux.features.PairingPhaseUi
import sh.amux.features.PairingSheetState
import sh.amux.features.label
import sh.amux.values.HostView

/**
 * Pure mappers from `core:runtime` store snapshots to `features` immutable UI states.
 */
object StateMappers {
    fun toAccountsUi(registry: AccountRegistryState): List<AccountUi> =
        registry.accounts.map { entry ->
            AccountUi(
                id = entry.id.value,
                name = entry.name,
                email = entry.account.email,
                line = entry.line,
                signedIn = entry.signedIn,
                selected = entry.id == registry.selected,
            )
        }

    fun toAgentsHomeState(
        fleet: FleetState,
        hosts: HostsState,
        registry: AccountRegistryState,
        offerNotifications: Boolean = false,
        signingIn: Boolean = false,
        scope: HomeScope = HomeScope.Fleet,
    ): AgentsHomeState {
        val accountUis = toAccountsUi(registry)
        val selectedUi = accountUis.firstOrNull { it.selected }
        return AgentsHomeState(
            loaded = fleet.loaded,
            rows = fleet.rows,
            sections = fleet.sections,
            needYou = fleet.needYou,
            working = fleet.working,
            machines = fleet.machines,
            discoveredHosts = hosts.discovered,
            filter = fleet.filter,
            orderedAtMs = fleet.orderedAtMs,
            accounts = accountUis,
            selectedAccount = selectedUi,
            accountPro = hosts.account?.pro,
            relay = fleet.relay,
            relayTrouble = fleet.relayTrouble,
            heldAwayHost = fleet.heldAway,
            unreachableHost = fleet.unreachableHost,
            offerNotifications = offerNotifications,
            signingIn = signingIn,
            scope = scope,
        )
    }

    fun toHostsTabState(
        hosts: HostsState,
        hostsStore: HostsStore,
    ): HostsTabState {
        val wentOffline = buildMap {
            for (host in hosts.hosts) {
                val id = sh.amux.core.runtime.HostId.fromBytes(host.hostId) ?: continue
                hostsStore.wentOfflineAtMs(id)?.let { put(host.hostId, it) }
            }
        }
        return HostsTabState(
            hosts = hosts.hosts,
            discovered = hosts.discovered,
            accountPro = hosts.account?.pro,
            awayHostName = hosts.awayHost?.name,
            connectingHostIds = hosts.connecting.map { it.bytes }.toSet(),
            foundButUnreachableIds = hosts.foundButUnreachable.map { it.bytes }.toSet(),
            wentOfflineAtMs = wentOffline,
            nowMs = hostsStore.nowMs,
        )
    }

    /**
     * An addressless QR invitation (e.g. from an IPv6-only LAN) can still dial
     * directly without an account once local discovery has found the host.
     */
    fun invitationNeedsAccount(
        invitation: PairingInvitation,
        discovered: List<HostView>,
    ): Boolean {
        if (!invitation.needsAnAccount) return false
        return discovered.none { candidate ->
            candidate.hostId == invitation.host.bytes && candidate.addrs.isNotEmpty()
        }
    }

    fun toPairingSheetState(
        pairing: PairingState,
        modal: PairingModal,
        signedIn: Boolean,
        signingIn: Boolean,
        nowMs: Long,
        discovered: List<HostView> = emptyList(),
    ): PairingSheetState {
        val viaLink = modal is PairingModal.Link
        val phase: PairingPhaseUi = when {
            modal is PairingModal.Link &&
                invitationNeedsAccount(modal.invitation, discovered) &&
                !signedIn -> {
                PairingPhaseUi.NeedsAccount(signingIn = signingIn)
            }
            else -> when (val raw = pairing.phase) {
                PairingStore.Phase.Entering -> {
                    if (viaLink) PairingPhaseUi.Checking(viaLink = true) else PairingPhaseUi.Entering
                }
                PairingStore.Phase.Checking -> PairingPhaseUi.Checking(viaLink = viaLink)
                is PairingStore.Phase.Confirming -> PairingPhaseUi.Confirming(
                    pending = raw.pending,
                    platformName = pairing.machine?.platform?.label,
                )
                is PairingStore.Phase.Trusted -> PairingPhaseUi.Trusted(raw.name)
                PairingStore.Phase.Refused -> PairingPhaseUi.Refused(
                    message = pairing.told,
                    viaLink = viaLink,
                )
                PairingStore.Phase.NeedsSubscription -> PairingPhaseUi.NeedsSubscription
            }
        }
        return PairingSheetState(
            digits = pairing.digits,
            phase = phase,
            machine = pairing.machine,
            offerWindow = PairingStore.OFFER_WINDOW,
            nowMs = nowMs,
        )
    }
}
