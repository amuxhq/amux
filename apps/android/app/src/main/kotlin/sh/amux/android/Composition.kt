package sh.amux.android

import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.lifecycle.ProcessLifecycleOwner
import java.io.File
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
import sh.amux.core.runtime.CloudService
import sh.amux.core.runtime.DeletionStore
import sh.amux.core.runtime.DiscoverySource
import sh.amux.core.runtime.EntitlementWatch
import sh.amux.core.runtime.InstallationPaths
import sh.amux.core.runtime.NetworkWatcher
import sh.amux.core.runtime.NoOpNetworkWatcher
import sh.amux.core.runtime.NoOpPushRegistrar
import sh.amux.core.runtime.PaywallStore
import sh.amux.core.runtime.PushRegistrar
import sh.amux.core.runtime.RemovalStore
import sh.amux.core.runtime.RuntimeCoordinator
import sh.amux.core.runtime.RuntimeStarter
import sh.amux.core.runtime.SignInStore
import sh.amux.core.runtime.StoreBundle
import sh.amux.core.runtime.StubBillingStoreFront
import sh.amux.design.AppearanceMode
import sh.amux.shell.AppLink
import sh.amux.shell.DeepLink
import sh.amux.shell.Router
import sh.amux.shell.ShellAction

/**
 * Optional constructor overrides injected by debug harnesses (`DoorServer`)
 * or unit tests. Release builds never supply overrides.
 */
data class CompositionOverrides(
    val cloud: CloudService? = null,
    val billing: BillingStoreFront? = null,
    val pushRegistrar: PushRegistrar? = null,
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
)

/**
 * Process-scoped manual dependency container assembling the embedded runtime,
 * account registry, stores, navigation router, and system observers.
 */
class Composition(
    context: Context,
    overrides: CompositionOverrides = CompositionOverrides(),
) {
    private val appContext: Context = context.applicationContext
    val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    val deviceName: String = overrides.deviceName ?: defaultDeviceName()

    private val accountsFile: File = overrides.accountsFile
        ?: File(File(appContext.filesDir, "amux"), "accounts.json")

    val accounts: AccountRegistry = AccountRegistry(
        file = accountsFile,
        makeStores = { account -> StoreBundle(account = account, scope = scope) },
    )

    val cloud: CloudService = overrides.cloud ?: AmuxCloudService()
    val billing: BillingStoreFront = overrides.billing ?: StubBillingStoreFront()
    val pushRegistrar: PushRegistrar = overrides.pushRegistrar ?: NoOpPushRegistrar()

    val router: Router = Router()
    val signIn: SignInStore = SignInStore()
    val paywall: PaywallStore = PaywallStore()
    val deletion: DeletionStore = DeletionStore()
    val removal: RemovalStore = RemovalStore()

    val signedOutStores: StoreBundle = StoreBundle(
        account = AccountId("signed-out"),
        scope = scope,
    )

    private val entitlementWatch: EntitlementWatch

    val runtime: RuntimeCoordinator
    val lifecycleObserver: AppLifecycleObserver

    private val _appearanceMode = MutableStateFlow(AppearanceMode.System)
    val appearanceMode: StateFlow<AppearanceMode> = _appearanceMode.asStateFlow()

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

        runtime.accountChanged = { router.leaveAccount() }

        scope.launch {
            cloud.lend { account -> runtime.bearer(account) }
        }

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
     * Tears down this composition completely: detaches its process lifecycle
     * observer, stops the embedded runtime, and cancels its coroutine scope.
     */
    fun shutdown() {
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

    fun setAppearanceMode(mode: AppearanceMode) {
        _appearanceMode.value = mode
    }

    fun handleDeepLink(url: String): AppLink? {
        val parsed = router.openDeepLink(url) ?: return null
        if (parsed is AppLink.AuthCallback) {
            _lastAuthCallback.value = parsed.url
        }
        return parsed
    }

    fun handleShellAction(action: ShellAction, activityContext: Context) {
        when (action) {
            is ShellAction.SelectAccount -> accounts.select(action.id)
            ShellAction.AddAccount,
            ShellAction.SignIn,
            is ShellAction.SignInAgain,
            -> {
                // Full Custom Tabs PKCE sign-in flow connects here.
            }
            ShellAction.Subscribe -> {
                paywall.entitled(
                    accounts.selectedAccount?.entitlement ?: sh.amux.core.runtime.Entitlement.None,
                )
                scope.launch { paywall.load(billing) }
            }
            ShellAction.SendInstallLink -> {
                val share = Intent(Intent.ACTION_SEND).apply {
                    type = "text/plain"
                    putExtra(Intent.EXTRA_SUBJECT, activityContext.getString(sh.amux.shell.R.string.shell_install_link_subject))
                    putExtra(Intent.EXTRA_TEXT, DeepLink.INSTALL_URL)
                }
                val chooser = Intent.createChooser(
                    share,
                    activityContext.getString(sh.amux.shell.R.string.shell_install_link_chooser),
                ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                runCatching { activityContext.startActivity(chooser) }
            }
            ShellAction.TurnOnNotifications,
            ShellAction.DismissNotifications,
            -> Unit
        }
    }

    companion object {
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
