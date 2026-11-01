import Foundation
import XCTest
@testable import AmuxCore

/// A store that answers one way, so every outcome a person can reach is
/// reachable without a sandbox account or a purchase sheet.
private final class OneStore: StoreFront, @unchecked Sendable {
    var offered: [Plan] = [
        Plan(id: Plan.monthlyID, period: .monthly, price: "£7.99"),
        Plan(id: Plan.yearlyID, period: .yearly, price: "£79.99", saving: "2 months free"),
    ]
    var purchase: Result<PurchaseOutcome, StoreError> = .success(.bought(OneStore.signed))
    var restored: Result<PurchaseOutcome, StoreError> = .success(.nothingToRestore)
    var listing: StoreError?
    /// What the store is still holding, and what has been finished with it.
    /// A purchase finished before the cloud has it is the failure this suite
    /// exists to catch, so both are recorded.
    var held: [SignedPurchase] = []
    private(set) var finished: [String] = []

    static let signed = SignedPurchase(
        id: "1000", productID: Plan.yearlyID, signed: "signed.transaction.one")

    init(
        offered: [Plan]? = nil,
        purchase: Result<PurchaseOutcome, StoreError>? = nil,
        restored: Result<PurchaseOutcome, StoreError>? = nil,
        listing: StoreError? = nil,
        held: [SignedPurchase] = []
    ) {
        if let offered { self.offered = offered }
        if let purchase { self.purchase = purchase }
        if let restored { self.restored = restored }
        self.listing = listing
        self.held = held
    }

    func finish(_ purchase: SignedPurchase) async {
        finished.append(purchase.id)
        held.removeAll { $0.id == purchase.id }
    }

    func unfinished() async -> [SignedPurchase] { held }

    func approvals() -> AsyncStream<SignedPurchase> { AsyncStream { $0.finish() } }

    func plans() async throws(StoreError) -> [Plan] {
        if let listing { throw listing }
        return offered
    }

    func buy(_ plan: Plan) async throws(StoreError) -> PurchaseOutcome {
        switch purchase {
        case .success(let outcome): return outcome
        case .failure(let error): throw error
        }
    }

    func restore() async throws(StoreError) -> PurchaseOutcome {
        switch restored {
        case .success(let outcome): return outcome
        case .failure(let error): throw error
        }
    }
}

/// An account service that answers one way about a purchase and keeps what it
/// was handed. Nothing else here is asked of it.
private final class OneCloud: CloudService, @unchecked Sendable {
    var recording: CloudError?
    private(set) var recorded: [String] = []

    init(recording: CloudError? = nil) {
        self.recording = recording
    }

    func recordPurchase(_ id: AccountId, signedTransaction: String) async throws(CloudError) {
        recorded.append(signedTransaction)
        if let recording { throw recording }
    }

    func handOver(_ id: AccountId) async -> Handover? { nil }
    func lend(from lender: @escaping @Sendable (AccountId) async -> String?) async {}
    func forgetSession(_ id: AccountId) async throws {}
    func signIn(_ intent: SignInIntent, presenting: any WebAuthPresenter) async throws(CloudError) -> SignedInAccount {
        throw .unauthenticated
    }
    func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement { .none }
    func requestDeletion(
        _ id: AccountId, confirmedEmail: String
    ) async throws(CloudError) -> DeletionOutcome {
        throw .unauthenticated
    }
}

@MainActor
final class PaywallStoreTests: XCTestCase {
    private func loaded(_ store: OneStore = OneStore()) async -> PaywallStore {
        let model = PaywallStore()
        await model.load(from: store)
        return model
    }

    func testTheYearIsChosenBeforeAnybodyChoosesAnything() async {
        let model = await loaded()
        XCTAssertEqual(model.plans.map(\.period), [.monthly, .yearly])
        XCTAssertEqual(model.chosen, .yearly)
        XCTAssertEqual(model.plan?.price, "£79.99")
    }

    func testChoosingTheMonthChangesWhatWouldBeBought() async {
        let model = await loaded()
        model.choose(.monthly)
        XCTAssertEqual(model.plan?.id, Plan.monthlyID)
    }

    func testBuyingLeavesThePurchaseWaitingOnTheAccountService() async {
        let model = await loaded()
        let outcome = await model.buy(OneStore())

        // Bought is not subscribed. What this account may do is amux.sh's to
        // say and it has not been told yet, so nothing here claims it.
        XCTAssertEqual(outcome, .bought(OneStore.signed))
        XCTAssertEqual(model.phase, .confirming)
        XCTAssertEqual(model.holding, OneStore.signed)
        XCTAssertEqual(model.entitlement, Entitlement.none)
        XCTAssertFalse(model.entitled)
    }

    func testAConfirmedPurchaseIsFinishedWithTheStoreOnlyAfterTheCloudHasIt() async {
        let model = await loaded()
        let store = OneStore(held: [OneStore.signed])
        let cloud = OneCloud()
        guard case .bought(let purchase)? = await model.buy(store) else {
            return XCTFail("the store was scripted to sell one")
        }
        let taken = await model.confirm(
            purchase, with: cloud, as: AccountId("ada"), finishing: store)

        XCTAssertTrue(taken)
        XCTAssertEqual(cloud.recorded, [OneStore.signed.signed])
        XCTAssertEqual(store.finished, [OneStore.signed.id])
        XCTAssertNil(model.holding)
    }

    func testAPurchaseTheCloudCouldNotBeToldAboutIsKeptAndSaidToBeUnconfirmed() async {
        let model = await loaded()
        let store = OneStore(held: [OneStore.signed])
        let cloud = OneCloud(recording: .network("offline"))
        let taken = await model.confirm(
            OneStore.signed, with: cloud, as: AccountId("ada"), finishing: store)

        XCTAssertFalse(taken)
        XCTAssertEqual(model.phase, .unconfirmed(.unreachable))
        // The transaction is still the store's, which is what makes a retry
        // — pressed, or made by the next launch — possible at all.
        XCTAssertEqual(store.finished, [])
        XCTAssertEqual(store.held, [OneStore.signed])
        XCTAssertEqual(model.holding, OneStore.signed)
    }

    func testACloudThatRefusesThePurchaseReadsDifferentlyFromOneItCannotReach() async {
        let model = await loaded()
        let store = OneStore(held: [OneStore.signed])
        let cloud = OneCloud(recording: .refused("that transaction is already another account's."))
        await model.confirm(OneStore.signed, with: cloud, as: AccountId("ada"), finishing: store)

        XCTAssertEqual(
            model.phase,
            .unconfirmed(.refused("that transaction is already another account's.")))
        XCTAssertEqual(store.finished, [])
    }

    /// The cloud takes the post and the entitlement read back afterwards still
    /// says nothing. Somebody has paid: leaving the screen on a disabled
    /// "Confirming with amux.sh…" would give them nothing to press at all.
    func testAPurchaseTheCloudTookWithoutAccessYetLeavesSomethingToPress() async {
        let model = await loaded()
        let store = OneStore(held: [OneStore.signed])
        let cloud = OneCloud()
        let taken = await model.confirm(
            OneStore.signed, with: cloud, as: AccountId("ada"), finishing: store)
        XCTAssertTrue(taken)
        XCTAssertEqual(model.phase, .confirming)

        // The read the screen believes, which is what the cloud actually says
        // this account may do.
        model.entitled(.none)

        XCTAssertEqual(model.phase, .unconfirmed(.switchingOn))
        // Pressable: nothing is waiting on anybody else any more.
        XCTAssertFalse(model.working)
        // And still not for sale a second time — the phase the screen reads
        // hides the plans and turns the button into asking again.
        XCTAssertFalse(model.entitled)
    }

    /// Asking again, and the answer is still no access. The screen stays where
    /// somebody can ask once more rather than falling back into the dead wait.
    func testAskingAgainWithoutAccessLeavesTheScreenAskable() async {
        let model = await loaded()
        let store = OneStore(held: [OneStore.signed])
        let cloud = OneCloud()
        await model.confirm(OneStore.signed, with: cloud, as: AccountId("ada"), finishing: store)
        model.entitled(.none)
        // The Retry path with nothing left to send: the transaction is
        // finished, so all that is left is reading the entitlement again.
        XCTAssertNil(model.holding)
        model.entitled(.none)

        XCTAssertEqual(model.phase, .unconfirmed(.switchingOn))
        XCTAssertFalse(model.working)
    }

    /// The same read arriving on a screen nobody has bought anything on is not
    /// news, and must not turn a paywall into a purchase that is switching on.
    func testAnEmptyEntitlementOnAnUntouchedPaywallSaysNothing() async {
        let model = await loaded()
        model.entitled(.none)
        XCTAssertEqual(model.phase, .ready)
    }

    func testAnUnconfirmedPurchaseIsOfferedAgainByTheNextLaunchWithoutAnybodyPressing() async {
        let model = await loaded()
        let store = OneStore(held: [OneStore.signed])
        let cloud = OneCloud()
        let taken = await model.confirmOutstanding(
            in: store, with: cloud, as: AccountId("ada"))

        XCTAssertTrue(taken)
        XCTAssertEqual(cloud.recorded, [OneStore.signed.signed])
        XCTAssertEqual(store.finished, [OneStore.signed.id])
    }

    func testALaunchWithNothingOutstandingAsksTheCloudAboutNothing() async {
        let model = await loaded()
        let cloud = OneCloud()
        let taken = await model.confirmOutstanding(
            in: OneStore(), with: cloud, as: AccountId("ada"))

        XCTAssertFalse(taken)
        XCTAssertEqual(cloud.recorded, [])
        XCTAssertEqual(model.phase, .ready)
    }

    func testTheScreenCannotBePressedWhileTheCloudIsBeingTold() async {
        let model = await loaded()
        await model.buy(OneStore())

        XCTAssertEqual(model.phase, .confirming)
        XCTAssertTrue(model.working)
        // Buying again while one purchase is being confirmed would be a second
        // charge for the same month.
        let second = await model.buy(OneStore())
        XCTAssertNil(second)
    }

    func testClosingTheStoresSheetLeavesTheScreenExactlyWhereItWas() async {
        let model = await loaded()
        model.choose(.monthly)
        let outcome = await model.buy(OneStore(purchase: .success(.cancelled)))

        // Cancelling is a decision, not a failure: nothing is said, and the
        // plan that was chosen is still chosen.
        XCTAssertEqual(outcome, .cancelled)
        XCTAssertEqual(model.phase, .ready)
        XCTAssertEqual(model.chosen, .monthly)
    }

    func testAPurchaseTheStoreCannotFinishStopsOfferingToBuyAgain() async {
        let model = await loaded()
        let outcome = await model.buy(OneStore(purchase: .success(.pending)))

        XCTAssertEqual(outcome, .pending)
        XCTAssertEqual(model.phase, .awaitingApproval)
        // Nothing is bought and nothing is owed: the entitlement has not moved.
        XCTAssertEqual(model.entitlement, Entitlement.none)
        XCTAssertFalse(model.entitled)
    }

    func testAPurchaseTheStoreRefusesSaysWhatTheStoreSaid() async {
        let model = await loaded()
        let outcome = await model.buy(
            OneStore(purchase: .failure(.failed("your payment method was declined"))))

        XCTAssertNil(outcome)
        XCTAssertEqual(model.phase, .failed("your payment method was declined"))
    }

    func testRestoringFindsWhatThisAppleAccountAlreadyHas() async {
        let model = await loaded()
        let outcome = await model.restore(
            OneStore(restored: .success(.bought(OneStore.signed))))

        // Found, and carrying the signature: a restored subscription reaches
        // the account service by exactly the same road a new one does.
        XCTAssertEqual(outcome, .bought(OneStore.signed))
        XCTAssertEqual(model.phase, .confirming)
        XCTAssertEqual(model.holding, OneStore.signed)
    }

    func testRestoringNothingIsAnAnswerAndIsSaidAsOne() async {
        let model = await loaded()
        let outcome = await model.restore(OneStore())

        XCTAssertEqual(outcome, .nothingToRestore)
        XCTAssertEqual(
            model.phase, .failed("there is nothing on this Apple Account to restore"))
    }

    func testASubscriptionBoughtOnTheWebIsHonouredRatherThanSoldAgain() async {
        let model = await loaded()
        let renews = Date(timeIntervalSince1970: 1_701_004_800)

        // The CLI bought it; the account service is what says so. Nothing on
        // this phone was purchased and there is nothing to restore.
        model.entitled(.active(grant: .purchased(.web), renews: renews))

        XCTAssertTrue(model.entitled)
        XCTAssertEqual(model.grant, .purchased(.web))
        XCTAssertEqual(model.phase, .entitled(.purchased(.web)))
        // The paywall refuses to sell a second subscription for the same thing.
        let outcome = await model.buy(OneStore())
        XCTAssertNil(outcome)
    }

    /// The settings row says only whether the relay is bought; where, and
    /// what works without it, are the paywall's to say.
    func testTheSummaryIsSubscribedOrNot() {
        XCTAssertEqual(Entitlement.active(grant: .purchased(.appStore), renews: nil).summary, "Subscribed")
        XCTAssertEqual(Entitlement.active(grant: .purchased(.web), renews: nil).summary, "Subscribed")
        XCTAssertEqual(Entitlement.active(grant: .granted, renews: nil).summary, "Subscribed")
        XCTAssertEqual(Entitlement.none.summary, "Not subscribed")
        XCTAssertEqual(Entitlement.lapsed(grant: .purchased(.web), endedAt: Date()).summary, "Not subscribed")
    }

    /// The paywall does not offer to sell to somebody who was given it, and
    /// says where it came from without inventing a store.
    func testAGivenEntitlementIsNotSoldASubscription() async {
        let model = await loaded()
        model.entitled(.active(grant: .granted, renews: nil))

        XCTAssertTrue(model.entitled)
        XCTAssertEqual(model.grant, .granted)
        XCTAssertEqual(model.phase, .entitled(.granted))
        let outcome = await model.buy(OneStore())
        XCTAssertNil(outcome)
    }

    func testAStoreWithNothingToSellSaysSoRatherThanDrawingAnEmptyList() async {
        let model = await loaded(OneStore(offered: []))
        XCTAssertTrue(model.plans.isEmpty)
        XCTAssertEqual(model.phase, .failed("the App Store has nothing to sell right now"))
    }

    func testAStoreThatCannotBeReachedSaysWhy() async {
        let model = await loaded(OneStore(listing: .unavailable("no network")))
        XCTAssertEqual(model.phase, .failed("no network"))
    }
}
