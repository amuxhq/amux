package sh.amux.shell

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
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.AccountRegistry
import sh.amux.core.runtime.Bridge
import sh.amux.core.runtime.ChatLease
import sh.amux.core.runtime.FleetState
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.HostsState
import sh.amux.core.runtime.PairingStore
import sh.amux.core.runtime.PickedAttachment
import sh.amux.core.runtime.ReviewModel
import sh.amux.core.runtime.StoreBundle
import sh.amux.core.runtime.failureDescription
import sh.amux.core.runtime.id
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxTheme
import sh.amux.features.AccountSwitcherSheet
import sh.amux.features.AgentsHome
import sh.amux.features.AskAction
import sh.amux.features.AttachChoice
import sh.amux.features.ChatOpenedAttachmentUi
import sh.amux.features.ChatScreen
import sh.amux.features.ChatScreenCallbacks
import sh.amux.features.ChatUnavailableScreen
import sh.amux.features.HomeAction
import sh.amux.features.HomeScope
import sh.amux.features.HostPlaceholder
import sh.amux.features.HostsAction
import sh.amux.features.HostsTab
import sh.amux.features.NewAgentPlaceholder
import sh.amux.features.PairingAction
import sh.amux.features.PairingModalSheet
import sh.amux.features.PaywallPlaceholder
import sh.amux.features.ReviewPage
import sh.amux.features.ReviewPageAction
import sh.amux.features.ReviewPageState
import sh.amux.features.SettingsPlaceholder
import sh.amux.features.TestTags
import sh.amux.features.defaultValidateAgentName
import sh.amux.features.reachGroup
import sh.amux.features.validateFormFields
import sh.amux.values.AgentKey
import sh.amux.values.FleetFilter
import sh.amux.words.OpenableLink
import sh.amux.words.name

/**
 * App-level actions the shell delegates to [Composition] (cloud sign-in,
 * account switching, subscription flow, sharing install link, notifications).
 */
sealed interface ShellAction {
    data class SelectAccount(val id: AccountId) : ShellAction
    data object AddAccount : ShellAction
    data object SignIn : ShellAction
    data class SignInAgain(val id: AccountId) : ShellAction
    data object Subscribe : ShellAction
    data object SendInstallLink : ShellAction
    data object TurnOnNotifications : ShellAction
    data object DismissNotifications : ShellAction
}

/**
 * Root navigation shell hosting the two bottom-bar tabs (Agents, Computers),
 * full-screen detail routes, predictive system back handling, auto-pairing
 * offers, and account switcher / pairing modal bottom sheets.
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
) {
    val colors = AmuxTheme.colors
    val coroutineScope = rememberCoroutineScope()

    val routerState by router.state.collectAsState()
    val accountsState by accounts.state.collectAsState()
    val fleetState by stores.fleet.state.collectAsState()
    val hostsState by stores.hosts.state.collectAsState()
    val pairingState by stores.pairing.state.collectAsState()
    val profileReady by stores.profileReady.collectAsState()

    var homeSearchExpanded by remember { mutableStateOf(false) }
    var startedLinkKey by remember(stores) { mutableStateOf<String?>(null) }

    val signedIn = accountsState.accounts.any { it.signedIn }

    // Automatically open pairing when a nearby computer advertises an open
    // pairing window and the user is looking at an empty Agents screen or
    // the Computers root tab.
    val windowCandidate: HostId? = remember(routerState, fleetState, hostsState) {
        if (routerState.pairing != null || routerState.accountSheetOpen || routerState.topRoute != null) {
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
    val handleBack = routerState.accountSheetOpen ||
        routerState.showsPairing ||
        homeSearchExpanded ||
        fleetState.filter != FleetFilter(words = "", only = null) ||
        routerState.canPop ||
        routerState.tab != Tab.Agents

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

    AmuxGround(modifier = modifier.testTag(TestTags.SHELL)) {
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
                                    signingIn = signingIn,
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
                        val hostView = hostsState.host(top.host)
                        HostPlaceholder(
                            hostName = hostView?.name ?: top.host.toString(),
                            reachLabel = hostView?.reachGroup()?.name,
                            onPairAgain = {
                                stores.pairing.open(machine = hostView)
                                router.presentPairing(PairingModal.Code(top.host))
                            },
                            onRevoke = {
                                stores.revoke(top.host)
                                router.pop()
                            },
                            onBack = { router.pop() },
                        )
                    }
                    Route.Settled -> {
                        val settledState = StateMappers.toAgentsHomeState(
                            fleet = fleetState,
                            hosts = hostsState,
                            registry = accountsState,
                            scope = HomeScope.Closed,
                        )
                        AgentsHome(
                            state = settledState,
                            onAction = handleHomeAction,
                            onPreviewDelete = { agent -> stores.deleteWarning(agent) },
                        )
                    }
                    Route.Settings -> {
                        SettingsPlaceholder(
                            accountEmail = accountsState.selectedAccount?.account?.email,
                            deviceName = deviceName,
                            onBack = { router.pop() },
                        )
                    }
                    Route.NewAgent -> {
                        NewAgentPlaceholder(
                            hostNames = hostsState.hosts.map { it.name },
                            onBack = { router.pop() },
                        )
                    }
                    is Route.Paywall -> {
                        PaywallPlaceholder(
                            onBack = { router.pop() },
                        )
                    }
                }
            }
        }
    }

    if (routerState.accountSheetOpen) {
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

    if (routerState.showsPairing && activePairingModal != null) {
        val sheetState = StateMappers.toPairingSheetState(
            pairing = pairingState,
            modal = activePairingModal,
            signedIn = signedIn,
            signingIn = signingIn,
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
    )

    val callbacks = remember(model, stores, agent, router, context) {
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
            onSend = { model.send() },
            onResume = { model.resume() },
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
            onToggleDictation = { },
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
