import Foundation
import XCTest

@testable import AmuxCore

/// The phone following what the account buys while it stays open: a lapse or
/// a purchase made elsewhere reaches You without the app being reopened.
@MainActor
final class EntitlementWatchTests: XCTestCase {
    private let ada = AccountId("ada")
    private var answer: Entitlement? = Entitlement.none
    private var reads = 0
    private var relinked: [AccountId] = []
    private var now = Date(timeIntervalSince1970: 1_000)

    private func registry(_ entitlement: Entitlement) -> AccountRegistry {
        let registry = AccountRegistry()
        registry.show([
            ProfileView(
                id: "p1", label: "ada", subject: "ada",
                account: AccountView(
                    binding: .signedIn, email: "ada@example.com", name: "Ada", picture: "", relay: .off, pro: nil)),
        ])
        registry.entitlement(entitlement, for: ada)
        return registry
    }

    private func watch(_ registry: AccountRegistry) -> EntitlementWatch {
        EntitlementWatch(
            registry: registry,
            read: { [unowned self] _ in
                self.reads += 1
                return self.answer
            },
            relink: { [unowned self] id in
                self.relinked.append(id)
                return true
            },
            now: { [unowned self] in self.now })
    }

    func testALapseAndAPurchaseElsewhereReachTheAccountOnScreen() async {
        let registry = registry(.active(grant: .granted, renews: nil))
        let watch = watch(registry)

        answer = Entitlement.none
        await watch.look()
        XCTAssertEqual(registry.selectedAccount?.entitlement, Entitlement.none)
        XCTAssertEqual(registry.selectedAccount?.entitlement.summary, "Not subscribed · hosts on this network still work")
        XCTAssertEqual(relinked, [ada], "the relay link was not asked to read the lapse")

        let bought = Entitlement.active(grant: .purchased(.web), renews: nil)
        answer = bought
        now += EntitlementWatch.spacing
        await watch.look()
        XCTAssertEqual(registry.selectedAccount?.entitlement, bought)
        XCTAssertEqual(relinked, [ada, ada])
    }

    func testLookingAgainSoonAfterAsksNothingUnlessForced() async {
        let registry = registry(.none)
        let watch = watch(registry)
        await watch.look()
        await watch.look()
        XCTAssertEqual(reads, 1, "a second look inside the spacing called amux.sh again")

        await watch.look(force: true)
        XCTAssertEqual(reads, 2, "a purchase's read waited out the spacing")
        XCTAssertEqual(relinked, [ada], "a purchase did not have the relay link read it")
    }

    func testAnUnchangedAnswerLeavesTheRelayLinkAlone() async {
        let registry = registry(.none)
        let watch = watch(registry)
        answer = Entitlement.none
        await watch.look()
        now += EntitlementWatch.spacing
        await watch.look()
        XCTAssertEqual(reads, 2)
        XCTAssertEqual(relinked, [], "an unchanged answer had the relay link renew its credential")
    }

    func testNoAnswerKeepsWhatWasLastSaid() async {
        let held = Entitlement.active(grant: .purchased(.appStore), renews: nil)
        let registry = registry(held)
        let watch = watch(registry)
        answer = nil
        let answered = await watch.look()
        XCTAssertFalse(answered)
        XCTAssertEqual(registry.selectedAccount?.entitlement, held)
        XCTAssertEqual(relinked, [])
    }
}
