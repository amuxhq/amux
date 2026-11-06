import CryptoKit
import Foundation
import UserNotifications
import XCTest

@testable import AmuxPush

/// Everything here opens `Fixtures/sealed-pushes.json`, which the daemon's
/// own sealing wrote (`write_the_phones_fixture` in crates/node/src/seal.rs),
/// so these tests and the daemon's are held to one format.
final class SealedPushTests: XCTestCase {
    struct Fixture: Decodable {
        struct Case: Decodable {
            let name: String
            let sealed: String
        }
        let private_key: String
        let cases: [Case]
    }

    private func fixture() throws -> Fixture {
        let url = try XCTUnwrap(
            Bundle.module.url(forResource: "Fixtures/sealed-pushes", withExtension: "json"))
        return try JSONDecoder().decode(Fixture.self, from: Data(contentsOf: url))
    }

    private func key(_ fixture: Fixture) throws -> Curve25519.KeyAgreement.PrivateKey {
        try Curve25519.KeyAgreement.PrivateKey(
            rawRepresentation: try XCTUnwrap(Data(base64Encoded: fixture.private_key)))
    }

    /// The plain JSON each case holds, as the daemon wrote it.
    private func plain() throws -> [String: Any] {
        let url = try XCTUnwrap(
            Bundle.module.url(forResource: "Fixtures/sealed-pushes", withExtension: "json"))
        let whole = try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any]
        let cases = try XCTUnwrap(whole?["cases"] as? [[String: Any]])
        return Dictionary(uniqueKeysWithValues: cases.map { ($0["name"] as! String, $0["plain"]!) })
    }

    func testEveryCaseTheDaemonSealedOpensToTheJSONItWrote() throws {
        let fixture = try fixture()
        let key = try key(fixture)
        let expected = try plain()
        XCTAssertFalse(fixture.cases.isEmpty)
        for sample in fixture.cases {
            let opened = try XCTUnwrap(SealedPush.plaintext(sample.sealed, with: key), sample.name)
            let json = try JSONSerialization.jsonObject(with: opened) as? NSDictionary
            XCTAssertEqual(json, expected[sample.name] as? NSDictionary, sample.name)
        }
    }

    func testEveryCaseDecodes() throws {
        let fixture = try fixture()
        let key = try key(fixture)
        for sample in fixture.cases {
            let opened = try XCTUnwrap(SealedPush.open(sample.sealed, with: key), sample.name)
            XCTAssertEqual(opened.host, "11111111-1111-1111-1111-111111111111", sample.name)
            XCTAssertEqual(opened.agent, "22222222-2222-2222-2222-222222222222", sample.name)
            XCTAssertEqual(opened.name, "refactor-auth", sample.name)
        }
    }

    func testEachNoticeSaysWhatTheFleetRowSays() throws {
        let fixture = try fixture()
        let key = try key(fixture)
        // An hour before the fixture's reset, so the wording doesn't depend
        // on when this runs. The time itself is the phone's zone and format.
        let now = Date(timeIntervalSince1970: 1_800_000_000 - 3_600)
        let reset = Date(timeIntervalSince1970: 1_800_000_000)
            .formatted(date: .omitted, time: .shortened)
        let expected = [
            "run": "Wants to run deploy --prod",
            "run and more": "Wants to run deploy --prod · and 2 more",
            "change one file": "Wants to edit src/lib.rs",
            "change many files": "Wants to edit 4 files",
            "use a tool": "Wants to use github create_issue",
            "question": "Which suite?",
            "plan with a title": "Has a plan for you to decide: Move the cache",
            "plan without one": "Has a plan for you to decide",
            "the provider's sentence": "Sign in to Linear to go on · and 1 more",
            "signed out": "Signed out of its provider · sign in on its host",
            "usage limit with a reset": "Usage limit reached · resets \(reset)",
            "usage limit without one": "Usage limit reached",
            "failed": "Connection refused",
            "died": "Stopped unexpectedly: exited with status 1",
        ]
        XCTAssertEqual(Set(expected.keys), Set(fixture.cases.map(\.name)))
        for sample in fixture.cases {
            let opened = try XCTUnwrap(SealedPush.open(sample.sealed, with: key), sample.name)
            XCTAssertEqual(PushWords.body(opened.notice, now: now), expected[sample.name], sample.name)
        }
    }

    func testAKeyItWasNotSealedToOpensNothing() throws {
        let sample = try XCTUnwrap(try fixture().cases.first)
        XCTAssertNil(SealedPush.open(sample.sealed, with: Curve25519.KeyAgreement.PrivateKey()))
    }

    func testWhatIsNotSealedOpensNothing() throws {
        let key = try key(try fixture())
        XCTAssertNil(SealedPush.open("not base64!", with: key))
        XCTAssertNil(SealedPush.open(Data(repeating: 1, count: 16).base64EncodedString(), with: key))
        XCTAssertNil(SealedPush.open(Data(repeating: 1, count: 80).base64EncodedString(), with: key))
    }

    func testAPayloadOpensWithTheKeyOfTheProfileItNames() throws {
        let fixture = try fixture()
        let keys = MemoryKeys(["11111111-aaaa-1111-1111-111111111111": try key(fixture)])
        let sealed = try XCTUnwrap(fixture.cases.first).sealed
        func payload(to: String) -> [AnyHashable: Any] {
            ["aps": [:], "amux": ["to": to, "sealed": sealed]]
        }
        XCTAssertNotNil(
            SealedPush.open(payload(to: "11111111-AAAA-1111-1111-111111111111"), keys: keys),
            "a host id's case is not part of it")
        XCTAssertNil(SealedPush.open(payload(to: "33333333-3333-3333-3333-333333333333"), keys: keys))
        XCTAssertNil(SealedPush.open(["aps": [:]], keys: keys))
    }

    func testAKeyThatCantBeReadLeavesThePushClosed() throws {
        let sealed = try XCTUnwrap(try fixture().cases.first).sealed
        let payload: [AnyHashable: Any] = ["amux": ["to": "any", "sealed": sealed]]
        XCTAssertNil(SealedPush.open(payload, keys: LockedKeys()))
    }
}

final class PushAlertTests: XCTestCase {
    private let opened = OpenedPush(
        host: "11111111-1111-1111-1111-111111111111",
        agent: "22222222-2222-2222-2222-222222222222", name: "refactor-auth",
        notice: .ask(wants: .run(command: "deploy"), why: "", more: 0))

    func testAnOpenedPushBecomesTheAgentsNameOverWhatItWants() {
        let content = UNMutableNotificationContent()
        content.title = "amux"
        content.body = "An agent needs you"
        content.userInfo = ["amux": ["to": "x", "sealed": "y"], "aps": ["mutable-content": 1]]
        PushAlert.write(opened, into: content)
        XCTAssertEqual(content.title, "refactor-auth")
        XCTAssertEqual(content.body, "Wants to run deploy")
        XCTAssertEqual(
            content.threadIdentifier,
            "11111111-1111-1111-1111-111111111111/22222222-2222-2222-2222-222222222222")
        let amux = content.userInfo["amux"] as? [String: String]
        XCTAssertEqual(amux?["host"], opened.host, "the tap reads these")
        XCTAssertEqual(amux?["agent"], opened.agent)
        XCTAssertNil(amux?["sealed"], "what it said is written out, not carried beside it")
    }

    func testAnAgentWithNoNameKeepsTheTitleItHad() {
        let content = UNMutableNotificationContent()
        content.title = "amux"
        var nameless = opened
        nameless.name = ""
        PushAlert.write(nameless, into: content)
        XCTAssertEqual(content.title, "amux")
    }
}

final class PushWordsTests: XCTestCase {
    func testAnAskWithNothingToSayStillSaysItNeedsYou() {
        XCTAssertEqual(
            PushWords.body(.ask(wants: .other(text: ""), why: "", more: 0)), "Needs you")
        XCTAssertEqual(
            PushWords.body(.ask(wants: .run(command: "ls"), why: "", more: 0)), "Wants to run ls")
    }

    func testALimitAlreadyResetSaysNoTime() {
        let now = Date(timeIntervalSince1970: 1_000)
        XCTAssertEqual(PushWords.body(.usageLimit(resetsAtMs: 500_000), now: now), "Usage limit reached")
    }

    func testALimitDaysAwaySaysItsWeekday() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let later = now.addingTimeInterval(3 * 86_400)
        let body = PushWords.body(
            .usageLimit(resetsAtMs: Int64(later.timeIntervalSince1970 * 1000)), now: now)
        XCTAssertEqual(
            body, "Usage limit reached · resets \(later.formatted(.dateTime.weekday(.abbreviated)))")
    }

    func testAFailureAndADeathWithNoWordsStillSaySomething() {
        XCTAssertEqual(PushWords.body(.failed(message: "")), "Its turn failed")
        XCTAssertEqual(PushWords.body(.died(detail: "")), "Stopped unexpectedly")
    }
}

private struct MemoryKeys: PushKeys {
    let held: [String: Curve25519.KeyAgreement.PrivateKey]

    init(_ held: [String: Curve25519.KeyAgreement.PrivateKey]) { self.held = held }

    func key(for host: String) -> Curve25519.KeyAgreement.PrivateKey? { held[host.lowercased()] }
    func makeKey(for host: String) -> Curve25519.KeyAgreement.PrivateKey { .init() }
    func removeKey(for host: String) {}
}

private struct LockedKeys: PushKeys {
    func key(for host: String) throws -> Curve25519.KeyAgreement.PrivateKey? {
        throw PushKeyFailure(status: errSecInteractionNotAllowed)
    }
    func makeKey(for host: String) throws -> Curve25519.KeyAgreement.PrivateKey {
        throw PushKeyFailure(status: errSecInteractionNotAllowed)
    }
    func removeKey(for host: String) throws {}
}
