package sh.amux.shell

import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.AccountRegistryState
import sh.amux.core.runtime.DeletionState
import sh.amux.core.runtime.DeletionStore
import sh.amux.core.runtime.Entitlement
import sh.amux.core.runtime.SignInOffers
import sh.amux.core.runtime.SignInOffersState
import sh.amux.core.runtime.SignInPrompt
import sh.amux.core.runtime.SignInStore
import sh.amux.features.DeleteAccountPhaseUi
import sh.amux.features.DeleteAccountSheetState
import sh.amux.features.RemoveAccountState
import sh.amux.features.SignInOfferFailureUi
import sh.amux.features.SignInOfferPromptUi
import sh.amux.features.SignInOfferSheetState
import sh.amux.features.SignInResultUi
import sh.amux.words.name

/** Pure mappers from sign-in, removal and deletion state to the account sheets and dialogs. */
object AccountMappers {
    fun toSignInOfferSheetState(
        prompt: SignInPrompt,
        offers: SignInOffersState,
    ): SignInOfferSheetState {
        val uiPrompt = when (prompt) {
            is SignInPrompt.Offered -> SignInOfferPromptUi.Offered(
                hostName = prompt.offer.hostName,
                email = prompt.offer.email,
            )
            is SignInPrompt.Offering -> SignInOfferPromptUi.Offering(
                hostName = prompt.host.name,
                email = prompt.email,
            )
        }
        val uiFailure = when (val failure = offers.failure) {
            is SignInOffers.Failure.Unreachable -> SignInOfferFailureUi.Unreachable(failure.hostName)
            is SignInOffers.Failure.Refused -> SignInOfferFailureUi.Refused(failure.reason)
            null -> null
        }
        return SignInOfferSheetState(
            prompt = uiPrompt,
            working = offers.answering != null,
            failure = uiFailure,
        )
    }

    fun toSignInResultUi(phase: SignInStore.Phase): SignInResultUi? = when (phase) {
        is SignInStore.Phase.Mismatched -> SignInResultUi.Mismatched(
            expectedEmail = phase.wanted.email,
            signedInEmail = phase.got.email,
        )
        is SignInStore.Phase.Failed -> SignInResultUi.Failed(reason = phase.reason)
        SignInStore.Phase.Ready, SignInStore.Phase.HandingOff -> null
    }

    fun toRemoveAccountState(
        accountId: AccountId?,
        registry: AccountRegistryState,
    ): RemoveAccountState? {
        val id = accountId ?: return null
        val entry = registry.accounts.firstOrNull { it.id == id }
        return RemoveAccountState(
            accountId = id.value,
            email = entry?.account?.email ?: id.value,
            displayName = entry?.account?.displayName,
        )
    }

    fun toDeleteAccountSheetState(
        deletion: DeletionState,
        registry: AccountRegistryState,
        formatBillingWarning: (String) -> String = { date ->
            "Your subscription renews on $date. Cancel renewal before deleting this account."
        },
    ): DeleteAccountSheetState? {
        val id = deletion.account ?: return null
        val phase: DeleteAccountPhaseUi = when (val raw = deletion.phase) {
            DeletionStore.Phase.Asking -> DeleteAccountPhaseUi.Asking
            DeletionStore.Phase.Working -> DeleteAccountPhaseUi.Deleting
            is DeletionStore.Phase.Blocked -> DeleteAccountPhaseUi.Blocked(
                providerPlace = raw.source.place,
                manageUrl = raw.manageUrl,
            )
            is DeletionStore.Phase.Failed -> DeleteAccountPhaseUi.Failed(raw.reason)
            DeletionStore.Phase.Deleted -> return null
        }
        val entry = registry.accounts.firstOrNull { it.id == id }
        val billingWarning = (entry?.entitlement as? Entitlement.Active)?.renewsAtMs?.let { ms ->
            formatBillingWarning(SettingsMappers.formatLocalizedDate(ms))
        }
        return DeleteAccountSheetState(
            accountId = id.value,
            email = entry?.account?.email ?: id.value,
            typedEmail = deletion.typed,
            billingWarning = billingWarning,
            phase = phase,
        )
    }

}
