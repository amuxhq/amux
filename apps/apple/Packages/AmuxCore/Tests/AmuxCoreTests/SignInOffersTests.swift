import Foundation
import XCTest

@testable import AmuxCore

/// Which account offered across a pairing the phone asks about, and what an
/// answer leaves behind.
@MainActor
final class SignInOffersTests: XCTestCase {
    private let desk = HostId(UUID(uuidString: "00000000-0000-0000-0000-000000000601")!)
    private let laptop = HostId(UUID(uuidString: "00000000-0000-0000-0000-000000000602")!)

    private func offer(_ host: HostId, _ name: String) -> SignInOfferView {
        SignInOfferView(
            hostId: host.bytes, hostName: name, email: "ada@example.com", accountName: "Ada")
    }

    private func account(_ binding: AccountBinding) -> AccountView {
        AccountView(
            binding: binding, email: binding == .unbound ? "" : "ada@example.com", name: "Ada",
            picture: "", relay: .off, pro: nil)
    }

    /// An offer a paired machine made this phone is asked about by its
    /// account's address and the machine that made it.
    func testAnOfferMadeToThisPhoneIsAskedAbout() {
        let offers = SignInOffers()
        let hosts = HostsStore()
        XCTAssertNil(offers.prompt(hosts: hosts))
        offers.listed([offer(desk, "desk")])
        XCTAssertEqual(offers.prompt(hosts: hosts), .offered(offer(desk, "desk")))
        XCTAssertEqual(offers.prompt(hosts: hosts)?.role, .receiver)
        offers.listed([])
        XCTAssertNil(offers.prompt(hosts: hosts), "an offer gone is not asked about")
    }

    /// A machine paired while this phone is signed in, which says it is
    /// signed out, may be offered this phone's account; one that says it is
    /// signed in, or has not said, is not.
    func testAMachineJustPairedThatIsSignedOutIsOfferedTheAccount() {
        let offers = SignInOffers()
        let hosts = HostsStore()
        hosts.show(account(.signedIn))
        offers.paired(desk)
        hosts.show([Cards.host(desk, name: "desk")])
        XCTAssertNil(offers.prompt(hosts: hosts), "the desk has not said yet")
        hosts.show([Cards.host(desk, name: "desk", signedIn: true)])
        XCTAssertNil(offers.prompt(hosts: hosts), "the desk is signed in already")
        let signedOut = Cards.host(desk, name: "desk", signedIn: false)
        hosts.show([signedOut])
        XCTAssertEqual(
            offers.prompt(hosts: hosts), .offering(host: signedOut, email: "ada@example.com"))
        XCTAssertEqual(offers.prompt(hosts: hosts)?.role, .offerer)

        // A machine paired before this launch is not asked about.
        hosts.show([signedOut, Cards.host(laptop, name: "laptop", signedIn: false)])
        offers.notOffering(desk)
        XCTAssertNil(offers.prompt(hosts: hosts))
    }

    /// A phone that is not signed in has no account to offer.
    func testASignedOutPhoneOffersNothing() {
        let offers = SignInOffers()
        let hosts = HostsStore()
        offers.paired(desk)
        hosts.show([Cards.host(desk, name: "desk", signedIn: false)])
        for binding in [AccountBinding.unbound, .signedOut] {
            hosts.show(account(binding))
            XCTAssertNil(offers.prompt(hosts: hosts), "\(binding)")
        }
    }

    /// Once offered, the machine is not asked about again, whatever the
    /// runtime answered.
    func testAnOfferMadeIsAskedOnce() {
        let offers = SignInOffers()
        let hosts = HostsStore()
        hosts.show(account(.signedIn))
        offers.paired(desk)
        hosts.show([Cards.host(desk, name: "desk", signedIn: false)])
        offers.answers(desk.bytes)
        XCTAssertEqual(offers.answering, desk.bytes)
        offers.offered(.success(Nothing()), to: desk)
        XCTAssertNil(offers.answering)
        XCTAssertNil(offers.prompt(hosts: hosts))
    }

    /// An offer accepted while its machine was offline says so with the
    /// machine's name, and stays to be accepted again; once the machine
    /// takes the offer back the saying goes too.
    func testAnOfflineOffererIsSaidByName() {
        let offers = SignInOffers()
        offers.listed([offer(desk, "desk")])
        offers.answers(desk.bytes)
        offers.answered(.success(.unreachable), from: desk.bytes)
        XCTAssertEqual(offers.failure, .unreachable("desk"))
        XCTAssertNil(offers.answering)
        offers.listed([offer(desk, "desk")])
        XCTAssertEqual(offers.failure, .unreachable("desk"), "the offer still stands")
        offers.listed([])
        XCTAssertNil(offers.failure)
    }

    func testAnAcceptedOfferLeavesNothingToSay() {
        let offers = SignInOffers()
        offers.listed([offer(desk, "desk")])
        offers.answered(.failure(RuntimeFailure("no ticket")), from: desk.bytes)
        XCTAssertEqual(offers.failure, .refused("no ticket"))
        offers.answered(.success(.declined), from: desk.bytes)
        XCTAssertNil(offers.failure)
    }

    /// Showing a prompt is reported once, however often it is drawn.
    func testAPromptIsReportedShownOnce() {
        let offers = SignInOffers()
        let prompt = SignInPrompt.offered(offer(desk, "desk"))
        XCTAssertTrue(offers.firstShown(prompt))
        XCTAssertFalse(offers.firstShown(prompt))
    }
}
