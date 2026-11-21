package sh.amux.android

import android.app.Activity
import android.content.Context
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
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.AccountRegistry
import sh.amux.core.runtime.AmuxCloudService
import sh.amux.core.runtime.AndroidLocalDiscovery
import sh.amux.core.runtime.AndroidNetworkWatch
import sh.amux.core.runtime.AppLifecycleObserver
import sh.amux.core.runtime.BillingStoreFront
import sh.amux.core.runtime.CloudService
import sh.amux.core.runtime.DeletionStore
import sh.amux.core.runtime.DiscoverySource
import sh.amux.core.runtime.EntitlementWatch
import sh.amux.core.runtime.InstallationPaths
import sh.amux.core.runtime.NetworkWatcher
import sh.amux.core.runtime.NoOpPushRegistrar
import sh.amux.core.runtime.PaywallStore
import sh.amux.core.runtime.PushRegistrar
import sh.amux.core.runtime.RemovalStore
import sh.amux.core.runtime.ReportFreezing
import sh.amux.core.runtime.ReportStore
import sh.amux.core.runtime.Runtime
import sh.amux.core.runtime.RuntimeCoordinator
import sh.amux.core.runtime.RuntimeStarter
import sh.amux.core.runtime.SignInStore
import sh.amux.core.runtime.StoreBundle
import sh.amux.core.runtime.StubBillingStoreFront
import sh.amux.core.runtime.WebAuthPresenter
import sh.amux.design.AppearanceMode
import sh.amux.features.DiagnosticsExportState
import sh.amux.features.SettingsNotificationStatus
import sh.amux.shell.AndroidSpeechDictation
import sh.amux.shell.AppLink
import sh.amux.shell.DictationController
import sh.amux.shell.NotificationChannels
import sh.amux.shell.OAuthBrowser
import sh.amux.shell.Router
import sh.amux.shell.ShellAction
import sh.amux.shell.ShellPlatform

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
    val report: ReportStore = ReportStore(redactor = BitmapReportImageRedactor)

    val signedOutStores: StoreBundle = StoreBundle(
        account = AccountId("signed-out"),
        scope = scope,
        attachmentDir = attachmentDir,
    )

    private val entitlementWatch: EntitlementWatch

    val runtime: RuntimeCoordinator
    val lifecycleObserver: AppLifecycleObserver
    private val actions: ShellActionHandler

    private val _appearanceMode = MutableStateFlow(readSavedAppearance(prefs))
    val appearanceMode: StateFlow<AppearanceMode> = _appearanceMode.asStateFlow()

    val notificationStatus: StateFlow<SettingsNotificationStatus>
        get() = actions.notificationStatus

    val notificationsDismissed: StateFlow<Boolean>
        get() = actions.notificationsDismissed

    val diagnosticsExport: StateFlow<DiagnosticsExportState>
        get() = actions.diagnosticsExport

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

        val starter = overrides.starter ?: RuntimeStarter { config, disp, onProfilesMoved, onStopped ->
            Runtime.start(
                config = config,
                dispatch = disp,
                onProfilesMoved = onProfilesMoved,
                onStopped = onStopped,
            )
        }
        runtime = RuntimeCoordinator(
            registry = accounts,
            installationDir = installDir,
            deviceName = deviceName,
            signedOutStores = signedOutStores,
            scope = scope,
            dispatch = mainDispatch,
            options = runtimeOptions,
            networkWatch = networkWatcher,
            pushRegistrar = pushRegistrar,
            starter = starter,
        )

        runtime.setShareUsage(prefs.getBoolean(PREF_SHARE_USAGE, true))
        runtime.setAutoReports(prefs.getBoolean(PREF_AUTO_REPORTS, false))

        entitlementWatch = EntitlementWatch(
            registry = accounts,
            read = { id ->
                try {
                    cloud.entitlement(id)
                } catch (e: CancellationException) {
                    throw e
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

        actions = ShellActionHandler(
            appContext = appContext,
            prefs = prefs,
            scope = scope,
            accounts = accounts,
            runtime = runtime,
            cloud = cloud,
            billing = billing,
            pushRegistrar = pushRegistrar,
            authPresenter = authPresenter,
            entitlementWatch = entitlementWatch,
            signIn = signIn,
            paywall = paywall,
            deletion = deletion,
            removal = removal,
            report = report,
            appVersion = appVersion,
            shareFile = overrides.shareFile ?: { ctx, file -> ShellPlatform.shareFile(ctx, file) },
            setAppearance = ::setAppearanceMode,
            setShareUsage = ::setShareUsage,
            setAutoReports = ::setAutoReports,
        )

        NotificationChannels.ensureCreated(appContext)
        actions.syncNotifications(appContext)

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
     *
     * Waits for the runtime to stop, because whoever replaces a composition
     * (the debug door, tests) starts another runtime on the same store or
     * deletes it next. The app itself never calls this.
     */
    fun shutdown() {
        oauthBrowser.cancelPending("App shutdown")
        dictation.destroy()
        if (lifecycleAttached) {
            lifecycleAttached = false
            lifecycleObserver.detach(ProcessLifecycleOwner.get().lifecycle)
        }
        val stopping = runtime.stop()
        scope.cancel()
        runBlocking { stopping.join() }
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
        actions.clearDiagnosticsExport()
    }

    fun onActivityResumed(activity: Activity) {
        activeActivity = WeakReference(activity)
        syncNotifications(activity)
        oauthBrowser.onActivityResumed()
    }

    fun syncNotifications(context: Context = appContext) {
        actions.syncNotifications(context)
    }

    fun onNotificationPermissionResult(context: Context) {
        actions.onNotificationPermissionResult(context)
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

    fun handleShellAction(
        action: ShellAction,
        activityContext: Context,
        requestNotificationPermission: () -> Unit = {},
        reportFreezer: ReportFreezing? = null,
    ) {
        actions.handle(action, activityContext, requestNotificationPermission, reportFreezer)
    }

    companion object {
        private const val DEFAULT_PREFS_NAME: String = "amux_prefs"
        private const val PREF_APPEARANCE_MODE: String = "appearance_mode"
        private const val PREF_SHARE_USAGE: String = "share_usage"
        private const val PREF_AUTO_REPORTS: String = "auto_reports"

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
