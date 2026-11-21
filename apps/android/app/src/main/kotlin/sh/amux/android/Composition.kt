package sh.amux.android

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.lifecycle.ProcessLifecycleOwner
import java.io.File
import java.lang.ref.WeakReference
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.AccountRegistry
import sh.amux.core.runtime.AmuxCloudService
import sh.amux.core.runtime.AndroidLocalDiscovery
import sh.amux.core.runtime.AndroidNetworkWatch
import sh.amux.core.runtime.AppLifecycleObserver
import sh.amux.core.runtime.BillingStoreFront
import sh.amux.core.runtime.CloudError
import sh.amux.core.runtime.CloudService
import sh.amux.core.runtime.DeletionStore
import sh.amux.core.runtime.DiscoverySource
import sh.amux.core.runtime.Entitlement
import sh.amux.core.runtime.EntitlementWatch
import sh.amux.core.runtime.InstallationPaths
import sh.amux.core.runtime.NetworkWatcher
import sh.amux.core.runtime.NoOpPushRegistrar
import sh.amux.core.runtime.PaywallStore
import sh.amux.core.runtime.PurchaseOutcome
import sh.amux.core.runtime.PushRegistrar
import sh.amux.core.runtime.RemovalStore
import sh.amux.core.runtime.ReportFreezing
import sh.amux.core.runtime.ReportStore
import sh.amux.core.runtime.RuntimeCoordinator
import sh.amux.core.runtime.RuntimeStarter
import sh.amux.core.runtime.SignInIntent
import sh.amux.core.runtime.SignInStore
import sh.amux.core.runtime.SignedInAccount
import sh.amux.core.runtime.StoreBundle
import sh.amux.core.runtime.StubBillingStoreFront
import sh.amux.core.runtime.WebAuthPresenter
import sh.amux.core.runtime.failureDescription
import sh.amux.design.AppearanceMode
import sh.amux.features.DiagnosticsExportState
import sh.amux.features.SettingsNotificationStatus
import sh.amux.shell.AndroidSpeechDictation
import sh.amux.shell.AppLink
import sh.amux.shell.DeepLink
import sh.amux.shell.DictationController
import sh.amux.shell.NotificationChannels
import sh.amux.shell.OAuthBrowser
import sh.amux.shell.Router
import sh.amux.shell.ShellAction
import sh.amux.shell.ShellPlatform
import sh.amux.values.RelayLink

/**
 * Optional constructor overrides injected by debug harnesses (`DoorServer`)
 * or unit tests. Release builds never supply overrides.
 */
data class CompositionOverrides(
    val cloud: CloudService? = null,
    val billing: BillingStoreFront? = null,
    val pushRegistrar: PushRegistrar? = null,
    val authPresenter: WebAuthPresenter? = null,
    val dictation: DictationController? = null,
    val options: RuntimeCoordinator.Options? = null,
    val starter: RuntimeStarter? = null,
    val discovery: DiscoverySource? = null,
    val useSystemDiscovery: Boolean = true,
    val networkWatch: NetworkWatcher? = null,
    val autoStart: Boolean = true,
    val attachProcessLifecycle: Boolean = true,
    val deviceName: String? = null,
    val installationDir: File? = null,
    val accountsFile: File? = null,
    val prefsName: String? = null,
    val shareFile: ((Context, File) -> Result<Unit>)? = null,
)

/**
 * Process-scoped manual dependency container assembling the embedded runtime,
 * account registry, stores, navigation router, OAuth Custom Tabs browser,
 * speech dictation, problem reporting, and system observers.
 */
class Composition(
    context: Context,
    overrides: CompositionOverrides = CompositionOverrides(),
) {
    private val appContext: Context = context.applicationContext
    private val prefs: SharedPreferences = appContext.getSharedPreferences(
        overrides.prefsName ?: DEFAULT_PREFS_NAME,
        Context.MODE_PRIVATE,
    )

    val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    val deviceName: String = overrides.deviceName ?: defaultDeviceName()
    val appVersion: String = resolveAppVersion(appContext)

    private val accountsFile: File = overrides.accountsFile
        ?: File(File(appContext.filesDir, "amux"), "accounts.json")

    private val attachmentDir: File = File(appContext.cacheDir, "amux-attachments")

    private var activeActivity: WeakReference<Activity>? = null

    val accounts: AccountRegistry = AccountRegistry(
        file = accountsFile,
        makeStores = { account ->
            StoreBundle(
                account = account,
                scope = scope,
                attachmentDir = attachmentDir,
            )
        },
    )

    val cloud: CloudService = overrides.cloud ?: AmuxCloudService()
    val billing: BillingStoreFront = overrides.billing ?: StubBillingStoreFront()
    val pushRegistrar: PushRegistrar = overrides.pushRegistrar ?: NoOpPushRegistrar()

    val oauthBrowser: OAuthBrowser = OAuthBrowser(
        contextProvider = { activeActivity?.get() ?: appContext },
    )
    val authPresenter: WebAuthPresenter = overrides.authPresenter ?: oauthBrowser
    val dictation: DictationController = overrides.dictation ?: AndroidSpeechDictation(appContext)

    val paywallStoreUnavailable: Boolean
        get() = billing is StubBillingStoreFront

    val pushDeliveryEnabled: Boolean
        get() = pushRegistrar !is NoOpPushRegistrar

    val router: Router = Router()
    val signIn: SignInStore = SignInStore()
    val paywall: PaywallStore = PaywallStore(manageUrl = cloud.endpoint.accountPageUrl)
    val deletion: DeletionStore = DeletionStore()
    val removal: RemovalStore = RemovalStore()
    val report: ReportStore = ReportStore()

    private val shareFileOp: (Context, File) -> Result<Unit> =
        overrides.shareFile ?: { ctx, file -> ShellPlatform.shareFile(ctx, file) }

    val signedOutStores: StoreBundle = StoreBundle(
        account = AccountId("signed-out"),
        scope = scope,
        attachmentDir = attachmentDir,
    )

    private val entitlementWatch: EntitlementWatch

    val runtime: RuntimeCoordinator
    val lifecycleObserver: AppLifecycleObserver

    private val _appearanceMode = MutableStateFlow(readSavedAppearance(prefs))
    val appearanceMode: StateFlow<AppearanceMode> = _appearanceMode.asStateFlow()

    private val _notificationStatus = MutableStateFlow(SettingsNotificationStatus.NotAsked)
    val notificationStatus: StateFlow<SettingsNotificationStatus> = _notificationStatus.asStateFlow()

    private val _notificationsDismissed = MutableStateFlow(
        prefs.getBoolean(PREF_NOTIFICATIONS_DISMISSED, false),
    )
    val notificationsDismissed: StateFlow<Boolean> = _notificationsDismissed.asStateFlow()

    private val _diagnosticsExport = MutableStateFlow<DiagnosticsExportState>(DiagnosticsExportState.Idle)
    val diagnosticsExport: StateFlow<DiagnosticsExportState> = _diagnosticsExport.asStateFlow()

    private val _lastAuthCallback = MutableStateFlow<String?>(null)
    val lastAuthCallback: StateFlow<String?> = _lastAuthCallback.asStateFlow()

    private val attachProcessLifecycle: Boolean = overrides.attachProcessLifecycle
    private var lifecycleAttached: Boolean = false

    init {
        val mainHandler = Handler(Looper.getMainLooper())
        val mainDispatch: (() -> Unit) -> Unit = { block ->
            if (Looper.myLooper() == Looper.getMainLooper()) {
                block()
            } else {
                mainHandler.post(block)
            }
        }

        val installDir = overrides.installationDir ?: InstallationPaths.installationDir(appContext)
        val networkWatcher = overrides.networkWatch ?: AndroidNetworkWatch(appContext, scope = scope)
        val runtimeOptions = overrides.options ?: RuntimeCoordinator.Options()

        runtime = if (overrides.starter != null) {
            RuntimeCoordinator(
                registry = accounts,
                installationDir = installDir,
                deviceName = deviceName,
                signedOutStores = signedOutStores,
                scope = scope,
                dispatch = mainDispatch,
                options = runtimeOptions,
                networkWatch = networkWatcher,
                pushRegistrar = pushRegistrar,
                starter = overrides.starter,
            )
        } else {
            RuntimeCoordinator(
                registry = accounts,
                installationDir = installDir,
                deviceName = deviceName,
                signedOutStores = signedOutStores,
                scope = scope,
                dispatch = mainDispatch,
                options = runtimeOptions,
                networkWatch = networkWatcher,
                pushRegistrar = pushRegistrar,
            )
        }

        runtime.setShareUsage(prefs.getBoolean(PREF_SHARE_USAGE, true))
        runtime.setAutoReports(prefs.getBoolean(PREF_AUTO_REPORTS, false))

        entitlementWatch = EntitlementWatch(
            registry = accounts,
            read = { id ->
                try {
                    cloud.entitlement(id)
                } catch (_: Throwable) {
                    null
                }
            },
            relink = { id -> runtime.refreshEntitlement(id) },
        )
        runtime.entitlementWatch = entitlementWatch

        runtime.discovery = when {
            overrides.discovery != null -> overrides.discovery
            overrides.useSystemDiscovery -> AndroidLocalDiscovery(
                context = appContext,
                dispatch = mainDispatch,
                handOver = { runtime.discovered(it) },
            )
            else -> null
        }

        runtime.accountChanged = {
            router.leaveAccount()
            report.dismiss()
        }

        scope.launch {
            cloud.lend { account -> runtime.bearer(account) }
        }

        NotificationChannels.ensureCreated(appContext)
        syncNotifications(appContext)

        lifecycleObserver = AppLifecycleObserver(coordinator = runtime, scope = scope)

        if (overrides.autoStart) {
            start()
        }
    }

    fun start() {
        if (attachProcessLifecycle && !lifecycleAttached) {
            lifecycleAttached = true
            lifecycleObserver.attach(ProcessLifecycleOwner.get().lifecycle)
        }
        runtime.start()
    }

    /**
     * Tears down this composition completely: cancels pending OAuth sessions,
     * destroys speech dictation, detaches its process lifecycle observer, stops
     * the embedded runtime, and cancels its coroutine scope.
     */
    fun shutdown() {
        oauthBrowser.cancelPending("App shutdown")
        dictation.destroy()
        if (lifecycleAttached) {
            lifecycleAttached = false
            lifecycleObserver.detach(ProcessLifecycleOwner.get().lifecycle)
        }
        runtime.stop()
        scope.cancel()
    }

    fun relaunch() {
        runtime.relaunch()
    }

    fun attachActivity(activity: Activity) {
        activeActivity = WeakReference(activity)
    }

    fun detachActivity(activity: Activity) {
        if (activeActivity?.get() === activity) {
            activeActivity = null
        }
    }

    fun onActivityStopped(activity: Activity) {
        dictation.cancel()
        runtime.stores.stopAllDictation()
        detachActivity(activity)
    }

    fun clearDiagnosticsExport() {
        if (_diagnosticsExport.value !is DiagnosticsExportState.Exporting) {
            _diagnosticsExport.value = DiagnosticsExportState.Idle
        }
    }

    fun onActivityResumed(activity: Activity) {
        activeActivity = WeakReference(activity)
        syncNotifications(activity)
        oauthBrowser.onActivityResumed()
    }

    fun syncNotifications(context: Context = appContext) {
        val status = NotificationChannels.syncPermission(context, pushRegistrar)
        _notificationStatus.value = when {
            status.permitted -> SettingsNotificationStatus.Enabled
            prefs.getBoolean(PREF_NOTIFICATIONS_ASKED, false) -> SettingsNotificationStatus.Off
            else -> SettingsNotificationStatus.NotAsked
        }
    }

    fun onNotificationPermissionResult(context: Context) {
        prefs.edit().putBoolean(PREF_NOTIFICATIONS_ASKED, true).apply()
        syncNotifications(context)
    }

    fun setAppearanceMode(mode: AppearanceMode) {
        _appearanceMode.value = mode
        prefs.edit().putString(PREF_APPEARANCE_MODE, mode.name).apply()
    }

    fun setShareUsage(enabled: Boolean) {
        prefs.edit().putBoolean(PREF_SHARE_USAGE, enabled).apply()
        runtime.setShareUsage(enabled)
    }

    fun setAutoReports(enabled: Boolean) {
        prefs.edit().putBoolean(PREF_AUTO_REPORTS, enabled).apply()
        runtime.setAutoReports(enabled)
    }

    fun handleIncomingUri(uri: Uri): AppLink? {
        if (uri.scheme?.equals("amux", ignoreCase = true) == true &&
            uri.host?.equals("callback", ignoreCase = true) == true
        ) {
            val url = uri.toString()
            _lastAuthCallback.value = url
            oauthBrowser.handleCallbackUri(uri)
            return AppLink.AuthCallback(url)
        }
        return handleDeepLink(uri.toString())
    }

    fun handleDeepLink(url: String): AppLink? {
        val parsed = router.openDeepLink(url) ?: return null
        if (parsed is AppLink.AuthCallback) {
            _lastAuthCallback.value = parsed.url
            oauthBrowser.handleCallbackUrl(parsed.url)
        }
        return parsed
    }

    fun startSignIn(intent: SignInIntent) {
        scope.launch {
            signIn.signIn(
                intent = intent,
                cloud = cloud,
                presenter = authPresenter,
                keep = { account -> bindSignedInAccount(account) },
            )
        }
    }

    private suspend fun bindSignedInAccount(account: SignedInAccount): CloudError? {
        val handover = cloud.handOver(account.id)
            ?: return CloudError.Refused("sign-in did not provide a refresh token")
        val bound = runtime.bind(
            account = account.id,
            cloudUrl = handover.cloudUrl,
            clientId = handover.clientId,
            refreshToken = handover.refreshToken,
        )
        return bound.fold(
            onSuccess = {
                scope.launch { entitlementWatch.look(force = true) }
                null
            },
            onFailure = { err ->
                CloudError.Refused(err.failureDescription)
            },
        )
    }

    fun handleShellAction(
        action: ShellAction,
        activityContext: Context,
        requestNotificationPermission: () -> Unit = {},
        reportFreezer: ReportFreezing? = null,
    ) {
        when (action) {
            is ShellAction.SelectAccount -> {
                if (accounts.selected != action.id) {
                    report.dismiss()
                }
                accounts.select(action.id)
            }
            ShellAction.AddAccount -> startSignIn(SignInIntent.Adding)
            ShellAction.SignIn -> {
                val entry = accounts.selectedAccount?.takeIf {
                    !it.signedIn || runtime.stores.fleet.snapshot.relay == RelayLink.SignInAgain
                }
                if (entry != null) {
                    startSignIn(SignInIntent.Returning(entry.account))
                } else {
                    startSignIn(SignInIntent.Adding)
                }
            }
            is ShellAction.SignInAgain -> {
                val entry = accounts.accounts.firstOrNull { it.id == action.id }
                if (entry != null) {
                    startSignIn(SignInIntent.Returning(entry.account))
                } else {
                    startSignIn(SignInIntent.Adding)
                }
            }
            is ShellAction.SignOut -> {
                if (report.state.value.capture?.account == action.id) {
                    report.dismiss()
                }
                scope.launch {
                    cloud.forgetSession(action.id)
                    runtime.signOut(action.id)
                }
            }
            is ShellAction.RequestRemoveAccount -> removal.ask(action.id)
            is ShellAction.ConfirmRemoveAccount -> {
                removal.dismiss()
                if (report.state.value.capture?.account == action.id) {
                    report.dismiss()
                }
                scope.launch {
                    cloud.forgetSession(action.id)
                    runtime.remove(action.id)
                }
            }
            ShellAction.DismissRemoveAccount -> removal.dismiss()
            is ShellAction.RequestDeleteAccount -> deletion.ask(action.id)
            is ShellAction.UpdateDeleteEmail -> deletion.setTyped(action.text)
            ShellAction.ConfirmDeleteAccount -> performDeleteAccount()
            ShellAction.RetryDeleteAccount -> {
                val snapshot = deletion.snapshot
                deletion.askAgain()
                val email = accounts.accounts.firstOrNull { it.id == snapshot.account }?.account?.email
                if (email != null && snapshot.confirms(email)) {
                    performDeleteAccount()
                }
            }
            ShellAction.DismissDeleteAccount -> deletion.dismiss()
            ShellAction.RetrySignIn -> {
                val intent = signIn.snapshot.intent
                val wasMismatch = signIn.snapshot.phase is SignInStore.Phase.Mismatched
                scope.launch {
                    if (wasMismatch) {
                        signIn.discard(cloud)
                    } else {
                        signIn.dismiss()
                    }
                    startSignIn(intent)
                }
            }
            ShellAction.KeepMismatchedSignIn -> {
                scope.launch {
                    signIn.keep { account -> bindSignedInAccount(account) }
                }
            }
            ShellAction.DismissSignInResult -> {
                if (signIn.snapshot.phase is SignInStore.Phase.Mismatched) {
                    scope.launch { signIn.discard(cloud) }
                } else {
                    signIn.dismiss()
                }
            }
            ShellAction.Subscribe -> {
                paywall.entitled(accounts.selectedAccount?.entitlement ?: Entitlement.None)
                scope.launch {
                    accounts.selected?.let { paywall.refreshEntitlement(cloud, it) }
                    paywall.load(billing)
                }
            }
            is ShellAction.ChoosePaywallPlan -> paywall.choose(action.period)
            ShellAction.PurchasePaywallPlan -> {
                scope.launch {
                    val outcome = paywall.buy(billing)
                    settlePurchaseOutcome(outcome)
                }
            }
            ShellAction.RestorePurchases -> {
                scope.launch {
                    val outcome = paywall.restore(billing)
                    settlePurchaseOutcome(outcome)
                }
            }
            ShellAction.RetryPaywall -> {
                scope.launch {
                    val holding = paywall.snapshot.holding
                    val id = accounts.selected
                    when {
                        holding != null && id != null -> {
                            if (paywall.confirm(holding, cloud, id, billing)) {
                                entitlementWatch.look(force = true)
                                paywall.entitled(accounts.selectedAccount?.entitlement ?: Entitlement.None)
                            }
                        }
                        paywall.snapshot.phase is PaywallStore.Phase.Unconfirmed && id != null -> {
                            entitlementWatch.look(force = true)
                            paywall.refreshEntitlement(cloud, id)
                        }
                        else -> {
                            id?.let { paywall.refreshEntitlement(cloud, it) }
                            paywall.load(billing)
                        }
                    }
                }
            }
            ShellAction.SendInstallLink -> {
                val share = Intent(Intent.ACTION_SEND).apply {
                    type = "text/plain"
                    putExtra(
                        Intent.EXTRA_SUBJECT,
                        activityContext.getString(sh.amux.shell.R.string.shell_install_link_subject),
                    )
                    putExtra(Intent.EXTRA_TEXT, DeepLink.INSTALL_URL)
                }
                val chooser = Intent.createChooser(
                    share,
                    activityContext.getString(sh.amux.shell.R.string.shell_install_link_chooser),
                ).apply {
                    if (activityContext !is Activity) {
                        addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                    }
                }
                runCatching { activityContext.startActivity(chooser) }
            }
            ShellAction.TurnOnNotifications -> {
                val current = _notificationStatus.value
                if (current == SettingsNotificationStatus.NotAsked) {
                    prefs.edit().putBoolean(PREF_NOTIFICATIONS_ASKED, true).apply()
                    requestNotificationPermission()
                } else {
                    openNotificationSettings(activityContext)
                }
            }
            ShellAction.OpenNotificationSettings -> openNotificationSettings(activityContext)
            ShellAction.DismissNotifications -> {
                _notificationsDismissed.value = true
                prefs.edit().putBoolean(PREF_NOTIFICATIONS_DISMISSED, true).apply()
            }
            is ShellAction.SetAppearance -> setAppearanceMode(action.mode)
            is ShellAction.SetShareUsage -> setShareUsage(action.enabled)
            is ShellAction.SetAutoReports -> setAutoReports(action.enabled)
            ShellAction.ReportProblem -> {
                if (reportFreezer != null) {
                    report.begin(reportFreezer)
                }
            }
            ShellAction.ExportDiagnostics -> exportDiagnostics(activityContext)
            ShellAction.ClearDiagnosticsExport -> clearDiagnosticsExport()
            ShellAction.OpenSettings -> {
                scope.launch { entitlementWatch.look(force = false) }
            }
            ShellAction.AcceptReportOffer -> report.accept()
            ShellAction.DismissReport -> report.dismiss()
            ShellAction.SendReport -> {
                scope.launch {
                    report.send(
                        submitter = runtime.profile,
                        account = accounts.selected,
                        build = appVersion,
                    )
                }
            }
        }
    }

    private fun performDeleteAccount() {
        scope.launch {
            deletion.delete(cloud) { deletedId ->
                if (report.state.value.capture?.account == deletedId) {
                    report.dismiss()
                }
                scope.launch {
                    cloud.forgetSession(deletedId)
                    runtime.remove(deletedId)
                }
            }
        }
    }

    private suspend fun settlePurchaseOutcome(outcome: PurchaseOutcome?) {
        if (outcome is PurchaseOutcome.Bought) {
            val id = accounts.selected ?: return
            if (paywall.confirm(outcome.purchase, cloud, id, billing)) {
                entitlementWatch.look(force = true)
                paywall.entitled(accounts.selectedAccount?.entitlement ?: Entitlement.None)
            }
        }
    }

    private fun openNotificationSettings(context: Context) {
        val intent = NotificationChannels.appNotificationSettingsIntent(context)
        runCatching { context.startActivity(intent) }
    }

    private fun exportDiagnostics(activityContext: Context) {
        if (_diagnosticsExport.value is DiagnosticsExportState.Exporting) return
        _diagnosticsExport.value = DiagnosticsExportState.Exporting
        scope.launch {
            val dumped = runtime.stores.dump("user-export")
            dumped.fold(
                onSuccess = { rawFile ->
                    val shareable = runCatching {
                        ShellPlatform.prepareShareableExport(appContext, rawFile)
                    }
                    shareable.fold(
                        onSuccess = { file ->
                            val shared = shareFileOp(activityContext, file)
                            shared.fold(
                                onSuccess = {
                                    _diagnosticsExport.value = DiagnosticsExportState.Exported(file.name)
                                },
                                onFailure = { err ->
                                    _diagnosticsExport.value = DiagnosticsExportState.Failed(err.failureDescription)
                                },
                            )
                        },
                        onFailure = { err ->
                            _diagnosticsExport.value = DiagnosticsExportState.Failed(err.failureDescription)
                        },
                    )
                },
                onFailure = { err ->
                    _diagnosticsExport.value = DiagnosticsExportState.Failed(err.failureDescription)
                },
            )
        }
    }

    companion object {
        private const val DEFAULT_PREFS_NAME: String = "amux_prefs"
        private const val PREF_APPEARANCE_MODE: String = "appearance_mode"
        private const val PREF_SHARE_USAGE: String = "share_usage"
        private const val PREF_AUTO_REPORTS: String = "auto_reports"
        private const val PREF_NOTIFICATIONS_ASKED: String = "notifications_asked"
        private const val PREF_NOTIFICATIONS_DISMISSED: String = "notifications_dismissed"

        private fun readSavedAppearance(prefs: SharedPreferences): AppearanceMode {
            val raw = prefs.getString(PREF_APPEARANCE_MODE, null) ?: return AppearanceMode.System
            return AppearanceMode.entries.firstOrNull { it.name == raw } ?: AppearanceMode.System
        }

        private fun resolveAppVersion(context: Context): String {
            val configured = BuildConfig.VERSION_NAME
            if (configured.isNotBlank()) return configured
            return runCatching {
                context.packageManager.getPackageInfo(context.packageName, 0).versionName.orEmpty()
            }.getOrDefault("")
        }

        private fun defaultDeviceName(): String {
            val model = Build.MODEL?.trim().orEmpty()
            val manufacturer = Build.MANUFACTURER?.trim().orEmpty()
            return when {
                model.isEmpty() -> "Android"
                manufacturer.isNotEmpty() && !model.startsWith(manufacturer, ignoreCase = true) -> {
                    "$manufacturer $model"
                }
                else -> model
            }
        }
    }
}
