package sh.amux.shell

import androidx.activity.compose.BackHandler
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.AccountRegistry
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.StoreBundle
import sh.amux.features.NetworkSight
import sh.amux.features.AgentsHome
import sh.amux.features.HomeAction
import sh.amux.features.HomeScope
import sh.amux.values.FleetFilter
import sh.amux.values.RelayLink

private val NO_FILTER = FleetFilter(words = "", only = null)

/**
 * The agents list: the fleet on the Agents tab, or its settled agents on a
 * page of their own when [scope] is [HomeScope.Closed].
 *
 * [backEnabled] is false while a sheet over the list takes the back gesture.
 */
@Composable
internal fun AgentsRoute(
    scope: HomeScope,
    stores: StoreBundle,
    accounts: AccountRegistry,
    router: Router,
    offerNotifications: Boolean,
    signingIn: Boolean,
    backEnabled: Boolean,
    networkSight: NetworkSight = NetworkSight.Looking,
    onAction: (ShellAction) -> Unit,
) {
    val coroutineScope = rememberCoroutineScope()
    val fleet by stores.fleet.state.collectAsState()
    val hosts by stores.hosts.state.collectAsState()
    val registry by accounts.state.collectAsState()
    val state = remember(fleet, hosts, registry, offerNotifications, signingIn, scope, networkSight) {
        AgentsMappers.toAgentsHomeState(
            fleet = fleet,
            hosts = hosts,
            registry = registry,
            offerNotifications = offerNotifications,
            signingIn = signingIn,
            scope = scope,
            networkSight = networkSight,
        )
    }
    var searchExpanded by rememberSaveable { mutableStateOf(false) }
    val filtered = fleet.filter != NO_FILTER

    // Back closes the search and clears what it filtered before it leaves the list.
    BackHandler(enabled = backEnabled && scope == HomeScope.Fleet && (searchExpanded || filtered)) {
        searchExpanded = false
        if (stores.fleet.snapshot.filter != NO_FILTER) {
            stores.filterFleet(NO_FILTER)
        }
    }

    val handle = remember(stores, accounts, router, coroutineScope, onAction) {
        homeActions(stores, accounts, router, coroutineScope, onAction)
    }
    AgentsHome(
        state = state,
        onAction = handle,
        searchExpanded = searchExpanded,
        onSearchExpandedChange = { searchExpanded = it },
        onPreviewDelete = { agent -> stores.deleteWarning(agent) },
        validateAgentName = ::validateAgentName,
    )
}

private fun homeActions(
    stores: StoreBundle,
    accounts: AccountRegistry,
    router: Router,
    coroutineScope: CoroutineScope,
    onAction: (ShellAction) -> Unit,
): (HomeAction) -> Unit = { action ->
    when (action) {
        is HomeAction.Open -> router.push(Route.Conversation(action.agent))
        is HomeAction.Filter -> stores.filterFleet(action.filter)
        is HomeAction.ToggleFamily -> stores.toggleFamily(action.head)
        is HomeAction.Pin -> {
            coroutineScope.launch { stores.pin(action.agent) }
        }
        is HomeAction.Unfile -> {
            coroutineScope.launch { stores.unfile(action.agent) }
        }
        is HomeAction.Settle -> {
            coroutineScope.launch { stores.close(action.agent) }
        }
        is HomeAction.CancelSettle -> {
            coroutineScope.launch { stores.cancelClose(action.agent) }
        }
        is HomeAction.Rename -> {
            coroutineScope.launch { stores.rename(action.name, action.agent) }
        }
        is HomeAction.Delete -> {
            coroutineScope.launch { stores.delete(action.agent, action.discardingChanges) }
        }
        HomeAction.ShowSettled -> router.push(Route.Settled)
        HomeAction.NewAgent -> router.push(Route.NewAgent)
        HomeAction.OpenAccountMenu -> router.openAccountSheet()
        is HomeAction.SwitchAccount -> {
            router.dismissAccountSheet()
            onAction(ShellAction.SelectAccount(AccountId(action.accountId)))
        }
        HomeAction.AddAccount -> {
            router.dismissAccountSheet()
            onAction(ShellAction.AddAccount)
        }
        HomeAction.SignIn -> {
            val selectedEntry = accounts.state.value.selectedAccount
            val expired = stores.fleet.snapshot.relay == RelayLink.SignInAgain
            if (selectedEntry != null && (!selectedEntry.signedIn || expired)) {
                onAction(ShellAction.SignInAgain(selectedEntry.id))
            } else {
                onAction(ShellAction.SignIn)
            }
        }
        is HomeAction.SignInAgain -> {
            router.dismissAccountSheet()
            onAction(ShellAction.SignInAgain(AccountId(action.accountId)))
        }
        HomeAction.Subscribe -> {
            router.push(Route.Paywall(router.snapshot.tab))
            onAction(ShellAction.Subscribe)
        }
        HomeAction.OpenSettings -> {
            router.dismissAccountSheet()
            router.push(Route.Settings)
        }
        is HomeAction.Pair -> {
            HostId.fromBytes(action.hostId)?.let { id -> presentPairing(stores, router, id) }
        }
        HomeAction.SendInstallLink -> onAction(ShellAction.SendInstallLink)
        HomeAction.OpenWifiSettings -> onAction(ShellAction.OpenWifiSettings)
        HomeAction.LookAgain -> onAction(ShellAction.LookOnNetworkAgain)
        HomeAction.TurnOnNotifications -> onAction(ShellAction.TurnOnNotifications)
        HomeAction.DismissNotifications -> onAction(ShellAction.DismissNotifications)
        HomeAction.OpenHosts -> router.selectTab(Tab.Hosts)
        HomeAction.Refresh -> stores.fleet.refreshOrder()
        HomeAction.Back -> router.pop()
    }
}
