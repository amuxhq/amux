package sh.amux.words

import sh.amux.values.SignInView

/**
 * The sign-in notice's last line. Nothing in amux can sign the provider in,
 * so the composer under the notice still sends: sending is how someone who
 * signed in elsewhere tries again.
 */
const val SIGN_IN_RETRY: String = "Signed in elsewhere? Send a message to try again."

val ChatWords.signInRetry: String
    get() = SIGN_IN_RETRY

/**
 * The sign-in notice's lines under its title: the account and what went wrong,
 * how to sign in on the agent's host, and that sending retries.
 */
fun ChatWords.signInDetail(view: SignInView, host: String): String =
    listOf(signIn(view), view.message, signInSteps(view.hint, host = host), signInRetry)
        .filter { it.isNotEmpty() }
        .joinToString("\n")
