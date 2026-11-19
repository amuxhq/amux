import AmuxCore
import XCTest
@testable import AmuxFeatures

/// What the paywall says to somebody who already has what it sells.
@MainActor
final class PaywallWordingTests: XCTestCase {
    /// A purchase names the place it was bought, because that is where it is
    /// cancelled.
    func testAPurchaseNamesWhereItWasBoughtAndWhereItIsManaged() {
        XCTAssertEqual(Paywall.subscribed(.purchased(.appStore)), "Subscribed in the App Store")
        XCTAssertEqual(Paywall.subscribed(.purchased(.web)), "Subscribed on amux.sh")
        XCTAssertEqual(Paywall.honoured(.purchased(.appStore)), "Manage your subscription in the App Store.")
        XCTAssertEqual(Paywall.honoured(.purchased(.web)), "Manage your subscription on amux.sh.")
    }

    /// Access that was given was bought nowhere. Calling it a subscription, or
    /// offering to manage it, would send somebody looking for a billing page
    /// that does not exist for their account.
    func testGivenAccessClaimsNoStoreAndOffersNothingToManage() {
        let headline = Paywall.subscribed(.granted)
        let explanation = Paywall.honoured(.granted)
        XCTAssertEqual(headline, "Pro is on for this account")
        XCTAssertFalse(headline.contains("Subscribed"))
        for said in [headline, explanation] {
            XCTAssertFalse(said.contains("App Store"))
            XCTAssertFalse(said.contains("amux.sh"))
            XCTAssertFalse(said.contains("Manage"))
        }
        XCTAssertEqual(explanation, "Pro is included with this account.")
    }
}
