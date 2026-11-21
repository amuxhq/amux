package sh.amux.shell

import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.AccountRegistry
import sh.amux.core.runtime.CloudEndpoint
import sh.amux.core.runtime.PaywallState
import sh.amux.core.runtime.PaywallStore
import sh.amux.core.runtime.SignInState
import sh.amux.core.runtime.StoreBundle
import sh.amux.design.AppearanceMode
import sh.amux.features.DiagnosticsExportState
import sh.amux.features.PaywallAction
import sh.amux.features.PaywallScreen
import sh.amux.features.SettingsAction
import sh.amux.features.SettingsNotificationStatus
import sh.amux.features.SettingsScreen

private const val GOOGLE_PLAY_SUBSCRIPTIONS_URL: String =
    "https://play.google.com/store/account/subscriptions"

/** What Settings shows that the app, not a store, decides. */
internal data class SettingsChoices(
    val deviceName: String,
    val appearance: AppearanceMode,
    val notificationStatus: SettingsNotificationStatus,
    val pushDeliveryComingSoon: Boolean,
    val shareUsage: Boolean,
    val autoReports: Boolean,
    val appVersion: String,
    val diagnosticsExport: DiagnosticsExportState,
    val cloudEndpoint: CloudEndpoint,
)

@Composable
internal fun SettingsRoute(
    stores: StoreBundle,
    accounts: AccountRegistry,
    router: Router,
    signInState: SignInState,
    choices: SettingsChoices,
    onAction: (ShellAction) -> Unit,
) {
    val context = LocalContext.current
    val registry by accounts.state.collectAsState()
    val hosts by stores.hosts.state.collectAsState()
    val relay by stores.fleet.state.collectPart { it.relay }
    var fingerprintCopied by remember { mutableStateOf(false) }
    val currentOnAction by rememberUpdatedState(onAction)

    // A diagnostics export belongs to the visit that made it: each visit
    // starts without one, and leaving Settings drops it.
    LaunchedEffect(stores) {
        currentOnAction(ShellAction.ClearDiagnosticsExport)
        currentOnAction(ShellAction.OpenSettings)
        if (stores.hosts.snapshot.roster == null) {
            stores.refreshRoster()
        }
    }
    DisposableEffect(Unit) {
        onDispose { currentOnAction(ShellAction.ClearDiagnosticsExport) }
    }

    val state = remember(registry, hosts, relay, signInState, choices, fingerprintCopied) {
        SettingsMappers.toSettingsScreenState(
            registry = registry,
            hosts = hosts,
            relay = relay,
            signIn = signInState,
            appearance = choices.appearance,
            notificationStatus = choices.notificationStatus,
            pushDeliveryComingSoon = choices.pushDeliveryComingSoon,
            shareUsage = choices.shareUsage,
            autoReports = choices.autoReports,
            diagnosticsExport = choices.diagnosticsExport,
            deviceName = choices.deviceName,
            fingerprintCopied = fingerprintCopied,
            appVersion = choices.appVersion,
        )
    }
    SettingsScreen(
        state = state,
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
                    if (accounts.state.value.selected != id) {
                        onAction(ShellAction.SelectAccount(id))
                    }
                    router.push(Route.Paywall(router.snapshot.tab))
                    onAction(ShellAction.Subscribe)
                }
                is SettingsAction.RemoveAccount -> {
                    onAction(ShellAction.RequestRemoveAccount(AccountId(action.accountId)))
                }
                is SettingsAction.DeleteAccount -> {
                    onAction(ShellAction.RequestDeleteAccount(AccountId(action.accountId)))
                }
                is SettingsAction.SetAppearance -> {
                    onAction(ShellAction.SetAppearance(SettingsMappers.toAppearanceMode(action.appearance)))
                }
                SettingsAction.EnableNotifications -> onAction(ShellAction.TurnOnNotifications)
                SettingsAction.OpenNotificationSettings -> onAction(ShellAction.OpenNotificationSettings)
                is SettingsAction.SetShareUsage -> onAction(ShellAction.SetShareUsage(action.enabled))
                is SettingsAction.SetAutoReports -> onAction(ShellAction.SetAutoReports(action.enabled))
                SettingsAction.ReportProblem -> onAction(ShellAction.ReportProblem)
                SettingsAction.ExportDiagnostics -> onAction(ShellAction.ExportDiagnostics)
                SettingsAction.ContactSupport -> {
                    ShellPlatform.openExternalUrl(context, choices.cloudEndpoint.supportUrl)
                }
                is SettingsAction.CopyFingerprint -> {
                    ShellPlatform.copyToClipboard(context, action.fingerprint)
                    fingerprintCopied = true
                }
            }
        },
    )
}

@Composable
internal fun PaywallRoute(
    paywall: PaywallStore?,
    accounts: AccountRegistry,
    router: Router,
    storeUnavailable: Boolean,
    cloudEndpoint: CloudEndpoint,
    onAction: (ShellAction) -> Unit,
) {
    val context = LocalContext.current
    val paywallState = paywall?.state?.collectAsState()?.value ?: PaywallState()
    val email by accounts.state.collectPart { it.selectedAccount?.account?.email }
    val state = remember(paywallState, email, storeUnavailable, cloudEndpoint) {
        SettingsMappers.toPaywallScreenState(
            paywall = paywallState,
            accountEmail = email,
            storeUnavailable = storeUnavailable,
            privacyUrl = cloudEndpoint.privacyUrl,
            formatRenews = { date -> context.getString(R.string.shell_paywall_renews, date) },
        )
    }
    PaywallScreen(
        state = state,
        onAction = { action ->
            when (action) {
                PaywallAction.Back -> router.pop()
                is PaywallAction.ChoosePlan -> {
                    onAction(ShellAction.ChoosePaywallPlan(SettingsMappers.toPlanPeriod(action.period)))
                }
                PaywallAction.Subscribe -> onAction(ShellAction.PurchasePaywallPlan)
                PaywallAction.RestorePurchases -> onAction(ShellAction.RestorePurchases)
                PaywallAction.Retry -> onAction(ShellAction.RetryPaywall)
                is PaywallAction.ManageOnWeb -> ShellPlatform.openExternalUrl(context, action.url)
                PaywallAction.ManageInStore -> {
                    ShellPlatform.openExternalUrl(context, GOOGLE_PLAY_SUBSCRIPTIONS_URL)
                }
                is PaywallAction.OpenLegalUrl -> ShellPlatform.openExternalUrl(context, action.url)
            }
        },
    )
}
