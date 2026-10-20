import Foundation
import Observation

/// The one account this phone is in the middle of giving up, and how far that
/// has got.
///
/// It is not one of an account's own stores: what is being deleted is the
/// account those stores belong to, and they are taken down the moment it goes.
/// It sits beside the registry instead, holding who was asked about, what has
/// been typed to confirm it, and what the account service answered.
@MainActor
@Observable
public final class DeletionStore {
    public enum Phase: Sendable, Equatable {
        /// The question is on screen and nothing has been sent.
        case asking
        case working
        /// The account service will not delete while money is still moving.
        /// It names where the billing is, which is the only place it can be
        /// stopped — for an App Store subscription, not a page amux owns.
        case blocked(source: EntitlementSource, manageURL: URL)
        case deleted
        case failed(String)
    }

    /// Who is being asked about, or nothing when nobody is.
    public private(set) var account: AccountId?
    /// What has been typed into the confirmation field. Held here rather than
    /// in the card so that leaving the app to cancel a renewal and coming back
    /// does not make somebody type their address a second time.
    public var typed: String = ""
    public private(set) var phase: Phase = .asking

    public init(asking account: AccountId? = nil, typed: String = "", phase: Phase = .asking) {
        self.account = account
        self.typed = typed
        self.phase = phase
    }

    /// Opens the question about one account, with nothing typed yet.
    public func ask(_ id: AccountId) {
        account = id
        typed = ""
        phase = .asking
    }

    /// Asks the same question again after a refusal, keeping what was typed:
    /// the person has gone to stop a renewal and will come back to delete.
    public func askAgain() {
        guard account != nil, !working else { return }
        phase = .asking
    }

    /// Puts the question away. Deleting is the one thing on this page that
    /// cannot be undone, so changing your mind costs nothing and leaves
    /// nothing behind.
    public func dismiss() {
        account = nil
        typed = ""
        phase = .asking
    }

    public var working: Bool { phase == .working }

    /// Whether what has been typed is this account's own address.
    ///
    /// Case is ignored and the ends are trimmed, because an address typed on a
    /// phone arrives capitalised and with a space after it, and refusing that
    /// would be refusing the right answer.
    public func confirms(_ email: String) -> Bool {
        let written = typed.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !written.isEmpty else { return false }
        return written.compare(email, options: .caseInsensitive) == .orderedSame
    }

    /// Deletes the account, and says what came back.
    ///
    /// The registry only forgets the account once the account service says it
    /// is gone. A deletion the billing system refuses leaves everything as it
    /// was — the account still works, the subscription still renews — and the
    /// screen says where to go and stop it.
    @discardableResult
    public func delete(
        with cloud: any CloudService, forgetting forget: (@MainActor (AccountId) -> Void)? = nil
    ) async -> DeletionOutcome? {
        guard let id = account, !working else { return nil }
        phase = .working
        do {
            let outcome = try await cloud.requestDeletion(id, confirmedEmail: typed)
            switch outcome {
            case .deleted:
                phase = .deleted
                forget?(id)
                account = nil
                typed = ""
            case .blockedByRenewal(let source, let manageURL):
                phase = .blocked(source: source, manageURL: manageURL)
            }
            return outcome
        } catch {
            phase = Self.phase(after: error)
            return nil
        }
    }

    static func phase(after error: CloudError) -> Phase {
        switch error {
        case .cancelled: .asking
        case .unauthenticated: .failed("amux.sh no longer recognises this account")
        case .refused(let reason): .failed(reason)
        case .network(let what): .failed(what)
        case .timeout: .failed("amux.sh did not answer")
        }
    }
}
