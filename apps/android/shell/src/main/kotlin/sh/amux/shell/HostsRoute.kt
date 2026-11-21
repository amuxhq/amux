package sh.amux.shell

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.StoreBundle
import sh.amux.features.HostPage
import sh.amux.features.HostPageAction
import sh.amux.features.HostsAction
import sh.amux.features.HostsTab

/** The Computers tab: paired computers and the ones found nearby. */
@Composable
internal fun HostsRoute(
    stores: StoreBundle,
    router: Router,
    onAction: (ShellAction) -> Unit,
) {
    val hosts by stores.hosts.state.collectAsState()
    // When each computer went offline is recorded as the host list moves, so
    // the list is read again whenever the hosts change.
    val state = remember(hosts) {
        HostsMappers.toHostsTabState(
            hosts = hosts,
            wentOfflineAtMs = stores.hosts::wentOfflineAtMs,
            nowMs = stores.hosts.nowMs,
        )
    }
    val clock = rememberTickingClock(now = { stores.hosts.nowMs }, ticking = state.wentOfflineAtMs.isNotEmpty())
    HostsTab(
        clock = clock,
        state = state,
        onAction = { action ->
            when (action) {
                is HostsAction.Open -> {
                    HostId.fromBytes(action.hostId)?.let { id -> router.push(Route.Host(id)) }
                }
                is HostsAction.Pair -> {
                    HostId.fromBytes(action.hostId)?.let { id -> presentPairing(stores, router, id) }
                }
                HostsAction.Subscribe -> {
                    router.push(Route.Paywall(Tab.Hosts))
                    onAction(ShellAction.Subscribe)
                }
            }
        },
    )
}

/** One computer's page: how it is reached, its providers, and pairing it again or unpairing it. */
@Composable
internal fun HostRoute(
    host: HostId,
    stores: StoreBundle,
    router: Router,
    onAction: (ShellAction) -> Unit,
) {
    val context = LocalContext.current
    val hosts by stores.hosts.state.collectAsState()
    val offers by stores.offers.state.collectAsState()

    // The page shows the computer's fingerprint, which comes with the roster.
    LaunchedEffect(stores) {
        if (stores.hosts.snapshot.roster == null) {
            stores.refreshRoster()
        }
    }

    val state = remember(host, hosts, offers) {
        HostsMappers.toHostPageState(
            hostId = host,
            hosts = hosts,
            offers = offers,
            wentOfflineAtMs = stores.hosts.wentOfflineAtMs(host),
            nowMs = stores.hosts.nowMs,
        )
    }
    val clock = rememberTickingClock(now = { stores.hosts.nowMs }, ticking = state.wentOfflineAtMs != null)
    HostPage(
        clock = clock,
        state = state,
        onAction = { action ->
            when (action) {
                HostPageAction.Back -> router.pop()
                is HostPageAction.PairAgain -> {
                    HostId.fromBytes(action.hostId)?.let { id -> presentPairing(stores, router, id) }
                }
                is HostPageAction.Revoke -> {
                    HostId.fromBytes(action.hostId)?.let { id ->
                        stores.revoke(id)
                        router.pop()
                    }
                }
                HostPageAction.Subscribe -> {
                    router.push(Route.Paywall(router.snapshot.tab))
                    onAction(ShellAction.Subscribe)
                }
                is HostPageAction.OfferSignIn -> {
                    HostId.fromBytes(action.hostId)?.let { id ->
                        stores.hosts.snapshot.host(id)?.let { hostView ->
                            stores.offerAccount(hostView)
                        }
                    }
                }
                is HostPageAction.CopyAddress -> {
                    ShellPlatform.copyToClipboard(context, action.address)
                }
                is HostPageAction.CopyFingerprint -> {
                    ShellPlatform.copyToClipboard(context, action.fingerprint)
                }
            }
        },
    )
}
