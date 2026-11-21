package sh.amux.shell

import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.HostsState
import sh.amux.core.runtime.PairingInvitation
import sh.amux.core.runtime.PairingState
import sh.amux.core.runtime.PairingStore
import sh.amux.core.runtime.SignInOffersState
import sh.amux.core.runtime.online
import sh.amux.features.HostPageState
import sh.amux.features.HostsTabState
import sh.amux.features.PairingPhaseUi
import sh.amux.features.PairingSheetState
import sh.amux.features.label
import sh.amux.values.AccountBinding
import sh.amux.values.HostView
import sh.amux.words.name

/** Pure mappers from the hosts and pairing stores to the Computers tab, a computer's page and the pairing sheet. */
object HostsMappers {
    fun toHostsTabState(
        hosts: HostsState,
        wentOfflineAtMs: (HostId) -> Long?,
        nowMs: Long,
    ): HostsTabState {
        val wentOffline = buildMap {
            for (host in hosts.hosts) {
                val id = HostId.fromBytes(host.hostId) ?: continue
                wentOfflineAtMs(id)?.let { put(host.hostId, it) }
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
            nowMs = nowMs,
        )
    }

    fun toHostPageState(
        hostId: HostId,
        hosts: HostsState,
        offers: SignInOffersState,
        wentOfflineAtMs: Long?,
        nowMs: Long,
    ): HostPageState {
        val host = hosts.host(hostId)
        val fingerprint = hosts.roster?.peers?.firstOrNull { it.hostId == hostId.bytes }?.fingerprint
        val canOffer = hosts.account?.binding == AccountBinding.SignedIn &&
            host != null &&
            host.signedIn == false &&
            host.online
        return HostPageState(
            host = host,
            fingerprint = fingerprint,
            isConnecting = hosts.isConnecting(hostId),
            foundButUnreachable = hostId in hosts.foundButUnreachable,
            wentOfflineAtMs = wentOfflineAtMs,
            nowMs = nowMs,
            canOfferSignIn = canOffer,
            offeringSignIn = offers.answering == hostId.bytes,
            accountPro = hosts.account?.pro,
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
        val alreadyConfirmedOrConfirming = pairing.phase is PairingStore.Phase.Confirming ||
            pairing.phase is PairingStore.Phase.Trusted
        val phase: PairingPhaseUi = when {
            modal is PairingModal.Link &&
                !alreadyConfirmedOrConfirming &&
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
