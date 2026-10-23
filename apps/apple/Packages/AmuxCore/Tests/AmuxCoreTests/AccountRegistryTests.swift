import Foundation
import XCTest

@testable import AmuxCore

@MainActor
final class AccountRegistryTests: XCTestCase {
    /// A profile as the installation lists it: nobody's when `subject` is
    /// empty, else that account's.
    private func profile(
        _ id: String, _ subject: String = "", _ binding: AccountBinding = .signedIn
    ) -> ProfileView {
        ProfileView(
            id: id, label: subject.isEmpty ? "default" : subject, subject: subject,
            account: AccountView(
                binding: subject.isEmpty ? .unbound : binding,
                email: subject.isEmpty ? "" : "\(subject)@example.com", name: subject, picture: "",
                relay: .off, pro: nil))
    }

    private func file() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString).appendingPathComponent("accounts.json")
    }

    func testAPhoneNobodySignedInOnShowsTheProfileNobodySignedInOn() {
        let registry = AccountRegistry()
        registry.show([profile("p0")])
        XCTAssertNil(registry.selected)
        XCTAssertTrue(registry.accounts.isEmpty)
        XCTAssertEqual(registry.unbound, "p0")
        XCTAssertEqual(registry.profile, "p0")
        XCTAssertNil(registry.stores)
        XCTAssertEqual(registry.gate, .signedOut)
    }

    func testTheFirstSignInTakesOverTheProfileNobodySignedInOn() {
        let registry = AccountRegistry()
        registry.show([profile("p0")])
        var switched: [AccountId?] = []
        registry.switching = { switched.append($0) }
        registry.show([profile("p0", "ada")])
        XCTAssertEqual(registry.selected, AccountId("ada"))
        XCTAssertEqual(registry.profile, "p0", "ada keeps what the phone paired before")
        XCTAssertNil(registry.unbound)
        XCTAssertEqual(registry.selectedAccount?.account.email, "ada@example.com")
        XCTAssertEqual(registry.selectedAccount?.name, "ada")
        XCTAssertEqual(registry.stores?.account, AccountId("ada"))
        XCTAssertEqual(switched, [AccountId("ada")])
    }

    func testAnotherAccountGetsItsOwnProfileAndGoesOnScreenWhenSelected() {
        let registry = AccountRegistry()
        registry.show([profile("p0", "ada")])
        var switched: [AccountId?] = []
        registry.switching = { switched.append($0) }
        registry.show([profile("p0", "ada"), profile("p1", "bob")])
        XCTAssertEqual(registry.selected, AccountId("ada"), "a new profile does not take the screen")
        XCTAssertEqual(switched, [])
        registry.select(AccountId("bob"))
        XCTAssertEqual(registry.selected, AccountId("bob"))
        XCTAssertEqual(registry.profile, "p1")
        XCTAssertEqual(registry.stores?.account, AccountId("bob"))
        XCTAssertEqual(switched, [AccountId("bob")])
    }

    func testASignedOutAccountStaysOnScreenAndListed() {
        let registry = AccountRegistry()
        registry.show([profile("p0", "ada")])
        registry.show([profile("p0", "ada", .signedOut)])
        XCTAssertEqual(registry.selected, AccountId("ada"), "a signed-out account stays on screen")
        XCTAssertEqual(registry.selectedAccount?.signedIn, false)
        XCTAssertEqual(registry.selectedAccount?.line, "Signed out")
        XCTAssertEqual(registry.profile, "p0")
        // A paused profile is still signed in: only its relay link is down.
        registry.show([profile("p0", "ada", .paused)])
        XCTAssertEqual(registry.selectedAccount?.signedIn, true)
    }

    func testLeavingTheAccountOnScreenMovesToASignedInOneThenAnyThenNobody() {
        let registry = AccountRegistry()
        registry.show([
            profile("p0", "ada", .signedOut), profile("p1", "bob"), profile("p2", "cid"),
        ])
        registry.select(AccountId("cid"))
        registry.leave(AccountId("cid"))
        XCTAssertEqual(registry.selected, AccountId("bob"), "the first signed-in account")
        registry.show([profile("p0", "ada", .signedOut), profile("p1", "bob")])
        registry.leave(AccountId("bob"))
        XCTAssertEqual(registry.selected, AccountId("ada"), "then any account")
        registry.show([profile("p0", "ada", .signedOut)])
        registry.leave(AccountId("ada"))
        XCTAssertNil(registry.selected, "then nobody")
        XCTAssertNil(registry.stores)
    }

    func testRemovingTheOnlyAccountLeavesAFreshProfileNobodySignedInOn() {
        let registry = AccountRegistry()
        registry.show([profile("p0", "ada")])
        // The installation keeps one profile: a fresh one comes first.
        registry.show([profile("p0", "ada"), profile("p1")])
        XCTAssertEqual(registry.profile, "p0")
        registry.leave(AccountId("ada"))
        XCTAssertEqual(registry.profile, "p1")
        registry.show([profile("p1")])
        XCTAssertNil(registry.selected)
        XCTAssertTrue(registry.accounts.isEmpty)
        XCTAssertEqual(registry.profile, "p1")
    }

    func testAnAccountWhoseProfileIsGoneLeavesTheScreen() {
        let registry = AccountRegistry()
        registry.show([profile("p0", "ada"), profile("p1", "bob")])
        XCTAssertEqual(registry.selected, AccountId("ada"))
        registry.show([profile("p1", "bob")])
        XCTAssertEqual(registry.selected, AccountId("bob"))
        XCTAssertEqual(registry.profile, "p1")
    }

    func testTheGateReadsTheLinkThenTheSavedEntitlement() {
        let registry = AccountRegistry()
        XCTAssertEqual(registry.gate, .signedOut)
        registry.show([profile("p0", "ada")])
        registry.entitlement(.active(grant: .purchased(.web), renews: nil), for: AccountId("ada"))
        XCTAssertEqual(registry.gate, .ready)
        registry.stores?.hosts.show(AccountView(
            binding: .signedIn, email: "", name: "", picture: "", relay: .connected, pro: false))
        XCTAssertEqual(registry.gate, .unsubscribed, "the link's word outranks the saved one")
        registry.show([profile("p0", "ada", .signedOut)])
        XCTAssertEqual(registry.gate, .signedOut)
    }

    func testTheRegistrySurvivesARelaunch() {
        let file = file()
        let first = AccountRegistry(file: file)
        first.show([profile("p0", "ada"), profile("p1", "bob")])
        first.saw(hosts: 2, attention: 1, for: AccountId("ada"))
        first.entitlement(.active(grant: .granted, renews: nil), for: AccountId("ada"))
        first.select(AccountId("bob"))
        let again = AccountRegistry(file: file)
        XCTAssertEqual(again.selected, AccountId("bob"), "the account on screen")
        XCTAssertEqual(again.stores?.account, AccountId("bob"))
        // Which accounts there are is the installation's to say.
        XCTAssertTrue(again.accounts.isEmpty)
        again.show([profile("p0", "ada"), profile("p1", "bob")])
        XCTAssertEqual(again.selected, AccountId("bob"))
        XCTAssertEqual(again.profile, "p1")
        let ada = again.accounts.first { $0.id == AccountId("ada") }
        XCTAssertEqual(ada?.line, "2 hosts")
        XCTAssertEqual(ada?.attention, 1)
        XCTAssertEqual(ada?.entitlement, .active(grant: .granted, renews: nil))
        XCTAssertEqual(ada?.profile, "p0")
        XCTAssertFalse(again.persistenceFailed)
    }

    func testALateAnswerForAnAccountNotOnScreenIsDropped() {
        let registry = AccountRegistry()
        registry.show([profile("p0", "ada")])
        XCTAssertEqual(registry.accept(1, for: AccountId("ada")), 1)
        XCTAssertNil(registry.accept(1, for: AccountId("bob")))
        XCTAssertEqual(registry.dropped, 1)
    }

    func testAnAccountRemembersWhatItListedWhenItGoesOffScreen() {
        let file = file()
        let registry = AccountRegistry(file: file)
        let both = [profile("p0", "ada"), profile("p1", "bob")]
        registry.show(both)
        registry.stores?.saw?(2, 1)
        registry.select(AccountId("bob"))
        registry.stores?.saw?(0, 0)
        let again = AccountRegistry(file: file)
        again.show(both)
        let ada = again.accounts.first { $0.id == AccountId("ada") }
        XCTAssertEqual(ada?.line, "2 hosts")
        XCTAssertEqual(ada?.attention, 1)
        let bob = again.accounts.first { $0.id == AccountId("bob") }
        XCTAssertEqual(bob?.line, "bob@example.com", "nothing paired yet reads as the address")
    }
}
