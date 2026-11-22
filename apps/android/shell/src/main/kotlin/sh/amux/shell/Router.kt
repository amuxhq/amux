package sh.amux.shell

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import sh.amux.core.runtime.PairingInvitation

/** Optional observer notified when pushed routes leave their back stack. */
interface RouteListener {
    fun onRouteEntered(route: Route) = Unit
    fun onRouteLeft(route: Route) = Unit
}

/**
 * Immutable navigation state for the shell: active tab, per-tab back stacks,
 * active pairing modal, and account switcher sheet visibility.
 */
data class RouterState(
    val tab: Tab = Tab.Agents,
    val agentsStack: List<Route> = emptyList(),
    val hostsStack: List<Route> = emptyList(),
    val pairing: PairingModal? = null,
    val accountSheetOpen: Boolean = false,
) {
    fun stack(tab: Tab): List<Route> = when (tab) {
        Tab.Agents -> agentsStack
        Tab.Hosts -> hostsStack
    }

    val currentStack: List<Route>
        get() = stack(tab)

    val topRoute: Route?
        get() = currentStack.lastOrNull()

    val showsBottomBar: Boolean
        get() = topRoute == null

    /**
     * Pairing steps aside while a Paywall route is open, then returns once
     * the paywall is dismissed.
     */
    val showsPairing: Boolean
        get() = pairing != null && topRoute !is Route.Paywall

    val canPop: Boolean
        get() = currentStack.isNotEmpty()
}

/**
 * Single owner of app navigation state. Screens never mutate routes directly;
 * they emit semantic actions and the shell drives [Router].
 */
class Router(
    initial: RouterState = RouterState(),
    private var listener: RouteListener? = null,
) {
    private val _state = MutableStateFlow(initial)
    val state: StateFlow<RouterState> = _state.asStateFlow()

    val snapshot: RouterState
        get() = _state.value

    fun setListener(listener: RouteListener?) {
        this.listener = listener
    }

    /**
     * Switches to [tab]. Re-selecting the active tab pops it back to its root.
     */
    fun selectTab(tab: Tab) {
        val current = _state.value
        if (current.tab == tab) {
            popToRoot()
        } else {
            _state.update { it.copy(tab = tab, accountSheetOpen = false) }
        }
    }

    /** Pushes [route] onto its target tab stack (or the active tab if tab-neutral). */
    fun push(route: Route) {
        val targetTab = route.preferredTab ?: _state.value.tab
        _state.update { current ->
            when (targetTab) {
                Tab.Agents -> current.copy(
                    tab = Tab.Agents,
                    agentsStack = current.agentsStack + route,
                    accountSheetOpen = false,
                )
                Tab.Hosts -> current.copy(
                    tab = Tab.Hosts,
                    hostsStack = current.hostsStack + route,
                    accountSheetOpen = false,
                )
            }
        }
        listener?.onRouteEntered(route)
    }

    /** Pops the top route of the current tab. Returns true if a route was popped. */
    fun pop(): Boolean {
        val current = _state.value
        val stack = current.currentStack
        if (stack.isEmpty()) return false
        val removed = stack.last()
        val updated = stack.dropLast(1)
        _state.update { state ->
            when (state.tab) {
                Tab.Agents -> state.copy(agentsStack = updated)
                Tab.Hosts -> state.copy(hostsStack = updated)
            }
        }
        listener?.onRouteLeft(removed)
        return true
    }

    /** Pops [route] only if it is currently on top of the active stack. */
    fun close(route: Route): Boolean {
        if (_state.value.topRoute != route) return false
        return pop()
    }

    /** Clears the active tab's back stack. */
    fun popToRoot() {
        val current = _state.value
        val removed = current.currentStack
        if (removed.isEmpty()) return
        _state.update { state ->
            when (state.tab) {
                Tab.Agents -> state.copy(agentsStack = emptyList())
                Tab.Hosts -> state.copy(hostsStack = emptyList())
            }
        }
        for (route in removed.asReversed()) {
            listener?.onRouteLeft(route)
        }
    }

    /** Returns directly to the root Agents tab. */
    fun openFleet() {
        val current = _state.value
        val removed = current.agentsStack
        _state.update { it.copy(tab = Tab.Agents, agentsStack = emptyList(), accountSheetOpen = false) }
        for (route in removed.asReversed()) {
            listener?.onRouteLeft(route)
        }
    }

    fun presentPairing(modal: PairingModal) {
        _state.update { it.copy(pairing = modal, accountSheetOpen = false) }
    }

    fun dismissPairing() {
        _state.update { it.copy(pairing = null) }
    }

    fun openAccountSheet() {
        _state.update { it.copy(accountSheetOpen = true) }
    }

    fun dismissAccountSheet() {
        _state.update { it.copy(accountSheetOpen = false) }
    }

    /**
     * Clears per-account stacks and code-based pairing sheets when the active
     * account changes. Link-based invitations survive account switches because
     * signing in is how a relay-only invitation is taken up. Settings is the
     * phone's, not an account's, and is where a person signs in or switches,
     * so it stays open over the new account's fleet to show the outcome.
     */
    fun leaveAccount() {
        val current = _state.value
        fun kept(stack: List<Route>) = if (Route.Settings in stack) listOf(Route.Settings) else emptyList()
        val removed = (current.agentsStack + current.hostsStack).filter { it != Route.Settings }
        _state.update { state ->
            state.copy(
                agentsStack = kept(state.agentsStack),
                hostsStack = kept(state.hostsStack),
                accountSheetOpen = false,
                pairing = if (state.pairing is PairingModal.Code) null else state.pairing,
            )
        }
        for (route in removed.asReversed()) {
            listener?.onRouteLeft(route)
        }
    }

    /**
     * Parses an incoming deep link. Valid pairing links open [PairingModal.Link]
     * without trusting anything yet; auth callbacks are returned to the caller.
     */
    fun openDeepLink(url: String): AppLink? {
        val parsed = DeepLink.parse(url) ?: return null
        if (parsed is AppLink.Pairing) {
            presentPairing(PairingModal.Link(parsed.invitation))
        }
        return parsed
    }

    fun openInvitation(invitation: PairingInvitation) {
        presentPairing(PairingModal.Link(invitation))
    }
}
