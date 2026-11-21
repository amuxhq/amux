package sh.amux.shell

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Computer
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.saveable.SaveableStateHolder
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.AccountRegistry
import sh.amux.core.runtime.CloudEndpoint
import sh.amux.core.runtime.DeletionStore
import sh.amux.core.runtime.NativeNewAgentBridge
import sh.amux.core.runtime.NewAgentBridge
import sh.amux.core.runtime.NewAgentSource
import sh.amux.core.runtime.PaywallStore
import sh.amux.core.runtime.Plan
import sh.amux.core.runtime.RemovalStore
import sh.amux.core.runtime.ReportStore
import sh.amux.core.runtime.SignInState
import sh.amux.core.runtime.SignInStore
import sh.amux.core.runtime.StoreBundle
import sh.amux.core.runtime.id
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxTheme
import sh.amux.design.AppearanceMode
import sh.amux.design.Space
import sh.amux.features.DiagnosticsExportState
import sh.amux.features.HomeScope
import sh.amux.features.ReportOfferBanner
import sh.amux.features.SettingsNotificationStatus
import sh.amux.features.TestTags

/**
 * App-level actions the shell delegates to [Composition] (cloud sign-in,
 * account lifecycle, subscription flow, settings preferences, diagnostics
 * export, notifications, and problem reporting).
 */
sealed interface ShellAction {
    data class SelectAccount(val id: AccountId) : ShellAction
    data object AddAccount : ShellAction
    data object SignIn : ShellAction
    data class SignInAgain(val id: AccountId) : ShellAction
    data class SignOut(val id: AccountId) : ShellAction
    data class RequestRemoveAccount(val id: AccountId) : ShellAction
    data class ConfirmRemoveAccount(val id: AccountId) : ShellAction
    data object DismissRemoveAccount : ShellAction
    data class RequestDeleteAccount(val id: AccountId) : ShellAction
    data class UpdateDeleteEmail(val text: String) : ShellAction
    data object ConfirmDeleteAccount : ShellAction
    data object RetryDeleteAccount : ShellAction
    data object DismissDeleteAccount : ShellAction
    data object RetrySignIn : ShellAction
    data object KeepMismatchedSignIn : ShellAction
    data object DismissSignInResult : ShellAction
    data object Subscribe : ShellAction
    data class ChoosePaywallPlan(val period: Plan.Period) : ShellAction
    data object PurchasePaywallPlan : ShellAction
    data object RestorePurchases : ShellAction
    data object RetryPaywall : ShellAction
    data object SendInstallLink : ShellAction
    data object TurnOnNotifications : ShellAction
    data object OpenNotificationSettings : ShellAction
    data object DismissNotifications : ShellAction
    data class SetAppearance(val mode: AppearanceMode) : ShellAction
    data class SetShareUsage(val enabled: Boolean) : ShellAction
    data class SetAutoReports(val enabled: Boolean) : ShellAction
    data object ReportProblem : ShellAction
    data object ExportDiagnostics : ShellAction
    data object ClearDiagnosticsExport : ShellAction
    data object OpenSettings : ShellAction
    data object AcceptReportOffer : ShellAction
    data object DismissReport : ShellAction
    data object SendReport : ShellAction
}

/**
 * Root navigation shell: the two bottom-bar tabs (Agents, Computers), the
 * full-screen route on top of them, system back, and the sheets, dialogs and
 * problem report that can rise over any screen.
 *
 * The shell draws chrome and routing only. Each route collects the stores it
 * draws, so a fleet change recomposes the screen that shows the fleet and
 * not the shell, the bar, or a chat open on top of it.
 */
@Composable
fun Shell(
    router: Router,
    accounts: AccountRegistry,
    stores: StoreBundle,
    deviceName: String,
    offerNotifications: Boolean = false,
    signingIn: Boolean = false,
    onAction: (ShellAction) -> Unit = {},
    modifier: Modifier = Modifier,
    appearanceMode: AppearanceMode = AppearanceMode.System,
    notificationStatus: SettingsNotificationStatus = SettingsNotificationStatus.NotAsked,
    pushDeliveryComingSoon: Boolean = true,
    shareUsage: Boolean = true,
    autoReports: Boolean = false,
    appVersion: String = "",
    diagnosticsExport: DiagnosticsExportState = DiagnosticsExportState.Idle,
    cloudEndpoint: CloudEndpoint = CloudEndpoint.Production,
    signIn: SignInStore? = null,
    paywall: PaywallStore? = null,
    paywallStoreUnavailable: Boolean = false,
    removal: RemovalStore? = null,
    deletion: DeletionStore? = null,
    report: ReportStore? = null,
    dictation: DictationController? = null,
    newAgentSourceProvider: ((StoreBundle) -> NewAgentSource?)? = null,
    newAgentBridge: NewAgentBridge = NativeNewAgentBridge,
) {
    val colors = AmuxTheme.colors

    val routerState by router.state.collectAsState()
    val signInState = signIn?.state?.collectAsState()?.value ?: SignInState()
    val reportState = report?.state?.collectAsState()?.value
    val isSelectedSignedIn by accounts.state.collectPart { registry ->
        registry.accounts.firstOrNull { it.account.id == registry.selected }?.signedIn == true
    }

    val effectiveSigningIn = signingIn || signInState.working
    val reportOpen = reportState?.open == true && reportState.capture != null
    val reportOffering = reportState?.offering == true && !reportOpen
    val overlayOpen = reportOpen || routerState.accountSheetOpen || routerState.showsPairing

    // System back: closes the sheet on top, then pops the stack, then
    // returns to the Agents tab. A route that takes back for itself (the
    // Agents search) registers its own handler inside this one.
    BackHandler(
        enabled = !reportOpen && (
            routerState.accountSheetOpen ||
                routerState.showsPairing ||
                routerState.canPop ||
                routerState.tab != Tab.Agents
            ),
    ) {
        when {
            routerState.accountSheetOpen -> router.dismissAccountSheet()
            routerState.showsPairing -> dismissPairing(stores, router)
            routerState.canPop -> router.pop()
            routerState.tab != Tab.Agents -> router.selectTab(Tab.Agents)
        }
    }

    val settingsChoices = SettingsChoices(
        deviceName = deviceName,
        appearance = appearanceMode,
        notificationStatus = notificationStatus,
        pushDeliveryComingSoon = pushDeliveryComingSoon,
        shareUsage = shareUsage,
        autoReports = autoReports,
        appVersion = appVersion,
        diagnosticsExport = diagnosticsExport,
        cloudEndpoint = cloudEndpoint,
    )

    Box(modifier = modifier.fillMaxSize()) {
        AmuxGround(modifier = Modifier.testTag(TestTags.SHELL)) {
            Scaffold(
                containerColor = Color.Transparent,
                contentColor = colors.ink,
                contentWindowInsets = WindowInsets(0, 0, 0, 0),
                bottomBar = {
                    if (routerState.showsBottomBar) {
                        ShellTabBar(selected = routerState.tab, onSelect = router::selectTab)
                    }
                },
            ) { innerPadding ->
                Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(innerPadding)
                        .consumeWindowInsets(innerPadding),
                ) {
                    ShellRoutes(routerState = routerState) { route ->
                        when (route) {
                            null -> when (routerState.tab) {
                                Tab.Agents -> AgentsRoute(
                                    scope = HomeScope.Fleet,
                                    stores = stores,
                                    accounts = accounts,
                                    router = router,
                                    offerNotifications = offerNotifications,
                                    signingIn = effectiveSigningIn,
                                    backEnabled = !overlayOpen,
                                    onAction = onAction,
                                )
                                Tab.Hosts -> HostsRoute(stores = stores, router = router, onAction = onAction)
                            }
                            is Route.Conversation -> ChatRoute(
                                agent = route.agent,
                                stores = stores,
                                router = router,
                                dictation = dictation,
                            )
                            is Route.Changes -> ChangesRoute(
                                agent = route.agent,
                                initialFile = route.initialFile,
                                stores = stores,
                                router = router,
                            )
                            is Route.Host -> HostRoute(
                                host = route.host,
                                stores = stores,
                                router = router,
                                onAction = onAction,
                            )
                            Route.Settled -> AgentsRoute(
                                scope = HomeScope.Closed,
                                stores = stores,
                                accounts = accounts,
                                router = router,
                                offerNotifications = false,
                                signingIn = effectiveSigningIn,
                                backEnabled = !overlayOpen,
                                onAction = onAction,
                            )
                            Route.Settings -> SettingsRoute(
                                stores = stores,
                                accounts = accounts,
                                router = router,
                                signInState = signInState,
                                choices = settingsChoices,
                                onAction = onAction,
                            )
                            Route.NewAgent -> NewAgentRoute(
                                stores = stores,
                                router = router,
                                sourceProvider = newAgentSourceProvider,
                                bridge = newAgentBridge,
                            )
                            is Route.Paywall -> PaywallRoute(
                                paywall = paywall,
                                accounts = accounts,
                                router = router,
                                storeUnavailable = paywallStoreUnavailable,
                                cloudEndpoint = cloudEndpoint,
                                onAction = onAction,
                            )
                        }
                    }
                }
            }
        }

        // Screenshot offer banner floats gently at the top without blocking the current screen.
        if (reportOffering) {
            ReportOfferBanner(
                onOpen = {
                    report.accept()
                    onAction(ShellAction.AcceptReportOffer)
                },
                onDismiss = {
                    report.dismiss()
                    onAction(ShellAction.DismissReport)
                },
                modifier = Modifier
                    .align(Alignment.TopCenter)
                    .statusBarsPadding()
                    .padding(horizontal = Space.margin, vertical = Space.s),
            )
        }

        if (reportOpen) {
            ReportHost(
                report = report,
                reportState = reportState,
                signedIn = isSelectedSignedIn,
                onAction = onAction,
            )
        }
    }

    PairingHost(
        routerState = routerState,
        router = router,
        stores = stores,
        signedIn = isSelectedSignedIn,
        signingIn = effectiveSigningIn,
        reportOpen = reportOpen,
        onAction = onAction,
    )

    if (!reportOpen) {
        AccountOverlays(
            routerState = routerState,
            router = router,
            stores = stores,
            accounts = accounts,
            signInState = signInState,
            removal = removal,
            deletion = deletion,
            onAction = onAction,
        )
    }
}

/**
 * Draws the route on top of the current tab, or the tab's root when the
 * stack is empty, keeping each one's saved UI state (scroll positions,
 * drafts, an open search) while it is on a stack. Coming back from a chat
 * finds the Agents list where it was left; a route popped off its stack
 * forgets what it kept.
 */
@Composable
private fun ShellRoutes(
    routerState: RouterState,
    content: @Composable (Route?) -> Unit,
) {
    val holder: SaveableStateHolder = rememberSaveableStateHolder()
    val kept = remember { mutableSetOf<String>() }
    val stack = routerState.currentStack
    val top = stack.lastOrNull()
    val key = routeKey(routerState.tab, stack.size, top)

    val live = Tab.entries.flatMapTo(mutableSetOf()) { tab ->
        val tabStack = routerState.stack(tab)
        (0..tabStack.size).map { depth -> routeKey(tab, depth, tabStack.getOrNull(depth - 1)) }
    }
    SideEffect {
        kept += key
        val gone = kept - live
        gone.forEach(holder::removeState)
        kept -= gone
    }

    holder.SaveableStateProvider(key) {
        content(top)
    }
}

/** Names a route by its tab, its depth on that tab's stack and the route itself; depth 0 is the tab's root. */
private fun routeKey(tab: Tab, depth: Int, route: Route?): String =
    if (route == null || depth == 0) "${tab.wireName}/root" else "${tab.wireName}/$depth/$route"

@Composable
private fun ShellTabBar(selected: Tab, onSelect: (Tab) -> Unit) {
    val colors = AmuxTheme.colors
    Column {
        AmuxHairline(color = colors.hairline.copy(alpha = 0.7f))
        NavigationBar(
            containerColor = colors.raised,
            contentColor = colors.ink,
        ) {
            val itemColors = NavigationBarItemDefaults.colors(
                selectedIconColor = colors.ink,
                selectedTextColor = colors.ink,
                unselectedIconColor = colors.inkMuted,
                unselectedTextColor = colors.inkMuted,
                indicatorColor = colors.sunken,
            )
            val agentsLabel = stringResource(R.string.shell_tab_agents)
            val computersLabel = stringResource(R.string.shell_tab_computers)
            NavigationBarItem(
                selected = selected == Tab.Agents,
                onClick = { onSelect(Tab.Agents) },
                icon = {
                    Icon(
                        imageVector = Icons.Outlined.Terminal,
                        contentDescription = null,
                    )
                },
                label = {
                    Text(
                        text = agentsLabel,
                        style = AmuxTheme.typography.caption,
                    )
                },
                colors = itemColors,
                modifier = Modifier.testTag(TestTags.TAB_AGENTS),
            )
            NavigationBarItem(
                selected = selected == Tab.Hosts,
                onClick = { onSelect(Tab.Hosts) },
                icon = {
                    Icon(
                        imageVector = Icons.Outlined.Computer,
                        contentDescription = null,
                    )
                },
                label = {
                    Text(
                        text = computersLabel,
                        style = AmuxTheme.typography.caption,
                    )
                },
                colors = itemColors,
                modifier = Modifier.testTag(TestTags.TAB_HOSTS),
            )
        }
    }
}
