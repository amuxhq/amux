package sh.amux.words

/** How a failure at the account service reads, in the iPhone app's words. */
object CloudWords {
    const val SIGN_IN_NOT_RECOGNISED: String = "amux.sh did not recognise this sign-in"
    const val ACCOUNT_NOT_RECOGNISED: String = "amux.sh no longer recognises this account"
    const val DID_NOT_ANSWER: String = "amux.sh did not answer"
}

/** Why the paywall cannot go on. */
object PaywallWords {
    const val NO_PLANS: String = "No subscription plans are available right now."
    const val NOTHING_TO_RESTORE: String = "There is nothing on this account to restore."
}
