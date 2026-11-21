package sh.amux.shell

import sh.amux.core.runtime.AccountRegistryState
import sh.amux.core.runtime.FleetState
import sh.amux.core.runtime.HostsState
import sh.amux.features.AccountUi
import sh.amux.features.AgentsHomeState
import sh.amux.features.HomeScope
import sh.amux.features.NetworkSight
import sh.amux.words.name

/** Pure mappers from the fleet, hosts and accounts to the Agents screen's state. */
object AgentsMappers {
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
        networkSight: NetworkSight = NetworkSight.Looking,
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
            networkSight = networkSight,
        )
    }

}
