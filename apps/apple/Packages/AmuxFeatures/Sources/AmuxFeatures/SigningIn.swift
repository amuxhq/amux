import AmuxCore
import AmuxDesign
import SwiftUI

/// What a Sign In shows in its own place while its sign-in is under way: the
/// breathing mark and the words, in ink, the way a pushed page's button
/// becomes a ``WaitingLine``. The browser covers the app while it is up, so
/// this is mostly seen after it closes, while the account that came back is
/// kept — a moment of network the press should not look dropped through.
struct SigningIn: View {
    @Environment(\.design) private var design
    let font: Design.Role

    var body: some View {
        HStack(spacing: 8) {
            Self.mark
            Self.words(font, design)
        }
        .accessibilityElement(children: .combine)
    }

    static let title = String(localized: "Signing in…")

    static var mark: some View { StateMark(state: .working).accessibilityHidden(true) }

    static func words(_ font: Design.Role, _ design: Design) -> some View {
        Text(title)
            .designFont(font, design)
            .foregroundStyle(design.ink.color)
    }
}

extension SignInIntent {
    /// Whether this is the sign-in back into one account this phone lists.
    func isReturning(to id: AccountId) -> Bool {
        if case .returning(let account) = self { return account.id == id }
        return false
    }
}

/// What somebody answered about a sign-in that came back with something to
/// say.
public enum SignInAnswer: Equatable, Sendable {
    /// Sign in again, for the account the refused attempt asked for.
    case retry
    /// Read the refusal and put it away.
    case dismiss
    /// Somebody other than the account asked for came back, and the person
    /// wants that account on the phone anyway.
    case keep
    /// The same, and the person does not.
    case discard
}

extension View {
    /// Asks what a finished sign-in leaves to ask, in the system's own alert
    /// and dialog, over whatever the person was looking at when they pressed
    /// Sign In.
    ///
    /// Only a refusal and a different account are asked about. A sign-in that
    /// was kept says so by the account appearing where the person is, and one
    /// abandoned in the browser says nothing at all.
    ///
    /// `shown` is whether this view is the one on top: an alert raised from a
    /// view a sheet covers never appears, so each surface that can be on top
    /// carries this and only the topmost one asks.
    ///
    /// The bindings never answer for the person. The system reports a dialog
    /// put away before it reports the press that put it away, and a dialog
    /// whose dismissal turned the other account down would let go of its
    /// session before Continue could keep it. Every way out of either one is
    /// a button: the system presses the cancelling one when the dialog is
    /// tapped away.
    public func signInQuestions(
        _ model: SignInStore, shown: Bool, answer: @escaping @MainActor (SignInAnswer) -> Void
    ) -> some View {
        modifier(SignInQuestions(model: model, shown: shown, answer: answer))
    }
}

private struct SignInQuestions: ViewModifier {
    let model: SignInStore
    let shown: Bool
    let answer: @MainActor (SignInAnswer) -> Void

    private var refusal: String? {
        if case .failed(let reason) = model.phase { return reason }
        return nil
    }

    private var mismatch: (wanted: SignedInAccount, got: SignedInAccount)? {
        if case .mismatched(let wanted, let got) = model.phase { return (wanted, got) }
        return nil
    }

    func body(content: Content) -> some View {
        content
            .alert(
                String(localized: "Sign-in did not finish"),
                isPresented: Binding(get: { shown && refusal != nil }, set: { _ in })
            ) {
                Button(String(localized: "Try Again")) { answer(.retry) }
                    .accessibilityIdentifier("sign-in.retry")
                Button(String(localized: "Cancel"), role: .cancel) { answer(.dismiss) }
                    .accessibilityIdentifier("sign-in.dismiss")
            } message: {
                // The cloud's own words. This app does not know what went
                // wrong on amux.sh, and a friendlier sentence would be a
                // guess made on somebody else's behalf.
                Text(refusal ?? "")
            }
            .confirmationDialog(
                String(localized: "Signed in as another account"),
                isPresented: Binding(get: { shown && mismatch != nil }, set: { _ in }),
                titleVisibility: .visible,
                presenting: mismatch
            ) { mismatch in
                Button(String(localized: "Continue as \(mismatch.got.email)")) { answer(.keep) }
                    .accessibilityIdentifier("sign-in.keep")
                Button(String(localized: "Cancel"), role: .cancel) { answer(.discard) }
                    .accessibilityIdentifier("sign-in.discard")
            } message: { mismatch in
                // Which account came back and which one was asked for, so
                // neither press is a guess.
                Text("You signed in as \(mismatch.got.email), not \(mismatch.wanted.email).")
            }
    }
}
