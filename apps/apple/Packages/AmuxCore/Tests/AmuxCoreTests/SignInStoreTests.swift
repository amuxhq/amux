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

    /// Kept, the account is on the phone and there is nothing left to say:
    /// the person stays where they pressed Sign In.
    func testASignInThatSucceedsKeepsTheAccountAndLeavesNothingOnScreen() async {
        let store = SignInStore()
        let kept = Kept()
        let cloud = OneAnswer(answer: .success(ada))

        let account = await store.signIn(.adding, with: cloud, presenting: Silent(), keeping: kept.keep)

        XCTAssertEqual(account, ada)
        XCTAssertEqual(store.phase, .ready)
        XCTAssertEqual(kept.accounts, [ada.id])
    }

    func testAnAccountThatCouldNotBeKeptSaysWhy() async {
        let store = SignInStore()
        let kept = Kept()
        kept.refusal = .refused("this account's installation did not start")
        let cloud = OneAnswer(answer: .success(ada))

        let account = await store.signIn(.adding, with: cloud, presenting: Silent(), keeping: kept.keep)

        XCTAssertNil(account)
        XCTAssertEqual(store.phase, .failed("this account's installation did not start"))
    }

    func testASignInTheCloudRefusesSaysWhatTheCloudSaid() async {
        let store = SignInStore()
        let kept = Kept()
        let cloud = OneAnswer(answer: .failure(.refused("that address is not recognised")))

        await store.signIn(.adding, with: cloud, presenting: Silent(), keeping: kept.keep)

        XCTAssertEqual(store.phase, .failed("that address is not recognised"))
        XCTAssertEqual(kept.accounts, [])
    }

    func testComingBackWithoutSigningInLeavesNothingToDismiss() async {
        let store = SignInStore()
        let cloud = OneAnswer(answer: .failure(.cancelled))

        await store.signIn(.adding, with: cloud, presenting: Silent(), keeping: Kept().keep)

        // Cancelling is a decision, not a failure: the screen is where it was
        // before the button was pressed, with nothing on it to clear.
        XCTAssertEqual(store.phase, .ready)
    }

    func testARefusalPutAwayLeavesNothingOnScreen() async {
        let store = SignInStore(phase: .failed("amux.sh did not answer"))

        store.dismiss()

        XCTAssertEqual(store.phase, .ready)
    }

    /// Trying again after a refusal asks amux.sh for the same account the
    /// refused attempt asked for.
    func testTryingAgainAsksForTheSameAccount() async {
        let store = SignInStore()
        let refused = OneAnswer(answer: .failure(.timeout))
        await store.signIn(.returning(ada), with: refused, presenting: Silent(), keeping: Kept().keep)
        XCTAssertEqual(store.phase, .failed("amux.sh did not answer"))

        let cloud = OneAnswer(answer: .success(ada))
        let account = await store.signIn(store.intent, with: cloud, presenting: Silent(), keeping: Kept().keep)

        XCTAssertEqual(account, ada)
        XCTAssertEqual(cloud.asked.intents, [.returning(ada)])
    }

    func testASecondPressWhileTheBrowserIsUpStartsNothing() async {
        let store = SignInStore(phase: .handingOff)
        let cloud = OneAnswer(answer: .success(ada))

        let account = await store.signIn(.adding, with: cloud, presenting: Silent(), keeping: Kept().keep)

        XCTAssertNil(account)
        XCTAssertEqual(store.phase, .handingOff)
        XCTAssertEqual(cloud.asked.intents, [])
    }

    /// The control that started a sign-in is told which account it is for,
    /// and only while the browser is up.
    func testWhatIsPendingIsTheAccountInFlight() {
        XCTAssertNil(SignInStore().pending)
        XCTAssertNil(SignInStore(phase: .failed("no"), intent: .returning(ada)).pending)
        XCTAssertEqual(SignInStore(phase: .handingOff, intent: .returning(ada)).pending, .returning(ada))
    }

    func testAddingAnAccountAsksForTheChooserAndSigningBackInNamesTheAccount() async {
        let store = SignInStore()
        let cloud = OneAnswer(answer: .success(ada))
        await store.signIn(.adding, with: cloud, presenting: Silent(), keeping: Kept().keep)
        await store.signIn(.returning(ada), with: cloud, presenting: Silent(), keeping: Kept().keep)

        XCTAssertEqual(cloud.asked.intents, [.adding, .returning(ada)])
        XCTAssertEqual(store.phase, .ready)
    }

    /// Asked for one account and handed another: nothing is kept until the
    /// person chooses, and the question names both.
    func testSigningBackInAsSomebodyElseKeepsNobody() async {
        let work = SignedInAccount(id: AccountId("work"), email: "team@acme.example")
        let stranger = SignedInAccount(id: AccountId("stranger"), email: "jw@example.com")
        let store = SignInStore()
        let kept = Kept()

        let account = await store.signIn(
            .returning(work), with: OneAnswer(answer: .success(stranger)), presenting: Silent(),
            keeping: kept.keep)

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

        let account = await store.keep(keeping: kept.keep)

        XCTAssertEqual(account, stranger)
        XCTAssertEqual(store.phase, .ready)
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

    func testSigningInOpensAmuxShAndComesBackToThisApp() {
        XCTAssertEqual(CloudEndpoint.production.host, "amux.sh")
        XCTAssertEqual(CloudEndpoint.production.callback.absoluteString, "amux://callback")
    }
}
