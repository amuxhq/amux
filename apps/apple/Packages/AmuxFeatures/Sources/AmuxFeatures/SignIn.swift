import AmuxCore
import AmuxDesign
import SwiftUI

/// What somebody did on the sign-in screen.
public enum SignInAction: Equatable, Sendable {
    /// Left without signing in.
    case cancel
    /// Hand off to the account service.
    case start
    /// Signed in, and finished reading about it.
    case done
    /// Somebody other than the account asked for came back, and the person
    /// wants that account on the phone anyway.
    case keep
    /// Somebody other than the account asked for came back, and the person
    /// does not want it.
    case discard
}

/// Signing in, which happens somewhere else.
///
/// The screen owns almost nothing, and that is the design: there is no field
/// here, no password, no address, no "forgot" link, because none of that is
/// this app's to hold. What it does own is the one thing a hand-off must get
/// right — naming where it is sending you. A password is about to be typed,
/// and a hand-off that does not say where it points is indistinguishable from
/// one that is lying, so the host is on the button, in the caption under it,
/// and in the sentence the screen leads with.
public struct SignIn: View {
    @Environment(\.design) private var design
    private let model: SignInStore
    private let actions: @MainActor (SignInAction) -> Void

    public init(model: SignInStore, actions: @escaping @MainActor (SignInAction) -> Void) {
        self.model = model
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                heading
                Spacer(minLength: 22)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .padding(.horizontal, design.metrics.gutter)
            foot
        }
        .backBar("sign-in.back") { actions(.cancel) }
        // A screen is a container of the things on it, not a name for all of
        // them. Without this the system spreads this identifier over every
        // element underneath, and everything on the screen answers to the
        // screen's own name — for VoiceOver and for anything driving the app.
        .accessibilityElement(children: .contain)
        .identified("sign-in", value: state)
    }

    private var state: String {
        switch model.phase {
        case .ready: "ready"
        case .handingOff: "handing off"
        case .failed: "failed"
        case .signedIn(let account): account.email
        case .mismatched: "mismatched"
        }
    }

    private var heading: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Sign in")
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .identified("sign-in.title", value: "Sign in")
            Text(explanation)
                .designFont(.body, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
                .identified("sign-in.explanation", value: explanation)
        }
        .padding(.top, 8)
        .padding(.bottom, 26)
    }

    /// What an account is for, and that a subscription bought on the web is
    /// not bought again here, in one sentence naming where the signing-in
    /// happens.
    ///
    /// Signing back into an account this phone already lists says which, so
    /// the page is about that account rather than about accounts in general.
    private var explanation: String {
        if case .returning(let account) = model.intent {
            return String(localized: "Sign back in as \(account.email).")
        }
        return String(localized: "An account lets this phone reach your hosts from anywhere, and a subscription bought on \(model.host) carries over.")
    }

    /// The hand-off, and everything said about it.
    ///
    /// A refusal is put here rather than at the top of the screen because this
    /// is where the person's hand already is: what went wrong and the button
    /// that tries again are one block, and neither has to be hunted for.
    private var foot: some View {
        VStack {
            Spacer(minLength: 0)
            BottomAction {
                VStack(spacing: 10) {
                    trouble
                    if model.working {
                        WaitingLine(title: title)
                            .identified("sign-in.continue", label: title, value: state, enabled: false)
                    } else {
                        Button { actions(pressed) } label: {
                            ActionLabel(title, kind: .primary, fill: true)
                        }
                        .buttonStyle(.amuxControl)
                        .identified("sign-in.continue", label: title, value: state)
                    }
                    if case .mismatched = model.phase {
                        Button { actions(.discard) } label: {
                            ActionLabel("Cancel", kind: .quiet, fill: true)
                        }
                        .buttonStyle(.amuxControl)
                        .identified("sign-in.discard", label: "Cancel")
                    }
                    caption
                }
            }
        }
    }

    private var title: String {
        switch model.phase {
        case .ready: "Continue on \(model.host)"
        case .handingOff: "Opening \(model.host)…"
        case .failed: "Try Again"
        case .signedIn: "Done"
        case .mismatched(_, let got): "Continue as \(got.email)"
        }
    }

    /// What the one primary press does in the phase on screen.
    private var pressed: SignInAction {
        switch model.phase {
        case .signedIn: .done
        case .mismatched: .keep
        case .ready, .handingOff, .failed: .start
        }
    }

    /// What came back, when it was not an account.
    @ViewBuilder
    private var trouble: some View {
        if case .failed(let reason) = model.phase {
            VStack(alignment: .leading, spacing: 4) {
                Text("Sign-in did not finish")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                // The cloud's own words. This app does not know what went
                // wrong on amux.sh and inventing a friendlier sentence would
                // be guessing on somebody else's behalf.
                Explain(reason)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
            .identified("sign-in.failed", value: reason)
        }
        // Said before either button: which account came back, and which one
        // this page was for, so neither press is a guess.
        if case .mismatched(let wanted, let got) = model.phase {
            VStack(alignment: .leading, spacing: 4) {
                Text("Signed in as another account")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                Explain("You signed in as \(got.email), not \(wanted.email).")
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
            .identified("sign-in.mismatch", value: got.email)
        }
    }

    /// Under the button: where it opens, and that the browser is the system's
    /// rather than a window this app draws.
    @ViewBuilder
    private var caption: some View {
        if case .signedIn(let account) = model.phase {
            Text("Signed in as \(account.email)")
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
                .identified("sign-in.signed-in", value: account.email)
        } else {
            HStack(spacing: 5) {
                Image(systemName: "lock.fill")
                    .font(.system(size: 11, weight: .semibold))
                Text("Opens \(model.host) in Safari")
                    .designFont(.detail, design)
            }
            .foregroundStyle(design.inkFaint.color)
            .accessibilityElement(children: .combine)
            .identified("sign-in.opens", value: model.host)
        }
    }
}

extension SignInStore {
    /// Whether the one thing left to do is leave.
    var finished: Bool {
        if case .signedIn = phase { return true }
        return false
    }
}
