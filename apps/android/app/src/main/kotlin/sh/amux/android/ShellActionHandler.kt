package sh.amux.android

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import sh.amux.core.runtime.AccountRegistry
import sh.amux.core.runtime.BillingStoreFront
import sh.amux.core.runtime.CloudError
import sh.amux.core.runtime.CloudService
import sh.amux.core.runtime.DeletionStore
import sh.amux.core.runtime.Entitlement
import sh.amux.core.runtime.EntitlementWatch
import sh.amux.core.runtime.PaywallStore
import sh.amux.core.runtime.PurchaseOutcome
import sh.amux.core.runtime.PushRegistrar
import sh.amux.core.runtime.RemovalStore
import sh.amux.core.runtime.ReportFreezing
import sh.amux.core.runtime.ReportStore
import sh.amux.core.runtime.RuntimeCoordinator
import sh.amux.core.runtime.SignInIntent
import sh.amux.core.runtime.SignInStore
import sh.amux.core.runtime.SignedInAccount
import sh.amux.core.runtime.WebAuthPresenter
import sh.amux.core.runtime.failureDescription
import sh.amux.design.AppearanceMode
import sh.amux.features.DiagnosticsExportState
import sh.amux.features.SettingsNotificationStatus
import sh.amux.shell.DeepLink
import sh.amux.shell.NotificationChannels
import sh.amux.shell.ShellAction
import sh.amux.shell.ShellPlatform
import sh.amux.values.RelayLink

/**
 * What the app does when the shell asks: accounts, sign-in, the paywall,
 * problem reports, notifications and diagnostics. [Composition] builds one and
 * hands it every [ShellAction]; it keeps only the wiring.
 *
 * Runs on the main thread; [scope] is main-confined.
 */
class ShellActionHandler(
    private val appContext: Context,
    private val prefs: SharedPreferences,
    private val scope: CoroutineScope,
    private val accounts: AccountRegistry,
    private val runtime: RuntimeCoordinator,
    private val cloud: CloudService,
    private val billing: BillingStoreFront,
    private val pushRegistrar: PushRegistrar,
    private val authPresenter: WebAuthPresenter,
    private val entitlementWatch: EntitlementWatch,
    private val signIn: SignInStore,
    private val paywall: PaywallStore,
    private val deletion: DeletionStore,
    private val removal: RemovalStore,
    private val report: ReportStore,
    private val appVersion: String,
    private val shareFile: (Context, File) -> Result<Unit>,
    private val setAppearance: (AppearanceMode) -> Unit,
    private val setShareUsage: (Boolean) -> Unit,
    private val setAutoReports: (Boolean) -> Unit,
) {
    private val _notificationStatus = MutableStateFlow(SettingsNotificationStatus.NotAsked)
    val notificationStatus: StateFlow<SettingsNotificationStatus> = _notificationStatus.asStateFlow()

    private val _notificationsDismissed = MutableStateFlow(
        prefs.getBoolean(PREF_NOTIFICATIONS_DISMISSED, false),
    )
    val notificationsDismissed: StateFlow<Boolean> = _notificationsDismissed.asStateFlow()

    private val _diagnosticsExport = MutableStateFlow<DiagnosticsExportState>(DiagnosticsExportState.Idle)
    val diagnosticsExport: StateFlow<DiagnosticsExportState> = _diagnosticsExport.asStateFlow()

    fun handle(
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
            ShellAction.ConfirmDeleteAccount -> deleteAccount()
            ShellAction.RetryDeleteAccount -> {
                val snapshot = deletion.snapshot
                deletion.askAgain()
                val email = accounts.accounts.firstOrNull { it.id == snapshot.account }?.account?.email
                if (email != null && snapshot.confirms(email)) {
                    deleteAccount()
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
                scope.launch { settle(paywall.buy(billing)) }
            }
            ShellAction.RestorePurchases -> {
                scope.launch { settle(paywall.restore(billing)) }
            }
            ShellAction.RetryPaywall -> scope.launch { retryPaywall() }
            ShellAction.SendInstallLink -> sendInstallLink(activityContext)
            ShellAction.TurnOnNotifications -> {
                if (_notificationStatus.value == SettingsNotificationStatus.NotAsked) {
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
            is ShellAction.SetAppearance -> setAppearance(action.mode)
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

    // --- Notifications -----------------------------------------------------

    /** Reads whether notifications are allowed now, after the person may have changed it in Settings. */
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

    private fun openNotificationSettings(context: Context) {
        val intent = NotificationChannels.appNotificationSettingsIntent(context)
        runCatching { context.startActivity(intent) }
    }

    // --- Sign-in -----------------------------------------------------------

    private fun startSignIn(intent: SignInIntent) {
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
            onFailure = { err -> CloudError.Refused(err.failureDescription) },
        )
    }

    private fun deleteAccount() {
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

    // --- Paywall -----------------------------------------------------------

    private suspend fun settle(outcome: PurchaseOutcome?) {
        if (outcome is PurchaseOutcome.Bought) {
            val id = accounts.selected ?: return
            if (paywall.confirm(outcome.purchase, cloud, id, billing)) {
                entitlementWatch.look(force = true)
                paywall.entitled(accounts.selectedAccount?.entitlement ?: Entitlement.None)
            }
        }
    }

    private suspend fun retryPaywall() {
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

    // --- Sharing and diagnostics -------------------------------------------

    private fun sendInstallLink(activityContext: Context) {
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

    fun clearDiagnosticsExport() {
        if (_diagnosticsExport.value !is DiagnosticsExportState.Exporting) {
            _diagnosticsExport.value = DiagnosticsExportState.Idle
        }
    }

    private fun exportDiagnostics(activityContext: Context) {
        if (_diagnosticsExport.value is DiagnosticsExportState.Exporting) return
        _diagnosticsExport.value = DiagnosticsExportState.Exporting
        scope.launch {
            _diagnosticsExport.value = runtime.stores.dump("user-export")
                .mapCatching { rawFile -> ShellPlatform.prepareShareableExport(appContext, rawFile) }
                .mapCatching { file -> shareFile(activityContext, file).getOrThrow().let { file } }
                .fold(
                    onSuccess = { file -> DiagnosticsExportState.Exported(file.name) },
                    onFailure = { err -> DiagnosticsExportState.Failed(err.failureDescription) },
                )
        }
    }

    companion object {
        internal const val PREF_NOTIFICATIONS_ASKED: String = "notifications_asked"
        internal const val PREF_NOTIFICATIONS_DISMISSED: String = "notifications_dismissed"
    }
}
