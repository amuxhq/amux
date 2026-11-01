import Foundation
import XCTest
@testable import AmuxCore

/// A cloud that answers a deletion one way, so each ending — gone, refused
/// while money is still moving, and refused outright — is reachable without a
/// network.
private struct OneDeletion: CloudService, @unchecked Sendable {
    var answer: Result<DeletionOutcome, CloudError>
    /// What the last request carried, so a test can prove the address that was
    /// typed is the address that left.
    let asked = Asked()

    final class Asked: @unchecked Sendable {
        private let lock = NSLock()
        private var value: (AccountId, String)?
        var last: (AccountId, String)? {
            get { lock.withLock { value } }
            set { lock.withLock { value = newValue } }
        }
    }

    func handOver(_ id: AccountId) async -> Handover? { nil }
    func lend(from lender: @escaping @Sendable (AccountId) async -> String?) async {}
    func forgetSession(_ id: AccountId) async throws {}
    func signIn(_ intent: SignInIntent, presenting: any WebAuthPresenter) async throws(CloudError) -> SignedInAccount {
        throw .unauthenticated
    }

    func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement { .none }

    func recordPurchase(_ id: AccountId, signedTransaction: String) async throws(CloudError) {
        throw .unauthenticated
    }

    func requestDeletion(
        _ id: AccountId, confirmedEmail: String
    ) async throws(CloudError) -> DeletionOutcome {
        asked.last = (id, confirmedEmail)
        switch answer {
        case .success(let outcome): return outcome
        case .failure(let error): throw error
        }
    }

}

@MainActor
final class DeletionStoreTests: XCTestCase {
    private let ada = SignedInAccount(id: AccountId("ada"), email: "ada@example.com")
    private let portal = URL(string: "https://billing.test/session/1")!

    /// The accounts taken off the phone, in order.
    private final class Forgotten {
        var accounts: [AccountId] = []
    }

    func testNothingConfirmsUntilTheAccountsOwnAddressIsTyped() {
        let store = DeletionStore(asking: ada.id)

        XCTAssertFalse(store.confirms(ada.email))

        store.typed = "bo@example.com"
        XCTAssertFalse(store.confirms(ada.email))

        store.typed = ada.email
        XCTAssertTrue(store.confirms(ada.email))
    }

    /// An address typed on a phone arrives capitalised and with a space after
    /// it. Refusing that would be refusing the right answer.
    func testTheTypedAddressIsReadPastCapitalsAndSpaces() {
        let store = DeletionStore(asking: ada.id)

        store.typed = "  Ada@Example.com "

        XCTAssertTrue(store.confirms(ada.email))
    }

    func testADeletionThatGoesThroughTakesTheAccountOffThePhone() async {
        let store = DeletionStore(asking: ada.id, typed: ada.email)
        let forgotten = Forgotten()
        let cloud = OneDeletion(answer: .success(.deleted))

        let outcome = await store.delete(with: cloud, forgetting: { forgotten.accounts.append($0) })

        XCTAssertEqual(outcome, .deleted)
        XCTAssertEqual(store.phase, .deleted)
        XCTAssertEqual(forgotten.accounts, [ada.id])
        XCTAssertEqual(cloud.asked.last?.0, ada.id)
        XCTAssertEqual(cloud.asked.last?.1, ada.email)
        // The question is over, and nothing is left holding an address.
        XCTAssertNil(store.account)
        XCTAssertEqual(store.typed, "")
    }

    /// Money still moving is not a failure and is not the end of the attempt:
    /// the account is untouched, the store says where the billing is so the
    /// screen can send somebody there, and what was typed survives the trip.
    func testADeletionBlockedByRenewalKeepsTheAccountAndNamesWhereBillingIs() async {
        let store = DeletionStore(asking: ada.id, typed: ada.email)
        let forgotten = Forgotten()
        let cloud = OneDeletion(
            answer: .success(.blockedByRenewal(source: .web, manageURL: portal)))

        let outcome = await store.delete(with: cloud, forgetting: { forgotten.accounts.append($0) })

        XCTAssertEqual(outcome, .blockedByRenewal(source: .web, manageURL: portal))
        XCTAssertEqual(store.phase, .blocked(source: .web, manageURL: portal))
        XCTAssertEqual(forgotten.accounts, [])
        XCTAssertEqual(store.account, ada.id)
        XCTAssertEqual(store.typed, ada.email)
    }

    /// Coming back from the billing source and pressing Delete again is one
    /// more request, not a fresh screen: the same account, the same address.
    func testDeletingAgainAfterCancellingARenewalGoesThrough() async {
        let store = DeletionStore(
            asking: ada.id, typed: ada.email,
            phase: .blocked(source: .web, manageURL: portal))
        let forgotten = Forgotten()
        let cloud = OneDeletion(answer: .success(.deleted))

        await store.delete(with: cloud, forgetting: { forgotten.accounts.append($0) })

        XCTAssertEqual(store.phase, .deleted)
        XCTAssertEqual(forgotten.accounts, [ada.id])
    }

    /// Leaving to cancel the renewal asks the question again with the address
    /// still typed, so Delete is one press away on the way back.
    func testLeavingToCancelARenewalAsksAgainWithTheAddressKept() {
        let store = DeletionStore(
            asking: ada.id, typed: ada.email,
            phase: .blocked(source: .appStore, manageURL: portal))

        store.askAgain()

        XCTAssertEqual(store.phase, .asking)
        XCTAssertEqual(store.account, ada.id)
        XCTAssertEqual(store.typed, ada.email)
    }

    func testARefusalIsSaidInTheAccountServicesOwnWords() async {
        let store = DeletionStore(asking: ada.id, typed: "bo@example.com")
        let forgotten = Forgotten()
        let cloud = OneDeletion(
            answer: .failure(.refused("that is not this account's address")))

        await store.delete(with: cloud, forgetting: { forgotten.accounts.append($0) })

        XCTAssertEqual(store.phase, .failed("that is not this account's address"))
        XCTAssertEqual(forgotten.accounts, [])
    }

    func testChangingYourMindLeavesNothingBehind() {
        let store = DeletionStore(
            asking: ada.id, typed: ada.email, phase: .failed("something went wrong"))

        store.dismiss()

        XCTAssertNil(store.account)
        XCTAssertEqual(store.typed, "")
        XCTAssertEqual(store.phase, .asking)
    }

    /// Asking about a second account is a new question, not the last one
    /// carried over: what was typed for the first account must not confirm the
    /// second.
    func testAskingAboutAnotherAccountStartsWithNothingTyped() {
        let store = DeletionStore(asking: ada.id, typed: ada.email)

        store.ask(AccountId("acme"))

        XCTAssertEqual(store.account, AccountId("acme"))
        XCTAssertEqual(store.typed, "")
        XCTAssertEqual(store.phase, .asking)
    }
}
