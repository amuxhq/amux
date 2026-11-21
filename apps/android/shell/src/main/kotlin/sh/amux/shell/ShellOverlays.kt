package sh.amux.shell

import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.AccountRegistry
import sh.amux.core.runtime.DeletionStore
import sh.amux.core.runtime.RemovalStore
import sh.amux.core.runtime.ReportState
import sh.amux.core.runtime.ReportStore
import sh.amux.core.runtime.SignInPrompt
import sh.amux.core.runtime.SignInState
import sh.amux.core.runtime.StoreBundle
import sh.amux.core.runtime.id
import sh.amux.features.AccountSwitcherSheet
import sh.amux.features.DeleteAccountAction
import sh.amux.features.DeleteAccountSheet
import sh.amux.features.RemoveAccountAction
import sh.amux.features.RemoveAccountDialog
import sh.amux.features.ReportAction
import sh.amux.features.ReportOverlay
import sh.amux.features.SignInOfferAction
import sh.amux.features.SignInOfferSheet
import sh.amux.features.SignInResultAction
import sh.amux.features.SignInResultDialog

/**
 * The account sheets and dialogs that can rise over any screen: the account
 * switcher, a computer offering or asking for a sign-in, how a sign-in went,
 * and removing or deleting an account.
 */
@Composable
internal fun AccountOverlays(
    routerState: RouterState,
    router: Router,
    stores: StoreBundle,
    accounts: AccountRegistry,
    signInState: SignInState,
    removal: RemovalStore?,
    deletion: DeletionStore?,
    onAction: (ShellAction) -> Unit,
) {
    val context = LocalContext.current
    val registry by accounts.state.collectAsState()
    val offersState by stores.offers.state.collectAsState()
    val hostsState by stores.hosts.state.collectAsState()
    val removalAccountId = removal?.account?.collectAsState()?.value
    val deletionState = deletion?.state?.collectAsState()?.value

    if (routerState.accountSheetOpen) {
        AccountSwitcherSheet(
            accounts = AgentsMappers.toAccountsUi(registry),
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

    val signInPrompt = remember(offersState, hostsState) {
        stores.offers.prompt(hostsState)
    }
    if (signInPrompt != null && !routerState.showsPairing && !routerState.accountSheetOpen) {
        SignInOfferSheet(
            state = AccountMappers.toSignInOfferSheetState(prompt = signInPrompt, offers = offersState),
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

    val signInResultUi = AccountMappers.toSignInResultUi(signInState.phase)
    if (signInResultUi != null) {
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

    val removeAccountState = AccountMappers.toRemoveAccountState(removalAccountId, registry)
    if (removeAccountState != null) {
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
        AccountMappers.toDeleteAccountSheetState(
            deletion = it,
            registry = registry,
            formatBillingWarning = { date ->
                context.getString(R.string.shell_delete_billing_warning, date)
            },
        )
    }
    if (deleteAccountState != null) {
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

/** The full-screen problem report, over every route, bar, sheet and dialog. */
@Composable
internal fun ReportHost(
    report: ReportStore,
    reportState: ReportState,
    signedIn: Boolean,
    onAction: (ShellAction) -> Unit,
) {
    val context = LocalContext.current
    val reportUi = ReportMappers.toReportUiState(report = reportState, signedIn = signedIn) ?: return
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
                        report.mark(ReportMappers.toReportMark(action.mark, frame))
                    }
                }
                is ReportAction.UpdateMarkNote -> report.noteOnMark(action.index, action.note)
                is ReportAction.ToggleMarkRedact -> report.toggleMarkRedacted(action.index)
                is ReportAction.RemoveMark -> report.unmark(action.index)
                ReportAction.Send -> onAction(ShellAction.SendReport)
                is ReportAction.CopyReference -> {
                    ShellPlatform.copyToClipboard(context, action.referenceId)
                }
            }
        },
        modifier = Modifier.fillMaxSize(),
    )
}
