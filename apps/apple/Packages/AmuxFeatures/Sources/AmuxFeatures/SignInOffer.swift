import AmuxCore
import AmuxDesign
import SwiftUI

/// The person's answer to an account offered across a pairing.
public enum SignInOfferAction: Equatable, Sendable {
    case accept
    case decline
}

/// An account offered across a pairing, either way round, with the
/// account's address in front of the person deciding.
///
/// Pairing already decided which machines this phone trusts; an account
/// only adds the relay as a route between them. So the question is short
/// and names whose account it is: a machine paired for some narrower reason
/// that offers the wrong account is turned down here.
public struct SignInOfferPanel: View {
    private let prompt: SignInPrompt
    /// The answer or the offer is with the runtime.
    private let working: Bool
    private let failure: SignInOffers.Failure?
    private let actions: @MainActor (SignInOfferAction) -> Void

    public init(
        prompt: SignInPrompt, working: Bool = false, failure: SignInOffers.Failure? = nil,
        actions: @escaping @MainActor (SignInOfferAction) -> Void
    ) {
        self.prompt = prompt
        self.working = working
        self.failure = failure
        self.actions = actions
    }

    public var body: some View {
        PairingPanel(title: title, detail: detail, identifier: "sign-in-offer.title") {
            if let failure {
                Explain(said(failure))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .identified("sign-in-offer.failure", value: said(failure))
            }
            if working {
                WaitingLine(title: SigningIn.title)
                    .identified(
                        "sign-in-offer.accept", label: SigningIn.title, value: "signing in",
                        enabled: false)
            } else {
                Button { actions(.accept) } label: {
                    ActionLabel("Sign In", kind: .primary, fill: true)
                }
                .buttonStyle(.amuxControl)
                .identified("sign-in-offer.accept", label: "Sign In")
            }
            Button { actions(.decline) } label: {
                ActionLabel("Not Now", kind: .outline, fill: true)
            }
            .buttonStyle(.amuxControl)
            .disabled(working)
            .identified("sign-in-offer.decline", label: "Not Now", enabled: !working)
        }
        .navigationTitle(String(localized: "Sign In"))
        .toolbarTitleDisplayMode(.inline)
        .accessibilityElement(children: .contain)
        .identified("sign-in-offer", value: prompt.role == .receiver ? "offered" : "offering")
    }

    private var title: String {
        switch prompt {
        case .offered(let offer): String(localized: "Sign in as \(offer.email)?")
        case .offering(let host, let email): String(localized: "Sign in \(host.name) as \(email) too?")
        }
    }

    private var detail: String {
        switch prompt {
        case .offered(let offer):
            String(localized: "\(offer.hostName) offers its account, so this phone reaches it from anywhere.")
        case .offering(let host, _):
            String(localized: "Then this phone reaches \(host.name) from anywhere.")
        }
    }

    /// What went wrong, as what to do next.
    private func said(_ failure: SignInOffers.Failure) -> String {
        switch failure {
        case .unreachable(let name) where !name.isEmpty:
            String(localized: "\(name) is offline. Open amux on \(name), then try again.")
        case .unreachable:
            String(localized: "That host is offline. Open amux on it, then try again.")
        case .refused:
            String(localized: "Signing in didn’t work. Sign in from Settings instead.")
        }
    }
}
