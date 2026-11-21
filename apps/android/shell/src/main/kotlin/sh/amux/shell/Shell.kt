package sh.amux.shell

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
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
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.AccountRegistry
import sh.amux.core.runtime.Bridge
import sh.amux.core.runtime.ChatLease
import sh.amux.core.runtime.CloudEndpoint
import sh.amux.core.runtime.DeletionStore
import sh.amux.core.runtime.DictationState
import sh.amux.core.runtime.FleetState
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.HostsState
import sh.amux.core.runtime.NativeNewAgentBridge
import sh.amux.core.runtime.NewAgentBridge
import sh.amux.core.runtime.NewAgentProvider
import sh.amux.core.runtime.NewAgentSource
import sh.amux.core.runtime.NewAgentStore
import sh.amux.core.runtime.PairingStore
import sh.amux.core.runtime.PaywallState
import sh.amux.core.runtime.PaywallStore
import sh.amux.core.runtime.PickedAttachment
import sh.amux.core.runtime.Plan
import sh.amux.core.runtime.RemovalStore
import sh.amux.core.runtime.ReportStore
import sh.amux.core.runtime.ReviewModel
import sh.amux.core.runtime.SignInPrompt
import sh.amux.core.runtime.SignInState
import sh.amux.core.runtime.SignInStore
import sh.amux.core.runtime.StoreBundle
import sh.amux.core.runtime.asNewAgentSource
import sh.amux.core.runtime.failureDescription
import sh.amux.core.runtime.id
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxTheme
import sh.amux.design.AppearanceMode
import sh.amux.design.Space
import sh.amux.features.AccountSwitcherSheet
import sh.amux.features.AgentsHome
import sh.amux.features.AskAction
import sh.amux.features.AttachChoice
import sh.amux.features.ChatOpenedAttachmentUi
import sh.amux.features.ChatScreen
import sh.amux.features.ChatScreenCallbacks
import sh.amux.features.ChatUnavailableScreen
import sh.amux.features.DeleteAccountAction
import sh.amux.features.DeleteAccountSheet
import sh.amux.features.DiagnosticsExportState
import sh.amux.features.HomeAction
import sh.amux.features.HomeScope
import sh.amux.features.HostPage
import sh.amux.features.HostPageAction
import sh.amux.features.HostsAction
import sh.amux.features.HostsTab
import sh.amux.features.NewAgentAction
import sh.amux.features.NewAgentScreen
import sh.amux.features.PairingAction
import sh.amux.features.PairingModalSheet
import sh.amux.features.PaywallAction
import sh.amux.features.PaywallScreen
import sh.amux.features.RemoveAccountAction
import sh.amux.features.RemoveAccountDialog
import sh.amux.features.ReportAction
import sh.amux.features.ReportOfferBanner
import sh.amux.features.ReportOverlay
import sh.amux.features.ReviewPage
import sh.amux.features.ReviewPageAction
import sh.amux.features.ReviewPageState
import sh.amux.features.SettingsAction
import sh.amux.features.SettingsNotificationStatus
import sh.amux.features.SettingsScreen
import sh.amux.features.SettledAction
import sh.amux.features.SettledScreen
import sh.amux.features.SignInOfferAction
import sh.amux.features.SignInOfferSheet
import sh.amux.features.SignInResultAction
import sh.amux.features.SignInResultDialog
import sh.amux.features.TestTags
import sh.amux.features.defaultValidateAgentName
import sh.amux.features.validateFormFields
import sh.amux.values.AgentKey
import sh.amux.values.FleetFilter
import sh.amux.words.OpenableLink
import sh.amux.words.name

private const val GOOGLE_PLAY_SUBSCRIPTIONS_URL: String =
    "https://play.google.com/store/account/subscriptions"

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
    data object OpenSettings : ShellAction
    data object AcceptReportOffer : ShellAction
    data object DismissReport : ShellAction
    data object SendReport : ShellAction
}

/**
 * Root navigation shell hosting the two bottom-bar tabs (Agents, Computers),
 * full-screen detail routes, predictive system back handling, auto-pairing
 * offers, account/sign-in sheets & dialogs, and the screenshot-to-report overlay.
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
    shareUsage: Boolean = true,
    autoReports: Boolean = false,
    appVersion: String = "",
    diagnosticsExport: DiagnosticsExportState = DiagnosticsExportState.Idle,
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
    val context = LocalContext.current
    val colors = AmuxTheme.colors
    val coroutineScope = rememberCoroutineScope()

    val routerState by router.state.collectAsState()
    val accountsState by accounts.state.collectAsState()
    val fleetState by stores.fleet.state.collectAsState()
    val hostsState by stores.hosts.state.collectAsState()
    val pairingState by stores.pairing.state.collectAsState()
    val offersState by stores.offers.state.collectAsState()
    val profileReady by stores.profileReady.collectAsState()

    val signInState = signIn?.state?.collectAsState()?.value ?: SignInState()
    val paywallState = paywall?.state?.collectAsState()?.value ?: PaywallState()
    val removalAccountId = removal?.account?.collectAsState()?.value
    val deletionState = deletion?.state?.collectAsState()?.value
    val reportState = report?.state?.collectAsState()?.value

    val effectiveSigningIn = signingIn || signInState.working
    val reportOpen = reportState?.open == true && reportState.capture != null
    val reportOffering = reportState?.offering == true && !reportOpen

    var homeSearchExpanded by remember { mutableStateOf(false) }
    var startedLinkKey by remember(stores) { mutableStateOf<String?>(null) }
    var settledQuery by remember { mutableStateOf("") }
    var settingsFingerprintCopied by remember { mutableStateOf(false) }

    val signedIn = accountsState.accounts.any { it.signedIn }

    LaunchedEffect(routerState.topRoute) {
        when (routerState.topRoute) {
            Route.Settings -> {
                settingsFingerprintCopied = false
                onAction(ShellAction.OpenSettings)
                if (hostsState.roster == null) {
                    stores.refreshRoster()
                }
            }
            is Route.Host -> {
                if (hostsState.roster == null) {
                    stores.refreshRoster()
                }
            }
            Route.Settled -> Unit
            else -> {
                if (settledQuery.isNotEmpty()) {
                    settledQuery = ""
                }
            }
        }
    }

    // Automatically open pairing when a nearby computer advertises an open
    // pairing window and the user is looking at an empty Agents screen or
    // the Computers root tab.
    val windowCandidate: HostId? = remember(routerState, fleetState, hostsState, reportOpen) {
        if (reportOpen ||
            routerState.pairing != null ||
            routerState.accountSheetOpen ||
            routerState.topRoute != null
        ) {
            null
        } else {
            val canOffer = when (routerState.tab) {
                Tab.Hosts -> true
                Tab.Agents -> fleetState.loaded &&
                    fleetState.rows.isEmpty() &&
                    fleetState.filter == FleetFilter(words = "", only = null)
            }
            if (canOffer) hostsState.windowToOffer?.id else null
        }
    }

    LaunchedEffect(windowCandidate) {
        val candidateId = windowCandidate ?: return@LaunchedEffect
        val hostView = stores.hosts.snapshot.known(candidateId)
        stores.pairing.open(machine = hostView)
        router.presentPairing(PairingModal.Code(candidateId))
    }

    // When a link pairing modal opens and the account is ready (or direct/discovered
    // addresses exist), begin authenticating the invitation once a live profile is attached.
    val activePairingModal = routerState.pairing
    val linkNeedsAccount = remember(activePairingModal, hostsState.discovered) {
        activePairingModal is PairingModal.Link &&
            StateMappers.invitationNeedsAccount(activePairingModal.invitation, hostsState.discovered)
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
                    if (stores.pairLink(
                            link = invitation.link,
                            relayOnly = linkNeedsAccount,
                        )
                    ) {
                        startedLinkKey = invitation.link
                    }
                }
            }
            null -> {
                startedLinkKey = null
            }
        }
    }

    // Auto-close the pairing modal once the machine is trusted.
    LaunchedEffect(pairingState.phase) {
        if (pairingState.phase is PairingStore.Phase.Trusted && router.snapshot.pairing != null) {
            router.dismissPairing()
        }
    }

    val dismissPairingModal: () -> Unit = {
        val current = router.snapshot.pairing
        if (current is PairingModal.Code) {
            stores.hosts.declineWindow(current.host)
        }
        startedLinkKey = null
        stores.cancelPairing()
        router.dismissPairing()
    }

    // Predictive Back / System Back handling across sheets, search, stacks, and tabs.
    val handleBack = !reportOpen && (
        routerState.accountSheetOpen ||
            routerState.showsPairing ||
            homeSearchExpanded ||
            fleetState.filter != FleetFilter(words = "", only = null) ||
            routerState.canPop ||
            routerState.tab != Tab.Agents
        )

    BackHandler(enabled = handleBack) {
        when {
            routerState.accountSheetOpen -> router.dismissAccountSheet()
            routerState.showsPairing -> dismissPairingModal()
            routerState.canPop -> router.pop()
            homeSearchExpanded || fleetState.filter != FleetFilter(words = "", only = null) -> {
                homeSearchExpanded = false
                if (fleetState.filter != FleetFilter(words = "", only = null)) {
                    stores.filterFleet(FleetFilter(words = "", only = null))
                }
            }
            routerState.tab != Tab.Agents -> router.selectTab(Tab.Agents)
        }
    }

    val handleHomeAction: (HomeAction) -> Unit = { action ->
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
            HomeAction.SignIn -> onAction(ShellAction.SignIn)
            is HomeAction.SignInAgain -> {
                router.dismissAccountSheet()
                onAction(ShellAction.SignInAgain(AccountId(action.accountId)))
            }
            HomeAction.Subscribe -> {
                router.push(Route.Paywall(routerState.tab))
                onAction(ShellAction.Subscribe)
            }
            HomeAction.OpenSettings -> {
                router.dismissAccountSheet()
                router.push(Route.Settings)
            }
            is HomeAction.Pair -> {
                HostId.fromBytes(action.hostId)?.let { id ->
                    stores.pairing.open(machine = stores.hosts.snapshot.known(id))
                    router.presentPairing(PairingModal.Code(id))
                }
            }
            HomeAction.SendInstallLink -> onAction(ShellAction.SendInstallLink)
            HomeAction.TurnOnNotifications -> onAction(ShellAction.TurnOnNotifications)
            HomeAction.DismissNotifications -> onAction(ShellAction.DismissNotifications)
            HomeAction.OpenHosts -> router.selectTab(Tab.Hosts)
            HomeAction.Refresh -> stores.fleet.refreshOrder()
            HomeAction.Back -> router.pop()
        }
    }

    Box(modifier = modifier.fillMaxSize()) {
        AmuxGround(modifier = Modifier.testTag(TestTags.SHELL)) {
            Scaffold(
                containerColor = Color.Transparent,
                contentColor = colors.ink,
                contentWindowInsets = WindowInsets(0, 0, 0, 0),
                bottomBar = {
                    if (routerState.showsBottomBar) {
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
                                    selected = routerState.tab == Tab.Agents,
                                    onClick = { router.selectTab(Tab.Agents) },
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
                                    selected = routerState.tab == Tab.Hosts,
                                    onClick = { router.selectTab(Tab.Hosts) },
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
                },
            ) { innerPadding ->
                Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(innerPadding)
                        .consumeWindowInsets(innerPadding),
                ) {
                    when (val top = routerState.topRoute) {
                        null -> {
                            when (routerState.tab) {
                                Tab.Agents -> {
                                    val homeState = StateMappers.toAgentsHomeState(
                                        fleet = fleetState,
                                        hosts = hostsState,
                                        registry = accountsState,
                                        offerNotifications = offerNotifications,
                                        signingIn = effectiveSigningIn,
                                        scope = HomeScope.Fleet,
                                    )
                                    AgentsHome(
                                        state = homeState,
                                        onAction = handleHomeAction,
                                        searchExpanded = homeSearchExpanded,
                                        onSearchExpandedChange = { homeSearchExpanded = it },
                                        onPreviewDelete = { agent -> stores.deleteWarning(agent) },
                                    )
                                }
                                Tab.Hosts -> {
                                    val hostsUiState = StateMappers.toHostsTabState(
                                        hosts = hostsState,
                                        hostsStore = stores.hosts,
                                    )
                                    HostsTab(
                                        state = hostsUiState,
                                        onAction = { action ->
                                            when (action) {
                                                is HostsAction.Open -> {
                                                    HostId.fromBytes(action.hostId)?.let { id ->
                                                        router.push(Route.Host(id))
                                                    }
                                                }
                                                is HostsAction.Pair -> {
                                                    HostId.fromBytes(action.hostId)?.let { id ->
                                                        stores.pairing.open(machine = stores.hosts.snapshot.known(id))
                                                        router.presentPairing(PairingModal.Code(id))
                                                    }
                                                }
                                                HostsAction.Subscribe -> {
                                                    router.push(Route.Paywall(Tab.Hosts))
                                                    onAction(ShellAction.Subscribe)
                                                }
                                            }
                                        },
                                    )
                                }
                            }
                        }
                        is Route.Conversation -> {
                            ConversationRouteContent(
                                agent = top.agent,
                                stores = stores,
                                fleetState = fleetState,
                                hostsState = hostsState,
                                router = router,
                                dictation = dictation,
                            )
                        }
                        is Route.Changes -> {
                            ChangesRouteContent(
                                agent = top.agent,
                                initialFile = top.initialFile,
                                stores = stores,
                                router = router,
                            )
                        }
                        is Route.Host -> {
                            val hostPageState = StateMappers.toHostPageState(
                                hostId = top.host,
                                hosts = hostsState,
                                hostsStore = stores.hosts,
                                offers = offersState,
                            )
                            HostPage(
                                state = hostPageState,
                                onAction = { action ->
                                    when (action) {
                                        HostPageAction.Back -> router.pop()
                                        is HostPageAction.PairAgain -> {
                                            HostId.fromBytes(action.hostId)?.let { id ->
                                                stores.pairing.open(machine = stores.hosts.snapshot.known(id))
                                                router.presentPairing(PairingModal.Code(id))
                                            }
                                        }
                                        is HostPageAction.Revoke -> {
                                            HostId.fromBytes(action.hostId)?.let { id ->
                                                stores.revoke(id)
                                                router.pop()
                                            }
                                        }
                                        HostPageAction.Subscribe -> {
                                            router.push(Route.Paywall(routerState.tab))
                                            onAction(ShellAction.Subscribe)
                                        }
                                        is HostPageAction.OfferSignIn -> {
                                            HostId.fromBytes(action.hostId)?.let { id ->
                                                hostsState.host(id)?.let { hostView ->
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
                        Route.Settled -> {
                            val settledState = StateMappers.toSettledScreenState(
                                fleet = fleetState,
                                query = settledQuery,
                            )
                            SettledScreen(
                                state = settledState,
                                onAction = { action ->
                                    when (action) {
                                        SettledAction.Back -> router.pop()
                                        is SettledAction.Open -> router.push(Route.Conversation(action.agent))
                                        is SettledAction.UpdateQuery -> settledQuery = action.query
                                        is SettledAction.Unsettle -> {
                                            coroutineScope.launch { stores.unfile(action.agent) }
                                        }
                                        is SettledAction.Delete -> {
                                            coroutineScope.launch {
                                                stores.delete(action.agent, action.discardingChanges)
                                            }
                                        }
                                    }
                                },
                                onPreviewDelete = { agent -> stores.deleteWarning(agent) },
                            )
                        }
                        Route.Settings -> {
                            val settingsState = StateMappers.toSettingsScreenState(
                                registry = accountsState,
                                hosts = hostsState,
                                fleet = fleetState,
                                signIn = signInState,
                                appearance = appearanceMode,
                                notificationStatus = notificationStatus,
                                shareUsage = shareUsage,
                                autoReports = autoReports,
                                diagnosticsExport = diagnosticsExport,
                                deviceName = deviceName,
                                fingerprintCopied = settingsFingerprintCopied,
                                appVersion = appVersion,
                            )
                            SettingsScreen(
                                state = settingsState,
                                onAction = { action ->
                                    when (action) {
                                        SettingsAction.Back -> router.pop()
                                        SettingsAction.SignIn -> onAction(ShellAction.SignIn)
                                        SettingsAction.AddAccount -> onAction(ShellAction.AddAccount)
                                        is SettingsAction.SelectAccount -> {
                                            onAction(ShellAction.SelectAccount(AccountId(action.accountId)))
                                        }
                                        is SettingsAction.SignInAgain -> {
                                            onAction(ShellAction.SignInAgain(AccountId(action.accountId)))
                                        }
                                        is SettingsAction.SignOut -> {
                                            onAction(ShellAction.SignOut(AccountId(action.accountId)))
                                        }
                                        is SettingsAction.ManageSubscription -> {
                                            val id = AccountId(action.accountId)
                                            if (accountsState.selected != id) {
                                                onAction(ShellAction.SelectAccount(id))
                                            }
                                            router.push(Route.Paywall(routerState.tab))
                                            onAction(ShellAction.Subscribe)
                                        }
                                        is SettingsAction.RemoveAccount -> {
                                            onAction(ShellAction.RequestRemoveAccount(AccountId(action.accountId)))
                                        }
                                        is SettingsAction.DeleteAccount -> {
                                            onAction(ShellAction.RequestDeleteAccount(AccountId(action.accountId)))
                                        }
                                        is SettingsAction.SetAppearance -> {
                                            onAction(
                                                ShellAction.SetAppearance(
                                                    StateMappers.toAppearanceMode(action.appearance),
                                                ),
                                            )
                                        }
                                        SettingsAction.EnableNotifications -> {
                                            onAction(ShellAction.TurnOnNotifications)
                                        }
                                        SettingsAction.OpenNotificationSettings -> {
                                            onAction(ShellAction.OpenNotificationSettings)
                                        }
                                        is SettingsAction.SetShareUsage -> {
                                            onAction(ShellAction.SetShareUsage(action.enabled))
                                        }
                                        is SettingsAction.SetAutoReports -> {
                                            onAction(ShellAction.SetAutoReports(action.enabled))
                                        }
                                        SettingsAction.ReportProblem -> {
                                            onAction(ShellAction.ReportProblem)
                                        }
                                        SettingsAction.ExportDiagnostics -> {
                                            onAction(ShellAction.ExportDiagnostics)
                                        }
                                        SettingsAction.ContactSupport -> {
                                            ShellPlatform.openExternalUrl(
                                                context,
                                                CloudEndpoint.Production.supportUrl,
                                            )
                                        }
                                        is SettingsAction.CopyFingerprint -> {
                                            ShellPlatform.copyToClipboard(context, action.fingerprint)
                                            settingsFingerprintCopied = true
                                        }
                                    }
                                },
                            )
                        }
                        Route.NewAgent -> {
                            NewAgentRouteContent(
                                stores = stores,
                                fleetState = fleetState,
                                hostsState = hostsState,
                                router = router,
                                sourceProvider = newAgentSourceProvider,
                                bridge = newAgentBridge,
                            )
                        }
                        is Route.Paywall -> {
                            val paywallUiState = StateMappers.toPaywallScreenState(
                                paywall = paywallState,
                                accountEmail = accountsState.selectedAccount?.account?.email,
                                storeUnavailable = paywallStoreUnavailable,
                            )
                            PaywallScreen(
                                state = paywallUiState,
                                onAction = { action ->
                                    when (action) {
                                        PaywallAction.Back -> router.pop()
                                        is PaywallAction.ChoosePlan -> {
                                            onAction(
                                                ShellAction.ChoosePaywallPlan(
                                                    StateMappers.toPlanPeriod(action.period),
                                                ),
                                            )
                                        }
                                        PaywallAction.Subscribe -> onAction(ShellAction.PurchasePaywallPlan)
                                        PaywallAction.RestorePurchases -> onAction(ShellAction.RestorePurchases)
                                        PaywallAction.Retry -> onAction(ShellAction.RetryPaywall)
                                        is PaywallAction.ManageOnWeb -> {
                                            ShellPlatform.openExternalUrl(context, action.url)
                                        }
                                        PaywallAction.ManageInStore -> {
                                            ShellPlatform.openExternalUrl(context, GOOGLE_PLAY_SUBSCRIPTIONS_URL)
                                        }
                                        is PaywallAction.OpenLegalUrl -> {
                                            ShellPlatform.openExternalUrl(context, action.url)
                                        }
                                    }
                                },
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

        // Full-screen problem report overlay covers every route, bottom bar, sheet, and dialog.
        if (reportOpen) {
            val reportUi = StateMappers.toReportUiState(
                report = reportState,
                signedIn = signedIn,
            )
            if (reportUi != null) {
                ReportOverlay(
                    state = reportUi,
                    onAction = { action ->
                        when (action) {
                            ReportAction.Dismiss -> {
                                report.dismiss()
                                onAction(ShellAction.DismissReport)
                            }
                            is ReportAction.SetNote -> report.setNote(action.note)
                            is ReportAction.AddMark -> {
                                val frame = reportState.capture?.frame
                                if (frame != null) {
                                    report.mark(StateMappers.toReportMark(action.mark, frame))
                                }
                            }
                            is ReportAction.UpdateMarkNote -> {
                                report.noteOnMark(action.index, action.note)
                            }
                            is ReportAction.ToggleMarkRedact -> {
                                report.toggleMarkRedacted(action.index)
                            }
                            is ReportAction.RemoveMark -> {
                                report.unmark(action.index)
                            }
                            ReportAction.Send -> onAction(ShellAction.SendReport)
                            is ReportAction.CopyReference -> {
                                ShellPlatform.copyToClipboard(context, action.referenceId)
                            }
                        }
                    },
                    modifier = Modifier.fillMaxSize(),
                )
            }
        }
    }

    if (!reportOpen && routerState.accountSheetOpen) {
        AccountSwitcherSheet(
            accounts = StateMappers.toAccountsUi(accountsState),
            onSelectAccount = { id ->
                router.dismissAccountSheet()
                onAction(ShellAction.SelectAccount(AccountId(id)))
            },
            onSignInAgain = { id ->
                router.dismissAccountSheet()
                onAction(ShellAction.SignInAgain(AccountId(id)))
            },
            onAddAccount = {
                router.dismissAccountSheet()
                onAction(ShellAction.AddAccount)
            },
            onOpenSettings = {
                router.dismissAccountSheet()
                router.push(Route.Settings)
            },
            onDismiss = { router.dismissAccountSheet() },
        )
    }

    if (!reportOpen && routerState.showsPairing && activePairingModal != null) {
        val sheetState = StateMappers.toPairingSheetState(
            pairing = pairingState,
            modal = activePairingModal,
            signedIn = signedIn,
            signingIn = effectiveSigningIn,
            nowMs = stores.pairing.nowMs,
            discovered = hostsState.discovered,
        )
        PairingModalSheet(
            state = sheetState,
            onAction = { action ->
                when (action) {
                    is PairingAction.Digits -> stores.pairDigits(action.typed)
                    is PairingAction.Confirm -> stores.confirmPairing(action.pending)
                    is PairingAction.Abandon -> {
                        stores.abandonPairing(action.pending)
                        dismissPairingModal()
                    }
                    PairingAction.Cancel -> dismissPairingModal()
                    PairingAction.SignIn -> onAction(ShellAction.SignIn)
                    PairingAction.Subscribe -> {
                        router.push(Route.Paywall(routerState.tab))
                        onAction(ShellAction.Subscribe)
                    }
                }
            },
        )
    }

    val signInPrompt = remember(offersState, hostsState) {
        stores.offers.prompt(hostsState)
    }
    if (!reportOpen &&
        signInPrompt != null &&
        !routerState.showsPairing &&
        !routerState.accountSheetOpen
    ) {
        val offerUi = StateMappers.toSignInOfferSheetState(
            prompt = signInPrompt,
            offers = offersState,
        )
        SignInOfferSheet(
            state = offerUi,
            onAction = { action ->
                when (action) {
                    SignInOfferAction.Accept -> {
                        when (signInPrompt) {
                            is SignInPrompt.Offered -> stores.answerOffer(signInPrompt.offer, accept = true)
                            is SignInPrompt.Offering -> stores.offerAccount(signInPrompt.host)
                        }
                    }
                    SignInOfferAction.Decline -> {
                        when (signInPrompt) {
                            is SignInPrompt.Offered -> stores.answerOffer(signInPrompt.offer, accept = false)
                            is SignInPrompt.Offering -> {
                                signInPrompt.host.id?.let { stores.offers.notOffering(it) }
                            }
                        }
                    }
                }
            },
        )
    }

    val signInResultUi = StateMappers.toSignInResultUi(signInState.phase)
    if (!reportOpen && signInResultUi != null) {
        SignInResultDialog(
            result = signInResultUi,
            onAction = { action ->
                when (action) {
                    SignInResultAction.TryAgain -> onAction(ShellAction.RetrySignIn)
                    SignInResultAction.KeepSignedInAccount -> onAction(ShellAction.KeepMismatchedSignIn)
                    SignInResultAction.Dismiss -> onAction(ShellAction.DismissSignInResult)
                }
            },
        )
    }

    val removeAccountState = StateMappers.toRemoveAccountState(removalAccountId, accountsState)
    if (!reportOpen && removeAccountState != null) {
        RemoveAccountDialog(
            state = removeAccountState,
            onAction = { action ->
                when (action) {
                    is RemoveAccountAction.Confirm -> {
                        onAction(ShellAction.ConfirmRemoveAccount(AccountId(action.accountId)))
                    }
                    RemoveAccountAction.Cancel -> onAction(ShellAction.DismissRemoveAccount)
                }
            },
        )
    }

    val deleteAccountState = deletionState?.let {
        StateMappers.toDeleteAccountSheetState(it, accountsState)
    }
    if (!reportOpen && deleteAccountState != null) {
        DeleteAccountSheet(
            state = deleteAccountState,
            onAction = { action ->
                when (action) {
                    is DeleteAccountAction.TypeEmail -> onAction(ShellAction.UpdateDeleteEmail(action.text))
                    DeleteAccountAction.ConfirmDelete -> onAction(ShellAction.ConfirmDeleteAccount)
                    is DeleteAccountAction.OpenBillingPortal -> {
                        ShellPlatform.openExternalUrl(context, action.url)
                    }
                    DeleteAccountAction.TryAgain -> onAction(ShellAction.RetryDeleteAccount)
                    DeleteAccountAction.Cancel -> onAction(ShellAction.DismissDeleteAccount)
                }
            },
        )
    }
}

@Composable
private fun NewAgentRouteContent(
    stores: StoreBundle,
    fleetState: FleetState,
    hostsState: HostsState,
    router: Router,
    sourceProvider: ((StoreBundle) -> NewAgentSource?)?,
    bridge: NewAgentBridge,
) {
    val coroutineScope = rememberCoroutineScope()
    val profileVersion by stores.profileVersion.collectAsState()
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
            hosts = hostsState.hosts,
            rows = fleetState.rows,
        )
    }
    LaunchedEffect(store, hostsState.hosts) {
        store.syncHosts(hostsState.hosts)
    }
    LaunchedEffect(store, fleetState.rows) {
        store.remember(fleetState.rows)
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

    val screenState = StateMappers.toNewAgentScreenState(
        state = state,
        connecting = hostsState.connecting,
    )

    NewAgentScreen(
        state = screenState,
        onAction = { action ->
            when (action) {
                NewAgentAction.Back -> router.pop()
                is NewAgentAction.SelectHost -> {
                    HostId.fromBytes(action.hostId)?.let(store::selectHost)
                }
                is NewAgentAction.SelectProvider -> {
                    NewAgentProvider.fromWire(action.provider)?.let(store::selectProvider)
                }
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

@Composable
private fun rememberChatLease(
    stores: StoreBundle,
    agent: AgentKey,
    retryNonce: Int = 0,
): Result<ChatLease> {
    val profileReady by stores.profileReady.collectAsState()
    val profileVersion by stores.profileVersion.collectAsState()
    val opened = remember(stores, agent, profileReady, profileVersion, retryNonce) {
        runCatching { stores.acquireChat(agent) }
    }
    DisposableEffect(opened) {
        onDispose {
            opened.getOrNull()?.release()
        }
    }
    return opened
}

@Composable
private fun ConversationRouteContent(
    agent: AgentKey,
    stores: StoreBundle,
    fleetState: FleetState,
    hostsState: HostsState,
    router: Router,
    dictation: DictationController? = null,
) {
    val context = LocalContext.current
    val coroutineScope = rememberCoroutineScope()

    val opened = rememberChatLease(stores = stores, agent = agent)
    val model = opened.getOrNull()?.model
    if (model == null) {
        val fleetRow = fleetState.row(agent)
        ChatUnavailableScreen(
            title = fleetRow?.name ?: agent.toString(),
            reason = opened.exceptionOrNull()?.failureDescription ?: "",
            onBack = { router.pop() },
        )
        return
    }

    val currentDictation by rememberUpdatedState(dictation)

    val startDictationSession: () -> Unit = remember(model) {
        {
            val controller = currentDictation
            if (controller == null || !controller.isAvailable) {
                model.prepareDictation(
                    speech = DictationState.Permission.Allowed,
                    microphone = DictationState.Permission.Allowed,
                    available = false,
                )
            } else {
                model.prepareDictation(
                    speech = DictationState.Permission.Allowed,
                    microphone = DictationState.Permission.Allowed,
                    available = true,
                )
                model.beginDictation()
                controller.start(
                    onPartial = { text -> model.heard(text) },
                    onFinal = { text ->
                        if (text.isNotEmpty()) {
                            model.heard(text)
                        }
                        model.stopDictation()
                    },
                    onError = { model.dictationFailed() },
                )
            }
        }
    }

    val recordAudioPermissionLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.RequestPermission(),
    ) { granted ->
        if (granted) {
            startDictationSession()
        } else {
            model.prepareDictation(
                speech = DictationState.Permission.Allowed,
                microphone = DictationState.Permission.Denied,
                available = currentDictation?.isAvailable == true,
            )
        }
    }

    DisposableEffect(model, dictation) {
        onDispose {
            dictation?.stop()
            model.stopDictation()
        }
    }

    val photoPicker = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        if (uri != null) {
            coroutineScope.launch(start = CoroutineStart.UNDISPATCHED) {
                ShellPlatform.attachPickedUri(
                    context = context,
                    uri = uri,
                    isPhoto = true,
                    model = model,
                )
            }
        }
    }

    val filePicker = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.OpenDocument(),
    ) { uri ->
        if (uri != null) {
            coroutineScope.launch(start = CoroutineStart.UNDISPATCHED) {
                ShellPlatform.attachPickedUri(
                    context = context,
                    uri = uri,
                    isPhoto = false,
                    model = model,
                )
            }
        }
    }

    val chatState by model.state.collectAsState()
    var nowMs by remember(stores) { mutableLongStateOf(stores.clock()) }
    val needsClockTick = chatState.frame?.composer?.activity != null ||
        chatState.overview?.jobs?.isNotEmpty() == true
    LaunchedEffect(needsClockTick, stores) {
        nowMs = stores.clock()
        if (!needsClockTick) return@LaunchedEffect
        while (true) {
            delay(1_000L)
            nowMs = stores.clock()
        }
    }

    val openedFile = chatState.openedAttachment
    var openedAttachmentUi by remember(openedFile) {
        mutableStateOf(
            openedFile?.let { file ->
                ChatOpenedAttachmentUi(name = file.name, bytes = null, isImage = false, file = file)
            },
        )
    }
    LaunchedEffect(openedFile) {
        if (openedFile == null) {
            openedAttachmentUi = null
        } else {
            openedAttachmentUi = withContext(Dispatchers.IO) {
                val head = ByteArray(12)
                val read = runCatching {
                    openedFile.inputStream().use { it.read(head) }
                }.getOrDefault(0)
                val sniffedMime = if (read > 0) {
                    PickedAttachment.sniffNativeImageMime(head.copyOf(read))
                } else {
                    null
                }
                val imageBytes = if (sniffedMime != null) {
                    runCatching { openedFile.readBytes() }.getOrNull()
                } else {
                    null
                }
                ChatOpenedAttachmentUi(
                    name = openedFile.name,
                    bytes = imageBytes,
                    isImage = sniffedMime != null,
                    file = openedFile,
                )
            }
        }
    }

    val fleetRow = fleetState.row(agent)
    val subject = StateMappers.toChatSubject(
        agent = agent,
        fleetRow = fleetRow,
        frame = chatState.frame,
        hosts = hostsState,
    )
    val family = remember(stores, agent, fleetState.rows) {
        stores.profile?.family(agent)
    }
    val rowSource = remember(model) { ModelChatRowSource(model) }
    val screenState = StateMappers.toChatScreenState(
        model = model,
        chat = chatState,
        subject = subject,
        family = family,
        nowMs = nowMs,
        openedAttachment = openedAttachmentUi,
        dictationEnabled = dictation?.isAvailable == true,
    )

    val callbacks = remember(model, stores, agent, router, context, dictation) {
        ChatScreenCallbacks(
            onBack = { router.pop() },
            onReadingChange = { atNewest -> model.reading(atNewest) },
            onReachedTop = { firstVisible -> model.reachedTop(firstVisible) },
            onJumpToNewest = { model.jumpToNewest() },
            onToggleExpand = { rowId -> model.toggle(rowId) },
            onToggleRun = { rowId -> model.toggleRun(rowId) },
            onToggleGroup = { rowId -> model.toggleGroup(rowId) },
            onOpenAgent = { target -> router.push(Route.Conversation(target)) },
            onOpenReview = { filePath -> router.push(Route.Changes(agent = agent, initialFile = filePath)) },
            onOpenAttachment = { blob -> model.openAttachment(blob) },
            onOpenAttachmentExternally = { file -> ShellPlatform.openOrShareFile(context, file) },
            onShareAttachment = { file -> ShellPlatform.shareFile(context, file) },
            onClearOpenedAttachment = { model.clearOpenedAttachment() },
            onOpenLink = { link -> ShellPlatform.openExternalLink(context, link) },
            onCopyCode = { code -> ShellPlatform.copyToClipboard(context, code) },
            onCopyAddress = { address -> ShellPlatform.copyToClipboard(context, address) },
            onDraftChange = { text -> model.type(text) },
            onSend = {
                dictation?.stop()
                model.send()
            },
            onResume = {
                dictation?.stop()
                model.resume()
            },
            onInterrupt = { model.interrupt() },
            onAttach = { choice ->
                when (choice) {
                    AttachChoice.Photo -> {
                        photoPicker.launch(
                            PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                        )
                    }
                    AttachChoice.File -> {
                        filePicker.launch(arrayOf("*/*"))
                    }
                }
            },
            onRemoveAttachment = { index -> model.removeAttachment(index) },
            onPickCommand = { match -> model.pick(match) },
            onSendNow = { queued -> model.sendNow(queued) },
            onWithdraw = { queued -> model.withdraw(queued) },
            onResend = { inputId -> model.resend(inputId) },
            onDiscard = { inputId -> model.discard(inputId) },
            onDismissRefused = { inputId -> model.discard(inputId) },
            onSwitchBack = { model.switchBack() },
            onStopJob = { key -> model.stopJob(key) },
            onToggleDictation = {
                val controller = dictation
                if (model.state.value.dictation.active) {
                    controller?.stop()
                    model.stopDictation()
                } else if (controller != null && controller.isAvailable) {
                    val hasMicPermission = ContextCompat.checkSelfPermission(
                        context,
                        Manifest.permission.RECORD_AUDIO,
                    ) == PackageManager.PERMISSION_GRANTED
                    if (hasMicPermission) {
                        startDictationSession()
                    } else {
                        model.prepareDictation(
                            speech = DictationState.Permission.Allowed,
                            microphone = DictationState.Permission.NotAsked,
                            available = true,
                        )
                        recordAudioPermissionLauncher.launch(Manifest.permission.RECORD_AUDIO)
                    }
                } else {
                    model.prepareDictation(
                        speech = DictationState.Permission.Allowed,
                        microphone = DictationState.Permission.Allowed,
                        available = false,
                    )
                }
            },
            onAskAction = { action ->
                when (action) {
                    is AskAction.Choose -> model.answer(
                        choice = action.index,
                        note = action.note,
                        expectedKey = action.askKey.ifEmpty { null },
                    )
                    is AskAction.Respond -> model.answerQuestions(
                        responses = action.responses,
                        expectedKey = action.askKey.ifEmpty { null },
                    )
                    is AskAction.Reply -> model.replyInstead(
                        text = action.text,
                        soFar = action.soFar,
                        expectedKey = action.askKey.ifEmpty { null },
                    )
                    is AskAction.Submit -> model.submitForm(
                        values = action.values,
                        expectedKey = action.askKey.ifEmpty { null },
                    )
                    is AskAction.OpenUrl -> {
                        OpenableLink.parse(action.url)?.let { link ->
                            ShellPlatform.openExternalLink(context, link)
                        }
                    }
                    AskAction.Interrupt -> model.interrupt()
                    AskAction.Resend -> model.resendAnswer()
                    AskAction.Discard -> model.discardAnswer()
                }
            },
            onQuestionDraftChange = { askKey, uiDraft ->
                model.keepQuestionDraft(askKey, StateMappers.toRuntimeQuestionDraft(uiDraft))
            },
            onFormValuesChange = { askKey, values ->
                model.keepFormDraft(askKey, values)
            },
            validateForm = { fields, values ->
                runCatching { model.formProblems(fields, values) }
                    .getOrElse { validateFormFields(fields, values) }
            },
            onSelectComparison = { comparison -> model.compare(comparison) },
            onRetryChangedFiles = { model.retryChangedFiles() },
            onChangeSetting = { setting -> model.change(setting) },
            onOverviewOpenChange = { shown -> model.showOverview(shown) },
            onClearRequestedOverlay = { model.clearAsk() },
            onPin = { coroutineScope.launch { stores.pin(agent) } },
            onUnpin = { coroutineScope.launch { stores.unfile(agent) } },
            onRename = { newName -> coroutineScope.launch { stores.rename(newName, agent) } },
            onSettle = { coroutineScope.launch { stores.close(agent) } },
            onUnsettle = { coroutineScope.launch { stores.unfile(agent) } },
            onCancelSettle = { coroutineScope.launch { stores.cancelClose(agent) } },
            onDelete = { discardingChanges ->
                coroutineScope.launch {
                    val outcome = stores.delete(agent, discardingChanges)
                    if (outcome.isSuccess) {
                        router.pop()
                    }
                }
            },
            onPreviewDelete = { stores.deleteWarning(agent) },
            validateAgentName = { candidate ->
                runCatching { Bridge.agentNameProblem(candidate) }
                    .getOrElse { defaultValidateAgentName(candidate) }
            },
        )
    }

    ChatScreen(
        state = screenState,
        rowSource = rowSource,
        callbacks = callbacks,
    )
}

@Composable
private fun ChangesRouteContent(
    agent: AgentKey,
    initialFile: String?,
    stores: StoreBundle,
    router: Router,
) {
    val coroutineScope = rememberCoroutineScope()
    var retryNonce by remember(stores, agent) { mutableIntStateOf(0) }
    val opened = rememberChatLease(stores = stores, agent = agent, retryNonce = retryNonce)
    val model = opened.getOrNull()?.model
    if (model == null) {
        ReviewPage(
            state = ReviewPageState(
                doc = null,
                error = opened.exceptionOrNull()?.failureDescription,
            ),
            onAction = { action ->
                when (action) {
                    ReviewPageAction.Back -> router.pop()
                    ReviewPageAction.Retry -> retryNonce += 1
                    else -> Unit
                }
            },
        )
        return
    }

    val chatState by model.state.collectAsState()
    var reviewModel by remember(model) { mutableStateOf<ReviewModel?>(model.reviewing) }
    var loading by remember(model) { mutableStateOf(model.reviewing == null) }
    var switching by remember(model) { mutableStateOf(false) }
    var loadError by remember(model) { mutableStateOf<String?>(null) }
    var reloadNonce by remember(model) { mutableIntStateOf(0) }

    LaunchedEffect(model, reloadNonce) {
        loading = reviewModel == null
        loadError = null
        model.loadReview().fold(
            onSuccess = { loaded ->
                reviewModel = loaded
                loadError = null
                if (loaded != null && !initialFile.isNullOrEmpty()) {
                    val matchIndex = loaded.doc.files.indexOfFirst { it.path == initialFile }
                    if (matchIndex >= 0) {
                        loaded.selectFile(matchIndex)
                    }
                }
            },
            onFailure = { failure ->
                loadError = failure.failureDescription
            },
        )
        loading = false
    }

    val activeReview = reviewModel
    val reviewState = activeReview?.state?.collectAsState()?.value

    fun performComparisonSwitch(target: sh.amux.values.Comparison) {
        model.compare(target)
        coroutineScope.launch {
            switching = true
            loadError = null
            val current = reviewModel
            if (current != null) {
                current.switchComparison(target, model.source).fold(
                    onSuccess = { doc ->
                        if (doc == null) {
                            reviewModel = null
                        }
                        loadError = null
                    },
                    onFailure = { failure ->
                        loadError = failure.failureDescription
                    },
                )
            } else {
                loading = true
                model.loadReview().fold(
                    onSuccess = { loaded ->
                        reviewModel = loaded
                        loadError = null
                    },
                    onFailure = { failure ->
                        loadError = failure.failureDescription
                    },
                )
                loading = false
            }
            switching = false
        }
    }

    ReviewPage(
        state = ReviewPageState(
            doc = reviewState?.doc,
            comments = reviewState?.comments.orEmpty(),
            comparison = reviewState?.comparison ?: chatState.comparison,
            comparisons = chatState.comparisons,
            baseBranch = chatState.base,
            selectedFileIndex = reviewState?.activeFile ?: 0,
            loading = loading,
            switching = switching || reviewState?.loading == true,
            error = loadError ?: reviewState?.error,
        ),
        onAction = { action ->
            when (action) {
                ReviewPageAction.Back -> router.pop()
                ReviewPageAction.Retry -> {
                    if (reviewModel != null && (loadError != null || reviewState?.error != null)) {
                        performComparisonSwitch(chatState.comparison)
                    } else {
                        reloadNonce += 1
                    }
                }
                is ReviewPageAction.SelectComparison -> performComparisonSwitch(action.comparison)
                is ReviewPageAction.SelectFile -> {
                    reviewModel?.selectFile(action.fileIndex)
                }
                is ReviewPageAction.AddComment -> {
                    if (!switching) {
                        reviewModel?.addComment(
                            path = action.path,
                            line = action.line,
                            oldLine = action.oldLine,
                            text = action.text,
                        )
                    }
                }
                is ReviewPageAction.EditComment -> {
                    if (!switching) {
                        reviewModel?.rewrite(action.index, action.text)
                    }
                }
                is ReviewPageAction.DeleteComment -> {
                    if (!switching) {
                        reviewModel?.remove(action.index)
                    }
                }
                ReviewPageAction.SendReview -> {
                    val current = reviewModel
                    if (!switching && current != null && current.comments.isNotEmpty()) {
                        model.attach(current)
                        router.pop()
                    }
                }
            }
        },
    )
}
