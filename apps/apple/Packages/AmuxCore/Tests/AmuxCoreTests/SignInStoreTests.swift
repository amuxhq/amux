import Foundation
import XCTest
@testable import AmuxCore

/// A cloud that answers one way, so the store's three outcomes can each be
/// reached without a browser.
private struct OneAnswer: CloudService, @unchecked Sendable {
    var answer: Result<SignedInAccount, CloudError>
    var entitlement: Entitlement = .active(grant: .purchased(.web), renews: nil)
    /// What was asked for and what was let go of, in order.
    let asked = Asked()

    final class Asked: @unchecked Sendable {
        var intents: [SignInIntent] = []
        var forgotten: [AccountId] = []
    }

    func handOver(_ id: AccountId) async -> Handover? {
        Handover(cloud: URL(string: "https://amux.test")!, client: "mobile", refreshToken: "refresh-\(id)")
    }
    func lend(from lender: @escaping @Sendable (AccountId) async -> String?) async {}
    func forgetSession(_ id: AccountId) async throws { asked.forgotten.append(id) }
    func signIn(_ intent: SignInIntent, presenting: any WebAuthPresenter) async throws(CloudError) -> SignedInAccount {
        asked.intents.append(intent)
        switch answer {
        case .success(let account): return account
        case .failure(let error): throw error
        }
    }

    func account(_ id: AccountId) async throws(CloudError) -> AccountFacts {
        throw .unauthenticated
    }

    func recordPurchase(_ id: AccountId, signedTransaction: String) async throws(CloudError) {
        throw .unauthenticated
    }

    func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement { entitlement }

    func requestDeletion(
        _ id: AccountId, confirmedEmail: String
    ) async throws(CloudError) -> DeletionOutcome {
        throw .unauthenticated
    }

}

private struct Silent: WebAuthPresenter {
    func present(_ url: URL, callbackScheme: String) async throws(CloudError) -> URL {
        URL(string: "amux://callback")!
    }
}

/// What keeping an account did, in order.
@MainActor
private final class Kept {
    var accounts: [AccountId] = []
    var refusal: CloudError?

    func keep(_ account: SignedInAccount) async -> CloudError? {
        accounts.append(account.id)
        return refusal
    }
}

@MainActor
final class SignInStoreTests: XCTestCase {
    private let ada = SignedInAccount(id: AccountId("ada"), email: "ada@example.com")

    func testASignInThatSucceedsKeepsTheAccount() async {
        let store = SignInStore()
        let kept = Kept()
        let cloud = OneAnswer(answer: .success(ada))

        let account = await store.signIn(with: cloud, presenting: Silent(), keeping: kept.keep)

        XCTAssertEqual(account, ada)
        XCTAssertEqual(store.phase, .signedIn(ada))
        XCTAssertEqual(kept.accounts, [ada.id])
    }

    func testAnAccountThatCouldNotBeKeptSaysWhy() async {
        let store = SignInStore()
        let kept = Kept()
        kept.refusal = .refused("this account's installation did not start")
        let cloud = OneAnswer(answer: .success(ada))

        let account = await store.signIn(with: cloud, presenting: Silent(), keeping: kept.keep)

        XCTAssertNil(account)
        XCTAssertEqual(store.phase, .failed("this account's installation did not start"))
    }

    func testASignInTheCloudRefusesSaysWhatTheCloudSaid() async {
        let store = SignInStore()
        let kept = Kept()
        let cloud = OneAnswer(answer: .failure(.refused("that address is not recognised")))

        await store.signIn(with: cloud, presenting: Silent(), keeping: kept.keep)

        XCTAssertEqual(store.phase, .failed("that address is not recognised"))
        XCTAssertEqual(kept.accounts, [])
    }

    func testComingBackWithoutSigningInLeavesNothingToDismiss() async {
        let store = SignInStore()
        let cloud = OneAnswer(answer: .failure(.cancelled))

        await store.signIn(with: cloud, presenting: Silent(), keeping: Kept().keep)

        // Cancelling is a decision, not a failure: the screen is where it was
        // before the button was pressed, with nothing on it to clear.
        XCTAssertEqual(store.phase, .ready)
    }

    func testASecondPressWhileTheBrowserIsUpStartsNothing() async {
        let store = SignInStore(phase: .handingOff)
        let cloud = OneAnswer(answer: .success(ada))

        let account = await store.signIn(with: cloud, presenting: Silent(), keeping: Kept().keep)

        XCTAssertNil(account)
        XCTAssertEqual(store.phase, .handingOff)
    }

    func testAddingAnAccountAsksForTheChooserAndSigningBackInNamesTheAccount() async {
        let store = SignInStore()
        let cloud = OneAnswer(answer: .success(ada))
        await store.signIn(with: cloud, presenting: Silent(), keeping: Kept().keep)
        store.begin(.returning(ada))
        await store.signIn(with: cloud, presenting: Silent(), keeping: Kept().keep)

        XCTAssertEqual(cloud.asked.intents, [.adding, .returning(ada)])
        XCTAssertEqual(store.phase, .signedIn(ada))
    }

    /// Asked for one account and handed another: nothing is kept until the
    /// person chooses, and the screen names both.
    func testSigningBackInAsSomebodyElseKeepsNobody() async {
        let work = SignedInAccount(id: AccountId("work"), email: "team@acme.example")
        let stranger = SignedInAccount(id: AccountId("stranger"), email: "jw@example.com")
        let store = SignInStore()
        store.begin(.returning(work))
        let kept = Kept()

        let account = await store.signIn(
            with: OneAnswer(answer: .success(stranger)), presenting: Silent(), keeping: kept.keep)

        XCTAssertNil(account)
        XCTAssertEqual(store.phase, .mismatched(wanted: work, got: stranger))
        XCTAssertEqual(kept.accounts, [])
    }

    func testKeepingTheAccountThatCameBackKeepsIt() async {
        let work = SignedInAccount(id: AccountId("work"), email: "team@acme.example")
        let stranger = SignedInAccount(id: AccountId("stranger"), email: "jw@example.com")
        let store = SignInStore(phase: .mismatched(wanted: work, got: stranger),
                                intent: .returning(work))
        let kept = Kept()

        await store.keep(keeping: kept.keep)

        XCTAssertEqual(store.phase, .signedIn(stranger))
        XCTAssertEqual(kept.accounts, [stranger.id])
    }

    /// Turned down, everything that sign-in obtained is let go.
    func testTurningDownTheAccountThatCameBackLetsGoOfItsSession() async {
        let work = SignedInAccount(id: AccountId("work"), email: "team@acme.example")
        let stranger = SignedInAccount(id: AccountId("stranger"), email: "jw@example.com")
        let cloud = OneAnswer(answer: .success(stranger))
        let store = SignInStore(phase: .mismatched(wanted: work, got: stranger))

        await store.discard(with: cloud)

        XCTAssertEqual(store.phase, .ready)
        XCTAssertEqual(cloud.asked.forgotten, [stranger.id])
    }

    func testTheScreenNamesTheHostTheHandOffOpens() {
        XCTAssertEqual(SignInStore().host, "amux.sh")
        XCTAssertEqual(CloudEndpoint.production.callback.absoluteString, "amux://callback")
    }
}
