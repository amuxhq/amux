import AmuxCore
import AuthenticationServices
import Foundation
import UIKit

/// The browser a sign-in happens in.
///
/// It is the system's, not this app's: `ASWebAuthenticationSession` draws the
/// address it opened above the page and this app cannot read what is typed
/// into it or what cookies it holds. Both halves matter — the person can see
/// they are on amux.sh and not on a page this app painted, and this app never
/// sees the password. A plain web view inside the app would look the same and
/// be neither.
public struct WebSignIn: WebAuthPresenter {
    public init() {}

    public func present(_ url: URL, callbackScheme: String) async throws(CloudError) -> URL {
        let outcome: Result<URL, CloudError> = await withCheckedContinuation { continuation in
            Task { @MainActor in
                // The window the browser is presented over. Looked up before
                // the session is made rather than asked for by it, so the one
                // moment there is nothing to present over — an app with no
                // window on screen — is a refusal a caller can read instead of
                // a window invented to satisfy a type.
                guard let anchor = Browser.frontWindow() else {
                    continuation.resume(returning: .failure(
                        .network("the app has no window to open a browser over")))
                    return
                }
                let browser = Browser(anchor: anchor)
                Browser.live = browser
                browser.open(url, callbackScheme: callbackScheme) { result in
                    continuation.resume(returning: result)
                }
            }
        }
        switch outcome {
        case .success(let callback): return callback
        case .failure(let error): throw error
        }
    }
}

/// One browser, kept alive while it is up.
///
/// `ASWebAuthenticationSession` is released the moment nothing refers to it,
/// and a released session takes its browser down with it — so a sign-in
/// started from a local variable closes itself before anybody can type.
@MainActor
private final class Browser: NSObject, ASWebAuthenticationPresentationContextProviding {
    /// The browser that is up, if one is. One at a time: a second sign-in
    /// while the first is on screen would be two attempts for one account.
    static var live: Browser?

    private let anchor: ASPresentationAnchor
    private var session: ASWebAuthenticationSession?

    init(anchor: ASPresentationAnchor) {
        self.anchor = anchor
    }

    func open(
        _ url: URL, callbackScheme: String,
        answering: @escaping @Sendable (Result<URL, CloudError>) -> Void
    ) {
        let session = ASWebAuthenticationSession(
            url: url, callbackURLScheme: callbackScheme
        ) { callback, error in
            Task { @MainActor in Browser.live = nil }
            if let callback {
                answering(.success(callback))
                return
            }
            // Closing the browser is not a failure. It is somebody deciding
            // not to sign in, and it is the one outcome this app must not
            // dress up as something having gone wrong.
            let cancelled = (error as? ASWebAuthenticationSessionError)?.code == .canceledLogin
            answering(.failure(cancelled
                ? .cancelled
                : .network(error?.localizedDescription ?? "the browser could not open")))
        }
        session.presentationContextProvider = self
        // The browser keeps whatever amux.sh already knows, and that is what
        // makes its account chooser work: every sign-in asks amux.sh to list
        // the accounts this browser has used and offer another, and picking
        // the one it is already signed in as needs no password. An ephemeral
        // session would remember nothing, so the chooser would be empty and
        // every sign-in a password.
        session.prefersEphemeralWebBrowserSession = false
        self.session = session
        guard session.start() else {
            self.session = nil
            Browser.live = nil
            answering(.failure(.network("no browser could be opened")))
            return
        }
    }

    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        anchor
    }

    static func frontWindow() -> UIWindow? {
        let windows = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
        return windows.first { $0.isKeyWindow } ?? windows.first
    }
}
