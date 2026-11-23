package sh.amux.shell

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.PairingStore
import sh.amux.core.runtime.StoreBundle
import sh.amux.core.runtime.id
import sh.amux.features.PairingAction
import sh.amux.features.PairingModalSheet
import sh.amux.values.FleetFilter

/** Raises the pairing sheet for [host]. */
internal fun presentPairing(stores: StoreBundle, router: Router, host: HostId) {
    stores.pairing.open(machine = stores.hosts.snapshot.known(host))
    router.presentPairing(PairingModal.Code(host))
}

/** Closes the pairing sheet, declining the window it was raised for and abandoning any pair underway. */
internal fun dismissPairing(stores: StoreBundle, router: Router) {
    val current = router.snapshot.pairing
    if (current is PairingModal.Code) {
        stores.hosts.declineWindow(current.host)
    }
    stores.cancelPairing()
    router.dismissPairing()
}

/**
 * Pairing over every screen: the sheet itself, raising it on its own when a
 * nearby computer opens a pairing window, starting a pairing link once the
 * account it needs is signed in, and closing it once the computer trusts
 * this phone.
 */
@Composable
internal fun PairingHost(
    routerState: RouterState,
    router: Router,
    stores: StoreBundle,
    signedIn: Boolean,
    signingIn: Boolean,
    reportOpen: Boolean,
    onAction: (ShellAction) -> Unit,
) {
    val pairingState by stores.pairing.state.collectAsState()
    val profileReady by stores.profileReady.collectAsState()
    val windowToOffer by stores.hosts.state.collectPart { it.windowToOffer?.id }
    val discovered by stores.hosts.state.collectPart { it.discovered }
    val fleetEmpty by stores.fleet.state.collectPart {
        it.loaded && it.rows.isEmpty() && it.filter == FleetFilter(words = "", only = null)
    }
    var startedLinkKey by remember(stores) { mutableStateOf<String?>(null) }

    // Open pairing on its own when a nearby computer advertises an open
    // pairing window and the user is looking at an empty Agents screen or
    // the Computers root tab.
    val windowCandidate: HostId? = when {
        reportOpen ||
            routerState.pairing != null ||
            routerState.accountSheetOpen ||
            routerState.topRoute != null -> null
        routerState.tab == Tab.Hosts || fleetEmpty -> windowToOffer
        else -> null
    }
    LaunchedEffect(windowCandidate) {
        val candidateId = windowCandidate ?: return@LaunchedEffect
        presentPairing(stores, router, candidateId)
    }

    // When a link pairing modal opens and the account is ready (or direct/discovered
    // addresses exist), begin authenticating the invitation once a live profile is attached.
    val activePairingModal = routerState.pairing
    val linkNeedsAccount = remember(activePairingModal, discovered) {
        activePairingModal is PairingModal.Link &&
            HostsMappers.invitationNeedsAccount(activePairingModal.invitation, discovered)
    }
    LaunchedEffect(activePairingModal, signedIn, stores, profileReady, linkNeedsAccount) {
        when (activePairingModal) {
            is PairingModal.Code -> {
                startedLinkKey = null
                val hostView = stores.hosts.snapshot.known(activePairingModal.host)
                if (stores.pairing.snapshot.machine?.hostId != activePairingModal.host.bytes) {
                    stores.pairing.open(machine = hostView)
                }
            }
            is PairingModal.Link -> {
                val invitation = activePairingModal.invitation
                if (startedLinkKey != invitation.link && (!linkNeedsAccount || signedIn)) {
                    if (stores.pairLink(link = invitation.link, relayOnly = linkNeedsAccount)) {
                        startedLinkKey = invitation.link
                    }
                }
            }
            null -> {
                startedLinkKey = null
            }
        }
    }

    // Close the pairing modal once the machine is trusted.
    LaunchedEffect(pairingState.phase) {
        if (pairingState.phase is PairingStore.Phase.Trusted && router.snapshot.pairing != null) {
            router.dismissPairing()
        }
    }

    if (reportOpen || !routerState.showsPairing || activePairingModal == null) return
    val dismiss = {
        startedLinkKey = null
        dismissPairing(stores, router)
    }
    val sheetState = HostsMappers.toPairingSheetState(
        pairing = pairingState,
        modal = activePairingModal,
        signedIn = signedIn,
        signingIn = signingIn,
        nowMs = stores.pairing.nowMs,
        discovered = discovered,
    )
    PairingModalSheet(
        state = sheetState,
        onAction = { action ->
            when (action) {
                is PairingAction.Digits -> stores.pairDigits(action.typed)
                is PairingAction.Confirm -> stores.confirmPairing(action.pending)
                is PairingAction.Abandon -> {
                    stores.abandonPairing(action.pending)
                    dismiss()
                }
                PairingAction.Cancel -> dismiss()
                PairingAction.SignIn -> onAction(ShellAction.SignIn)
                PairingAction.Subscribe -> {
                    router.push(Route.Paywall(routerState.tab))
                    onAction(ShellAction.Subscribe)
                }
            }
        },
    )
}
