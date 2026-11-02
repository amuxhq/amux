import Foundation
import XCTest

@testable import AmuxCore

@MainActor
final class PairingTests: XCTestCase {
    private let pending = PendingPair(
        token: [1, 2], hostId: Cards.desk.bytes, name: "desk", fingerprint: "ab", expiresAtMs: 0,
        via: .direct)

    func testSixDigitsMakeARequestForTheChosenMachine() {
        let pairing = PairingStore()
        pairing.open(machine: Cards.host(addrs: ["10.0.0.2:4000"]))
        pairing.enter("12a34")
        XCTAssertNil(pairing.request)
        XCTAssertEqual(pairing.digits, "1234")
        pairing.enter("1234567")
        XCTAssertEqual(pairing.digits, "123456")
        XCTAssertEqual(
            pairing.request,
            .pin(pin: "123456", addrs: ["10.0.0.2:4000"], hostId: Cards.desk.bytes))
    }

    /// A code copied where the host printed it, "418 207", pastes whole.
    func testAPastedCodeKeepsItsDigits() {
        let pairing = PairingStore()
        pairing.open(machine: Cards.host(addrs: ["10.0.0.2:4000"]))
        pairing.enter("Pairing PIN: 418 207")
        XCTAssertEqual(pairing.digits, "418207")
        XCTAssertNotNil(pairing.request)
    }

    func testAReachedMachineIsConfirmedThenTrusted() {
        let pairing = PairingStore()
        pairing.open()
        let attempt = pairing.checking()
        XCTAssertEqual(pairing.phase, .checking)
        XCTAssertFalse(pairing.taking)
        pairing.reached(.success(pending), attempt: attempt)
        XCTAssertEqual(pairing.phase, .confirming(pending))
        let confirming = pairing.checking()
        pairing.paired(.success("desk"), attempt: confirming)
        XCTAssertEqual(pairing.phase, .trusted("desk"))
    }

    func testEveryRefusalReadsTheSameAndClearsTheDigits() {
        let pairing = PairingStore()
        pairing.open()
        pairing.enter("123456")
        let attempt = pairing.checking()
        pairing.reached(.failure(RuntimeFailure("wrong secret")), attempt: attempt)
        XCTAssertEqual(pairing.phase, .refused)
        XCTAssertEqual(pairing.digits, "")
        XCTAssertTrue(pairing.taking)
    }

    func testARefusalTheCoreWroteForThePersonIsShownInItsWords() {
        let pairing = PairingStore()
        pairing.open()
        let attempt = pairing.checking()
        pairing.reached(.success(pending), attempt: attempt)
        let confirming = pairing.checking()
        pairing.paired(
            .failure(RuntimeFailure("Already paired with desk. Unpair it first.")),
            attempt: confirming)
        XCTAssertEqual(pairing.phase, .refused)
        XCTAssertEqual(pairing.told, "Already paired with desk. Unpair it first.")
        // The next attempt starts with nothing said.
        _ = pairing.checking()
        XCTAssertNil(pairing.told)
    }

    /// Codes and error fragments keep the one refusal the screen draws.
    func testACodeOrAnErrorFragmentIsNotToldAsWords() {
        for reason in [
            "INVALID_PIN", "pairing stream closed",
            "PROTOCOL_VIOLATION: SELF_PAIRING", "the installation hosts no profile 3",
        ] {
            let pairing = PairingStore()
            pairing.open()
            pairing.reached(.failure(RuntimeFailure(reason)), attempt: pairing.checking())
            XCTAssertEqual(pairing.phase, .refused, reason)
            XCTAssertNil(pairing.told, reason)
        }
    }

    /// A lockout reaches the screen as the runtime's sentence, which it
    /// writes from the refusal's typed reason.
    func testALockoutIsToldInTheRuntimesWords() {
        let locked =
            "Too many wrong codes closed pairing on that machine. Open pairing there again for a new code."
        let pairing = PairingStore()
        pairing.open()
        pairing.reached(.failure(RuntimeFailure(locked)), attempt: pairing.checking())
        XCTAssertEqual(pairing.phase, .refused)
        XCTAssertEqual(pairing.told, locked)
    }

    func testARelayOnlyMachineOnAnUnpaidAccountAsksForASubscription() {
        let pairing = PairingStore()
        pairing.open()
        let attempt = pairing.checking()
        pairing.reached(.failure(RuntimeFailure("refused")), attempt: attempt, relayOnly: true)
        XCTAssertEqual(pairing.phase, .needsSubscription)
    }

    func testAnAnswerToAnAttemptSomebodyLeftIsDropped() {
        let pairing = PairingStore()
        pairing.open()
        let stale = pairing.checking()
        pairing.open()
        pairing.reached(.success(pending), attempt: stale)
        XCTAssertEqual(pairing.phase, .entering)
    }

    func testTurningAMachineAwayStartsTheKeypadAgain() {
        let pairing = PairingStore()
        pairing.open()
        pairing.reached(.success(pending), attempt: pairing.checking())
        pairing.abandoned()
        XCTAssertEqual(pairing.phase, .entering)
        XCTAssertEqual(pairing.digits, "")
    }
}
