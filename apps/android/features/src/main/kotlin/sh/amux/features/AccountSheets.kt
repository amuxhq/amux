package sh.amux.features

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.words.RowState

/** Semantic test tags for account sign-in, removal, and deletion sheets and dialogs. */
object AccountSheetTags {
    const val SIGN_IN_OFFER_SHEET: String = "account.sign-in-offer.sheet"
    const val SIGN_IN_OFFER_TITLE: String = "account.sign-in-offer.title"
    const val SIGN_IN_OFFER_DETAIL: String = "account.sign-in-offer.detail"
    const val SIGN_IN_OFFER_FAILURE: String = "account.sign-in-offer.failure"
    const val SIGN_IN_OFFER_ACCEPT: String = "account.sign-in-offer.accept"
    const val SIGN_IN_OFFER_DECLINE: String = "account.sign-in-offer.decline"
    const val SIGN_IN_OFFER_WORKING: String = "account.sign-in-offer.working"
    const val SIGN_IN_OFFER_CLOSE: String = "account.sign-in-offer.close"

    const val SIGN_IN_RESULT_DIALOG: String = "account.sign-in-result.dialog"
    const val SIGN_IN_RESULT_TITLE: String = "account.sign-in-result.title"
    const val SIGN_IN_RESULT_BODY: String = "account.sign-in-result.body"
    const val SIGN_IN_RESULT_TRY_AGAIN: String = "account.sign-in-result.try-again"
    const val SIGN_IN_RESULT_KEEP: String = "account.sign-in-result.keep"
    const val SIGN_IN_RESULT_DISMISS: String = "account.sign-in-result.dismiss"

    const val REMOVE_DIALOG: String = "account.remove.dialog"
    const val REMOVE_TITLE: String = "account.remove.title"
    const val REMOVE_DETAIL: String = "account.remove.detail"
    const val REMOVE_CONFIRM: String = "account.remove.confirm"
    const val REMOVE_CANCEL: String = "account.remove.cancel"

    const val DELETE_SHEET: String = "account.delete.sheet"
    const val DELETE_TITLE: String = "account.delete.title"
    const val DELETE_EMAIL_INPUT: String = "account.delete.email"
    const val DELETE_CONFIRM: String = "account.delete.confirm"
    const val DELETE_WORKING: String = "account.delete.working"
    const val DELETE_CANCEL: String = "account.delete.cancel"
    const val DELETE_CLOSE: String = "account.delete.close"
    const val DELETE_BLOCKED: String = "account.delete.blocked"
    const val DELETE_BILLING_PORTAL: String = "account.delete.billing-portal"
    const val DELETE_TRY_AGAIN: String = "account.delete.try-again"
    const val DELETE_FAILED: String = "account.delete.failed"
}

// =============================================================================
// Sign-in offer sheet (received from a paired host OR offered to a paired host)
// =============================================================================

/** Direction of an account sign-in offer across a paired link. */
@Immutable
sealed interface SignInOfferPromptUi {
    /** A paired host offers its account to sign this phone in. */
    data class Offered(
        val hostName: String,
        val email: String,
    ) : SignInOfferPromptUi

    /** This phone offers its account to a newly paired host. */
    data class Offering(
        val hostName: String,
        val email: String,
    ) : SignInOfferPromptUi
}

/** Failure surfaced when an offer could not complete. */
@Immutable
sealed interface SignInOfferFailureUi {
    data class Unreachable(val hostName: String) : SignInOfferFailureUi
    data class Refused(val reason: String? = null) : SignInOfferFailureUi
}

/** Pure UI state rendered by [SignInOfferSheet] and [SignInOfferSheetContent]. */
@Immutable
data class SignInOfferSheetState(
    val prompt: SignInOfferPromptUi,
    val working: Boolean = false,
    val failure: SignInOfferFailureUi? = null,
)

/** Actions emitted by [SignInOfferSheet] and [SignInOfferSheetContent]. */
sealed interface SignInOfferAction {
    data object Accept : SignInOfferAction
    data object Decline : SignInOfferAction
}

/** M3 [ModalBottomSheet] presenting an incoming or outgoing paired sign-in offer. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SignInOfferSheet(
    state: SignInOfferSheetState,
    onAction: (SignInOfferAction) -> Unit,
) {
    val colors = AmuxTheme.colors
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(
        onDismissRequest = { if (!state.working) onAction(SignInOfferAction.Decline) },
        sheetState = sheetState,
        containerColor = colors.ground,
        contentColor = colors.ink,
    ) {
        SignInOfferSheetContent(
            state = state,
            onAction = onAction,
        )
    }
}

/** Pure Compose body of the sign-in offer sheet for direct storybook and Robolectric rendering. */
@Composable
fun SignInOfferSheetContent(
    state: SignInOfferSheetState,
    onAction: (SignInOfferAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val closeLabel = stringResource(R.string.action_close)

    val headline = when (val prompt = state.prompt) {
        is SignInOfferPromptUi.Offered -> stringResource(R.string.account_sheet_offer_received_title, prompt.email)
        is SignInOfferPromptUi.Offering -> stringResource(
            R.string.account_sheet_offer_sending_title,
            prompt.hostName,
            prompt.email,
        )
    }
    val detail = when (val prompt = state.prompt) {
        is SignInOfferPromptUi.Offered -> stringResource(R.string.account_sheet_offer_received_detail, prompt.hostName)
        is SignInOfferPromptUi.Offering -> stringResource(R.string.account_sheet_offer_sending_detail, prompt.hostName)
    }
    val primaryLabel = when (state.prompt) {
        is SignInOfferPromptUi.Offered -> stringResource(R.string.account_sheet_offer_accept)
        is SignInOfferPromptUi.Offering -> stringResource(R.string.account_sheet_offer_send)
    }
    val secondaryLabel = when (state.prompt) {
        is SignInOfferPromptUi.Offered -> stringResource(R.string.account_sheet_offer_decline)
        is SignInOfferPromptUi.Offering -> stringResource(R.string.account_sheet_offer_not_now)
    }

    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(colors.ground)
            .windowInsetsPadding(WindowInsets.navigationBars)
            .padding(bottom = Space.xl)
            .testTag(AccountSheetTags.SIGN_IN_OFFER_SHEET),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.s, vertical = Space.xs),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(
                onClick = { onAction(SignInOfferAction.Decline) },
                enabled = !state.working,
                modifier = Modifier
                    .thumbTarget()
                    .testTag(AccountSheetTags.SIGN_IN_OFFER_CLOSE),
            ) {
                Icon(
                    imageVector = Icons.Outlined.Close,
                    contentDescription = closeLabel,
                    tint = colors.inkMuted,
                )
            }
            Text(
                text = stringResource(R.string.account_sheet_sign_in_title),
                style = AmuxTheme.typography.identifier,
                color = colors.ink,
                modifier = Modifier.weight(1f),
            )
        }
        AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))

        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin, vertical = Space.l),
            verticalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
                Text(
                    text = headline,
                    style = AmuxTheme.typography.screenTitle,
                    color = colors.ink,
                    modifier = Modifier.testTag(AccountSheetTags.SIGN_IN_OFFER_TITLE),
                )
                Text(
                    text = detail,
                    style = AmuxTheme.typography.body,
                    color = colors.inkMuted,
                    modifier = Modifier.testTag(AccountSheetTags.SIGN_IN_OFFER_DETAIL),
                )
            }

            val failure = state.failure
            if (failure != null) {
                val failureMessage = when (failure) {
                    is SignInOfferFailureUi.Unreachable -> if (failure.hostName.isNotBlank()) {
                        stringResource(R.string.account_sheet_offer_unreachable_host, failure.hostName)
                    } else {
                        stringResource(R.string.account_sheet_offer_unreachable_generic)
                    }
                    is SignInOfferFailureUi.Refused -> failure.reason?.takeIf { it.isNotBlank() }
                        ?: stringResource(R.string.account_sheet_offer_refused)
                }
                AmuxPanel(
                    borderColor = colors.removed.copy(alpha = 0.55f),
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag(AccountSheetTags.SIGN_IN_OFFER_FAILURE),
                ) {
                    Text(
                        text = failureMessage,
                        style = AmuxTheme.typography.detail,
                        color = colors.removed,
                        modifier = Modifier.padding(Space.m),
                    )
                }
            }

            Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                if (state.working) {
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .thumbTarget()
                            .testTag(AccountSheetTags.SIGN_IN_OFFER_WORKING),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.Center,
                    ) {
                        AmuxStateMark(state = RowState.Working, size = 10.dp)
                        Spacer(modifier = Modifier.padding(horizontal = Space.xs))
                        Text(
                            text = stringResource(R.string.settings_signing_in),
                            style = AmuxTheme.typography.bodyEmphasis,
                            color = colors.inkMuted,
                        )
                    }
                } else {
                    AmuxActionButton(
                        label = primaryLabel,
                        kind = ActionButtonKind.Primary,
                        fillWidth = true,
                        onClick = { onAction(SignInOfferAction.Accept) },
                        modifier = Modifier.testTag(AccountSheetTags.SIGN_IN_OFFER_ACCEPT),
                    )
                }
                AmuxActionButton(
                    label = secondaryLabel,
                    kind = ActionButtonKind.Outline,
                    fillWidth = true,
                    enabled = !state.working,
                    onClick = { onAction(SignInOfferAction.Decline) },
                    modifier = Modifier.testTag(AccountSheetTags.SIGN_IN_OFFER_DECLINE),
                )
            }
        }
    }
}

// =============================================================================
// Sign-in result dialog (mismatched account OR sign-in failure)
// =============================================================================

/** Outcome requiring user attention after a browser sign-in completes. */
@Immutable
sealed interface SignInResultUi {
    data class Mismatched(
        val expectedEmail: String,
        val signedInEmail: String,
    ) : SignInResultUi

    data class Failed(
        val reason: String,
    ) : SignInResultUi
}

/** Actions emitted by [SignInResultDialog] and [SignInResultDialogContent]. */
sealed interface SignInResultAction {
    data object TryAgain : SignInResultAction
    data object KeepSignedInAccount : SignInResultAction
    data object Dismiss : SignInResultAction
}

/** Modal dialog presenting a browser sign-in mismatch or refusal. */
@Composable
fun SignInResultDialog(
    result: SignInResultUi,
    onAction: (SignInResultAction) -> Unit,
) {
    Dialog(onDismissRequest = { onAction(SignInResultAction.Dismiss) }) {
        SignInResultDialogContent(
            result = result,
            onAction = onAction,
        )
    }
}

/** Pure Compose body of [SignInResultDialog] for direct storybook and Robolectric rendering. */
@Composable
fun SignInResultDialogContent(
    result: SignInResultUi,
    onAction: (SignInResultAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    AmuxPanel(
        shape = RoundedCornerShape(AmuxTheme.metrics.floatRadius),
        modifier = modifier
            .fillMaxWidth()
            .testTag(AccountSheetTags.SIGN_IN_RESULT_DIALOG),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(Space.xl),
            verticalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            when (result) {
                is SignInResultUi.Mismatched -> {
                    Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
                        Text(
                            text = stringResource(R.string.account_sheet_mismatch_title),
                            style = AmuxTheme.typography.identifier,
                            color = colors.ink,
                            modifier = Modifier.testTag(AccountSheetTags.SIGN_IN_RESULT_TITLE),
                        )
                        Text(
                            text = stringResource(
                                R.string.account_sheet_mismatch_detail,
                                result.signedInEmail,
                                result.expectedEmail,
                            ),
                            style = AmuxTheme.typography.detail,
                            color = colors.inkMuted,
                            modifier = Modifier.testTag(AccountSheetTags.SIGN_IN_RESULT_BODY),
                        )
                    }
                    Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                        AmuxActionButton(
                            label = stringResource(R.string.account_sheet_mismatch_try_again),
                            kind = ActionButtonKind.Primary,
                            fillWidth = true,
                            onClick = { onAction(SignInResultAction.TryAgain) },
                            modifier = Modifier.testTag(AccountSheetTags.SIGN_IN_RESULT_TRY_AGAIN),
                        )
                        AmuxActionButton(
                            label = stringResource(
                                R.string.account_sheet_mismatch_continue,
                                result.signedInEmail,
                            ),
                            kind = ActionButtonKind.Outline,
                            fillWidth = true,
                            onClick = { onAction(SignInResultAction.KeepSignedInAccount) },
                            modifier = Modifier.testTag(AccountSheetTags.SIGN_IN_RESULT_KEEP),
                        )
                        TextButton(
                            onClick = { onAction(SignInResultAction.Dismiss) },
                            modifier = Modifier
                                .fillMaxWidth()
                                .thumbTarget()
                                .testTag(AccountSheetTags.SIGN_IN_RESULT_DISMISS),
                        ) {
                            Text(
                                text = stringResource(R.string.action_cancel),
                                style = AmuxTheme.typography.bodyEmphasis,
                                color = colors.inkMuted,
                            )
                        }
                    }
                }
                is SignInResultUi.Failed -> {
                    Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
                        Text(
                            text = stringResource(R.string.account_sheet_failed_title),
                            style = AmuxTheme.typography.identifier,
                            color = colors.ink,
                            modifier = Modifier.testTag(AccountSheetTags.SIGN_IN_RESULT_TITLE),
                        )
                        Text(
                            text = result.reason,
                            style = AmuxTheme.typography.detail,
                            color = colors.inkMuted,
                            modifier = Modifier.testTag(AccountSheetTags.SIGN_IN_RESULT_BODY),
                        )
                    }
                    Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                        AmuxActionButton(
                            label = stringResource(R.string.account_sheet_failed_try_again),
                            kind = ActionButtonKind.Primary,
                            fillWidth = true,
                            onClick = { onAction(SignInResultAction.TryAgain) },
                            modifier = Modifier.testTag(AccountSheetTags.SIGN_IN_RESULT_TRY_AGAIN),
                        )
                        AmuxActionButton(
                            label = stringResource(R.string.account_sheet_failed_dismiss),
                            kind = ActionButtonKind.Outline,
                            fillWidth = true,
                            onClick = { onAction(SignInResultAction.Dismiss) },
                            modifier = Modifier.testTag(AccountSheetTags.SIGN_IN_RESULT_DISMISS),
                        )
                    }
                }
            }
        }
    }
}

// =============================================================================
// Remove account confirmation dialog
// =============================================================================

/** Pure UI state rendered by [RemoveAccountDialog] and [RemoveAccountDialogContent]. */
@Immutable
data class RemoveAccountState(
    val accountId: String,
    val email: String,
    val displayName: String? = null,
)

/** Actions emitted by [RemoveAccountDialog] and [RemoveAccountDialogContent]. */
sealed interface RemoveAccountAction {
    data class Confirm(val accountId: String) : RemoveAccountAction
    data object Cancel : RemoveAccountAction
}

/** Modal confirmation dialog for removing an account from this phone only. */
@Composable
fun RemoveAccountDialog(
    state: RemoveAccountState,
    onAction: (RemoveAccountAction) -> Unit,
) {
    Dialog(onDismissRequest = { onAction(RemoveAccountAction.Cancel) }) {
        RemoveAccountDialogContent(
            state = state,
            onAction = onAction,
        )
    }
}

/** Pure Compose body of [RemoveAccountDialog] for direct storybook and Robolectric rendering. */
@Composable
fun RemoveAccountDialogContent(
    state: RemoveAccountState,
    onAction: (RemoveAccountAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    AmuxPanel(
        shape = RoundedCornerShape(AmuxTheme.metrics.floatRadius),
        modifier = modifier
            .fillMaxWidth()
            .testTag(AccountSheetTags.REMOVE_DIALOG),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(Space.xl),
            verticalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
                Text(
                    text = stringResource(R.string.account_sheet_remove_title, state.email),
                    style = AmuxTheme.typography.identifier,
                    color = colors.ink,
                    modifier = Modifier.testTag(AccountSheetTags.REMOVE_TITLE),
                )
                Text(
                    text = stringResource(R.string.account_sheet_remove_detail),
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                    modifier = Modifier.testTag(AccountSheetTags.REMOVE_DETAIL),
                )
            }
            Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                AmuxActionButton(
                    label = stringResource(R.string.account_sheet_remove_confirm),
                    kind = ActionButtonKind.Destructive,
                    fillWidth = true,
                    onClick = { onAction(RemoveAccountAction.Confirm(state.accountId)) },
                    modifier = Modifier.testTag(AccountSheetTags.REMOVE_CONFIRM),
                )
                AmuxActionButton(
                    label = stringResource(R.string.action_cancel),
                    kind = ActionButtonKind.Outline,
                    fillWidth = true,
                    onClick = { onAction(RemoveAccountAction.Cancel) },
                    modifier = Modifier.testTag(AccountSheetTags.REMOVE_CANCEL),
                )
            }
        }
    }
}

// =============================================================================
// Permanent account deletion sheet (typed email confirmation + 409 billing portal)
// =============================================================================

/** Current phase of permanent account deletion. */
@Immutable
sealed interface DeleteAccountPhaseUi {
    data object Asking : DeleteAccountPhaseUi
    data object Deleting : DeleteAccountPhaseUi
    data class Blocked(
        val providerPlace: String,
        val manageUrl: String,
    ) : DeleteAccountPhaseUi

    data class Failed(val reason: String) : DeleteAccountPhaseUi
}

/** Pure UI state rendered by [DeleteAccountSheet] and [DeleteAccountSheetContent]. */
@Immutable
data class DeleteAccountSheetState(
    val accountId: String,
    val email: String,
    val typedEmail: String = "",
    val billingWarning: String? = null,
    val phase: DeleteAccountPhaseUi = DeleteAccountPhaseUi.Asking,
) {
    val working: Boolean
        get() = phase == DeleteAccountPhaseUi.Deleting

    val blocked: Boolean
        get() = phase is DeleteAccountPhaseUi.Blocked

    /** True only when the typed address matches [email] case-insensitively after trimming. */
    val emailMatches: Boolean
        get() {
            val written = typedEmail.trim()
            return written.isNotEmpty() && written.equals(email.trim(), ignoreCase = true)
        }

    val canConfirmDelete: Boolean
        get() = emailMatches && !working && !blocked
}

/** Actions emitted by [DeleteAccountSheet] and [DeleteAccountSheetContent]. */
sealed interface DeleteAccountAction {
    data class TypeEmail(val text: String) : DeleteAccountAction
    data object ConfirmDelete : DeleteAccountAction
    data class OpenBillingPortal(val url: String) : DeleteAccountAction
    data object TryAgain : DeleteAccountAction
    data object Cancel : DeleteAccountAction
}

/**
 * M3 [ModalBottomSheet] for permanent account deletion with keyboard-safe
 * insets so the soft keyboard never obscures the Delete button.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DeleteAccountSheet(
    state: DeleteAccountSheetState,
    onAction: (DeleteAccountAction) -> Unit,
) {
    val colors = AmuxTheme.colors
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(
        onDismissRequest = { if (!state.working) onAction(DeleteAccountAction.Cancel) },
        sheetState = sheetState,
        containerColor = colors.ground,
        contentColor = colors.ink,
        contentWindowInsets = { WindowInsets.navigationBars.union(WindowInsets.ime) },
    ) {
        DeleteAccountSheetContent(
            state = state,
            onAction = onAction,
        )
    }
}

/**
 * Pure Compose body of the permanent account deletion sheet. Applies
 * `WindowInsets.navigationBars.union(WindowInsets.ime)` and vertical scrolling
 * so the Delete button remains reachable with the keyboard open.
 */
@Composable
fun DeleteAccountSheetContent(
    state: DeleteAccountSheetState,
    onAction: (DeleteAccountAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val closeLabel = stringResource(R.string.action_close)
    val emailDescription = stringResource(R.string.account_sheet_delete_email_description)

    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(colors.ground)
            .windowInsetsPadding(WindowInsets.navigationBars.union(WindowInsets.ime))
            .verticalScroll(rememberScrollState())
            .padding(bottom = Space.xl)
            .testTag(AccountSheetTags.DELETE_SHEET),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.s, vertical = Space.xs),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(
                onClick = { onAction(DeleteAccountAction.Cancel) },
                enabled = !state.working,
                modifier = Modifier
                    .thumbTarget()
                    .testTag(AccountSheetTags.DELETE_CLOSE),
            ) {
                Icon(
                    imageVector = Icons.Outlined.Close,
                    contentDescription = closeLabel,
                    tint = colors.inkMuted,
                )
            }
            Text(
                text = stringResource(R.string.account_sheet_delete_bar_title),
                style = AmuxTheme.typography.identifier,
                color = colors.ink,
                modifier = Modifier.weight(1f),
            )
        }
        AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))

        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin, vertical = Space.l),
            verticalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            Text(
                text = stringResource(R.string.account_sheet_delete_title),
                style = AmuxTheme.typography.screenTitle,
                color = colors.ink,
                modifier = Modifier.testTag(AccountSheetTags.DELETE_TITLE),
            )

            Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                ConsequenceRow(
                    kept = true,
                    text = stringResource(R.string.account_sheet_delete_kept),
                )
                ConsequenceRow(
                    kept = false,
                    text = stringResource(R.string.account_sheet_delete_lost),
                )
                ConsequenceRow(
                    kept = false,
                    text = stringResource(R.string.account_sheet_delete_permanent),
                )
                if (!state.billingWarning.isNullOrBlank()) {
                    ConsequenceRow(
                        kept = false,
                        text = state.billingWarning,
                    )
                }
            }

            when (val phase = state.phase) {
                is DeleteAccountPhaseUi.Blocked -> {
                    AmuxPanel(
                        borderColor = colors.accent.copy(alpha = 0.5f),
                        modifier = Modifier
                            .fillMaxWidth()
                            .testTag(AccountSheetTags.DELETE_BLOCKED),
                    ) {
                        Column(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(Space.l),
                            verticalArrangement = Arrangement.spacedBy(Space.xs),
                        ) {
                            Text(
                                text = stringResource(R.string.account_sheet_delete_blocked_title),
                                style = AmuxTheme.typography.bodyEmphasis,
                                color = colors.ink,
                            )
                            Text(
                                text = stringResource(
                                    R.string.account_sheet_delete_blocked_detail,
                                    phase.providerPlace,
                                ),
                                style = AmuxTheme.typography.detail,
                                color = colors.inkMuted,
                            )
                        }
                    }

                    Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                        AmuxActionButton(
                            label = stringResource(R.string.account_sheet_delete_billing_portal),
                            kind = ActionButtonKind.Primary,
                            fillWidth = true,
                            onClick = { onAction(DeleteAccountAction.OpenBillingPortal(phase.manageUrl)) },
                            modifier = Modifier.testTag(AccountSheetTags.DELETE_BILLING_PORTAL),
                        )
                        AmuxActionButton(
                            label = stringResource(R.string.account_sheet_delete_try_again),
                            kind = ActionButtonKind.Outline,
                            fillWidth = true,
                            onClick = { onAction(DeleteAccountAction.TryAgain) },
                            modifier = Modifier.testTag(AccountSheetTags.DELETE_TRY_AGAIN),
                        )
                        TextButton(
                            onClick = { onAction(DeleteAccountAction.Cancel) },
                            modifier = Modifier
                                .fillMaxWidth()
                                .thumbTarget()
                                .testTag(AccountSheetTags.DELETE_CANCEL),
                        ) {
                            Text(
                                text = stringResource(R.string.account_sheet_delete_not_now),
                                style = AmuxTheme.typography.bodyEmphasis,
                                color = colors.inkMuted,
                            )
                        }
                    }
                }

                else -> {
                    if (phase is DeleteAccountPhaseUi.Failed) {
                        AmuxPanel(
                            borderColor = colors.removed.copy(alpha = 0.55f),
                            modifier = Modifier
                                .fillMaxWidth()
                                .testTag(AccountSheetTags.DELETE_FAILED),
                        ) {
                            Column(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .padding(Space.l),
                                verticalArrangement = Arrangement.spacedBy(Space.xs),
                            ) {
                                Text(
                                    text = stringResource(R.string.account_sheet_delete_failed_title),
                                    style = AmuxTheme.typography.bodyEmphasis,
                                    color = colors.removed,
                                )
                                Text(
                                    text = phase.reason,
                                    style = AmuxTheme.typography.detail,
                                    color = colors.inkMuted,
                                )
                            }
                        }
                    }

                    Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                        Text(
                            text = stringResource(R.string.account_sheet_delete_email_label, state.email),
                            style = AmuxTheme.typography.caption,
                            color = colors.inkMuted,
                        )
                        OutlinedTextField(
                            value = state.typedEmail,
                            onValueChange = { onAction(DeleteAccountAction.TypeEmail(it)) },
                            enabled = !state.working,
                            singleLine = true,
                            textStyle = AmuxTheme.typography.mono.copy(color = colors.ink),
                            placeholder = {
                                Text(
                                    text = state.email,
                                    style = AmuxTheme.typography.mono,
                                    color = colors.inkFaint,
                                )
                            },
                            shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                            colors = OutlinedTextFieldDefaults.colors(
                                focusedBorderColor = colors.ink,
                                unfocusedBorderColor = colors.hairline,
                                disabledBorderColor = colors.hairline,
                                focusedContainerColor = colors.sunken,
                                unfocusedContainerColor = colors.sunken,
                                disabledContainerColor = colors.sunken,
                                cursorColor = colors.ink,
                            ),
                            keyboardOptions = KeyboardOptions(
                                capitalization = KeyboardCapitalization.None,
                                autoCorrectEnabled = false,
                                keyboardType = KeyboardType.Email,
                                imeAction = ImeAction.Done,
                            ),
                            keyboardActions = KeyboardActions(
                                onDone = {
                                    if (state.canConfirmDelete) {
                                        onAction(DeleteAccountAction.ConfirmDelete)
                                    }
                                },
                            ),
                            modifier = Modifier
                                .fillMaxWidth()
                                .semantics { contentDescription = emailDescription }
                                .testTag(AccountSheetTags.DELETE_EMAIL_INPUT),
                        )
                    }

                    Spacer(modifier = Modifier.height(Space.xs))

                    Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                        if (state.working) {
                            Row(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .thumbTarget()
                                    .testTag(AccountSheetTags.DELETE_WORKING),
                                verticalAlignment = Alignment.CenterVertically,
                                horizontalArrangement = Arrangement.Center,
                            ) {
                                AmuxStateMark(state = RowState.Working, size = 10.dp)
                                Spacer(modifier = Modifier.padding(horizontal = Space.xs))
                                Text(
                                    text = stringResource(R.string.account_sheet_delete_deleting),
                                    style = AmuxTheme.typography.bodyEmphasis,
                                    color = colors.inkMuted,
                                )
                            }
                        } else if (phase is DeleteAccountPhaseUi.Failed) {
                            AmuxActionButton(
                                label = stringResource(R.string.account_sheet_delete_try_again),
                                kind = ActionButtonKind.Destructive,
                                fillWidth = true,
                                enabled = state.emailMatches,
                                onClick = { onAction(DeleteAccountAction.TryAgain) },
                                modifier = Modifier.testTag(AccountSheetTags.DELETE_TRY_AGAIN),
                            )
                        } else {
                            AmuxActionButton(
                                label = stringResource(R.string.account_sheet_delete_confirm),
                                kind = ActionButtonKind.Destructive,
                                fillWidth = true,
                                enabled = state.canConfirmDelete,
                                onClick = { onAction(DeleteAccountAction.ConfirmDelete) },
                                modifier = Modifier.testTag(AccountSheetTags.DELETE_CONFIRM),
                            )
                        }
                        AmuxActionButton(
                            label = stringResource(R.string.action_cancel),
                            kind = ActionButtonKind.Outline,
                            fillWidth = true,
                            enabled = !state.working,
                            onClick = { onAction(DeleteAccountAction.Cancel) },
                            modifier = Modifier.testTag(AccountSheetTags.DELETE_CANCEL),
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun ConsequenceRow(
    kept: Boolean,
    text: String,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.Top,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Icon(
            imageVector = if (kept) Icons.Outlined.Check else Icons.Outlined.Close,
            contentDescription = null,
            tint = if (kept) colors.inkMuted else colors.removed,
            modifier = Modifier
                .padding(top = 2.dp)
                .size(16.dp),
        )
        Text(
            text = text,
            style = AmuxTheme.typography.detail,
            color = if (kept) colors.inkMuted else colors.ink,
            modifier = Modifier.weight(1f),
        )
    }
}
