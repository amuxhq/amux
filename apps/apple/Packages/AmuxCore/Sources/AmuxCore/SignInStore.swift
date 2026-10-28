import Foundation
import Observation

/// The one sign-in this phone has in flight, and how it went.
///
/// It is not part of an account's stores, because there is no account yet:
/// signing in is what makes one. It sits beside the registry a finished
/// sign-in is added to.
///
/// There is no screen of its own. A Sign In opens the system browser straight
/// from where it was pressed, and that control says the sign-in is under way;
/// what is left to say afterwards — a refusal, or somebody else coming back —
/// is asked over wherever the person still is.
@MainActor
@Observable
public final class SignInStore {
    public enum Phase: Sendable, Equatable {
        /// Nothing in flight and nothing to say: never attempted, finished,
        /// or abandoned in the browser.
        case ready
        /// The browser is up, or what came back is being kept. Everything
        /// until the browser closes happens on amux.sh.
        case handingOff
        /// It came back refused, in the cloud's own words.
        case failed(String)
        /// Asked to sign back into one account, and somebody else came back —
        /// the browser was signed in as another account, or the person chose
        /// one. Nothing is added until they say which they meant.
        case mismatched(wanted: SignedInAccount, got: SignedInAccount)
    }

    public private(set) var phase: Phase = .ready
    /// Which account the last sign-in was for. Kept after it ends, so trying
    /// again asks amux.sh for the same account.
    public private(set) var intent: SignInIntent

    public init(phase: Phase = .ready, intent: SignInIntent = .adding) {
        self.phase = phase
        self.intent = intent
    }

    public var working: Bool { phase == .handingOff }

    /// Which account the sign-in in flight is for, so the control that asked
    /// for that account is the one that says so. Nothing while none is.
    public var pending: SignInIntent? { working ? intent : nil }

    /// Keeps an account that signed in: binds its installation with the
    /// sign-in's refresh token and puts it on screen. Answers what stopped
    /// it, if anything.
    public typealias Keep = @MainActor (SignedInAccount) async -> CloudError?

    /// Signs in, and says what came back.
    ///
    /// Cancelling is not a failure and leaves nothing to dismiss: coming back
    /// from the browser without finishing is a person changing their mind,
    /// and an alert over it would be the app arguing. Every other refusal is
    /// said, because there is nothing the person can do about it until they
    /// know what it was. A second press while one is in flight starts
    /// nothing: two browsers would be two attempts for one account.
    @discardableResult
    public func signIn(
        _ intent: SignInIntent,
        with cloud: any CloudService,
        presenting: any WebAuthPresenter,
        keeping keep: Keep
    ) async -> SignedInAccount? {
        guard !working else { return nil }
        self.intent = intent
        phase = .handingOff
        do {
            let account = try await cloud.signIn(intent, presenting: presenting)
            if case .returning(let wanted) = intent, wanted.id != account.id {
                phase = .mismatched(wanted: wanted, got: account)
                return nil
            }
            return await kept(account, keep)
        } catch {
            phase = Self.phase(after: error)
            return nil
        }
    }

    static func phase(after error: CloudError) -> Phase {
        switch error {
        case .cancelled: .ready
        case .unauthenticated: .failed("amux.sh did not recognise this sign-in")
        case .refused(let reason): .failed(reason)
        case .network(let what): .failed(what)
        case .timeout: .failed("amux.sh did not answer")
        }
    }

    /// Puts a refusal away once it has been read.
    public func dismiss() {
        guard case .failed = phase else { return }
        phase = .ready
    }

    /// Keeps the account that came back instead of the one asked for.
    @discardableResult
    public func keep(keeping keep: Keep) async -> SignedInAccount? {
        guard case .mismatched(_, let got) = phase else { return nil }
        phase = .handingOff
        return await kept(got, keep)
    }

    /// Turns away the account that signed in instead of the one asked for,
    /// letting go of everything its sign-in obtained.
    public func discard(with cloud: any CloudService) async {
        guard case .mismatched(_, let got) = phase else { return }
        phase = .ready
        try? await cloud.forgetSession(got.id)
    }

    private func kept(_ account: SignedInAccount, _ keep: Keep) async -> SignedInAccount? {
        if let refused = await keep(account) {
            phase = Self.phase(after: refused)
            return nil
        }
        phase = .ready
        return account
    }
}
