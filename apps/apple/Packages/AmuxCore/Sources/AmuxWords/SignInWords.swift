import AmuxValues

extension ChatWords {
    /// The sign-in notice's last line. Nothing in amux can sign the provider
    /// in, so the composer under the notice still sends: sending is how
    /// someone who signed in elsewhere tries again.
    public static let signInRetry = String(localized: "Signed in elsewhere? Send a message to try again.")

    /// The sign-in notice's lines under its title: the account and what went
    /// wrong, how to sign in on the agent's host, and that sending retries.
    public static func signInDetail(_ view: SignInView, host: String) -> String {
        [signIn(view), view.message, signInSteps(view.hint, host: host), signInRetry]
            .filter { !$0.isEmpty }
            .joined(separator: "\n")
    }
}
