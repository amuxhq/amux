package sh.amux.shell

import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.NewAgentBridge
import sh.amux.core.runtime.NewAgentSource
import sh.amux.core.runtime.NewAgentStore
import sh.amux.core.runtime.StoreBundle
import sh.amux.core.runtime.asNewAgentSource
import sh.amux.features.NewAgentAction
import sh.amux.features.NewAgentScreen

/** Starting a new agent: choosing a computer, a provider and a folder. */
@Composable
internal fun NewAgentRoute(
    stores: StoreBundle,
    router: Router,
    sourceProvider: ((StoreBundle) -> NewAgentSource?)?,
    bridge: NewAgentBridge,
) {
    val coroutineScope = rememberCoroutineScope()
    val profileVersion by stores.profileVersion.collectAsState()
    val fleet by stores.fleet.state.collectAsState()
    val hosts by stores.hosts.state.collectAsState()
    val source = remember(stores, profileVersion, sourceProvider) {
        sourceProvider?.invoke(stores) ?: stores.profile?.asNewAgentSource()
    }
    val store = remember(stores, source, bridge) {
        NewAgentStore(
            source = source,
            scope = coroutineScope,
            bridge = bridge,
        )
    }

    LaunchedEffect(store) {
        store.open(
            hosts = hosts.hosts,
            rows = fleet.rows,
        )
    }
    LaunchedEffect(store, hosts.hosts) {
        store.syncHosts(hosts.hosts)
    }
    LaunchedEffect(store, fleet.rows) {
        store.remember(fleet.rows)
    }
    DisposableEffect(store) {
        onDispose { store.close() }
    }

    val state by store.state.collectAsState()

    LaunchedEffect(state.created) {
        val createdKey = state.created ?: return@LaunchedEffect
        router.pop()
        router.push(Route.Conversation(createdKey))
    }

    val screenState = remember(state, hosts.connecting) {
        NewAgentMappers.toNewAgentScreenState(
            state = state,
            connecting = hosts.connecting,
        )
    }

    NewAgentScreen(
        state = screenState,
        onAction = { action ->
            when (action) {
                NewAgentAction.Back -> router.pop()
                is NewAgentAction.SelectHost -> {
                    HostId.fromBytes(action.hostId)?.let(store::selectHost)
                }
                is NewAgentAction.SelectProvider -> store.selectProvider(action.provider)
                is NewAgentAction.SetBrowsing -> store.setBrowsing(action.open)
                is NewAgentAction.ChooseDirectory -> store.chooseDirectory(action.path)
                is NewAgentAction.UpdateQuery -> store.updateQuery(action.query)
                is NewAgentAction.UpdateTypedPath -> store.updateTypedPath(action.path)
                is NewAgentAction.PinFolder -> store.pinFolder(action.directory, action.pinned)
                is NewAgentAction.PickSetting -> store.pick(action.pick)
                is NewAgentAction.SetNewWorktree -> store.setNewWorktree(action.enabled)
                is NewAgentAction.UpdateName -> store.updateName(action.name)
                NewAgentAction.Start -> store.start()
            }
        },
    )
}
