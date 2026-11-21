import AmuxValues
import Foundation

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

    /// Words an agent wrote with its commands between backticks, as
    /// OpenCode's sign-in hint does ("Run `opencode auth login` in the
    /// terminal"): each such span drawn as code, without its backticks,
    /// the rest as written. A lone backtick is kept, as it is not a span.
    public static func withCode(_ text: String) -> AttributedString {
        let parts = text.components(separatedBy: "`")
        guard parts.count >= 3 else { return AttributedString(text) }
        var out = AttributedString()
        for (index, part) in parts.enumerated() {
            let unclosed = index == parts.count - 1 && index % 2 == 1
            if unclosed {
                out += AttributedString("`" + part)
            } else if index % 2 == 1 {
                var code = AttributedString(part)
                code.inlinePresentationIntent = .code
                out += code
            } else {
                out += AttributedString(part)
            }
        }
        return out
    }

    /// The same words as a reader hears them: the spans' backticks gone.
    public static func withoutCodeMarks(_ text: String) -> String {
        String(withCode(text).characters)
    }
}
