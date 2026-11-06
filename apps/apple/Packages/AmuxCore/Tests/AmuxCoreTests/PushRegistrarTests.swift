import AmuxPush
import CryptoKit
import Foundation
import XCTest

@testable import AmuxCore

private typealias Key = Curve25519.KeyAgreement.PrivateKey

/// A keychain in memory, which survives a registrar the way the real one
/// survives a launch.
private final class Keys: PushKeys, @unchecked Sendable {
    private let lock = NSLock()
    private var held: [String: Key] = [:]
    var locked = false

    var count: Int { lock.withLock { held.count } }

    func key(for host: String) throws -> Key? {
        try lock.withLock {
            if locked { throw CocoaError(.fileReadNoPermission) }
            return held[host]
        }
    }

    func makeKey(for host: String) throws -> Key {
        try lock.withLock {
            if locked { throw CocoaError(.fileReadNoPermission) }
            let key = Key()
            held[host] = key
            return key
        }
    }

    func removeKey(for host: String) throws {
        lock.withLock { held[host] = nil }
    }
}

private final class Devices: CloudService, @unchecked Sendable {
    private let lock = NSLock()
    private var refusing = false
    private(set) var registered: [(account: AccountId, host: HostId, device: PushDevice)] = []
    private(set) var removed: [(account: AccountId, host: HostId)] = []

    func refuse(_ refuse: Bool) { lock.withLock { refusing = refuse } }

    func registerDevice(
        _ id: AccountId, host: HostId, device: PushDevice
    ) async throws(CloudError) {
        let refused: Bool = lock.withLock {
            if !refusing { registered.append((id, host, device)) }
            return refusing
        }
        if refused { throw .network("down") }
    }

    func removeDevice(_ id: AccountId, host: HostId) async throws(CloudError) {
        let refused: Bool = lock.withLock {
            if !refusing { removed.append((id, host)) }
            return refusing
        }
        if refused { throw .network("down") }
    }

    func signIn(
        _ intent: SignInIntent, presenting: any WebAuthPresenter
    ) async throws(CloudError) -> SignedInAccount { throw .unauthenticated }
    func handOver(_ id: AccountId) async -> Handover? { nil }
    func forgetSession(_ id: AccountId) async throws {}
    func lend(from lender: @escaping @Sendable (AccountId) async -> String?) async {}
    func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement { .none }
    func recordPurchase(_ id: AccountId, signedTransaction: String) async throws(CloudError) {}
    func requestDeletion(
        _ id: AccountId, confirmedEmail: String
    ) async throws(CloudError) -> DeletionOutcome { throw .unauthenticated }
}

@MainActor
final class PushRegistrarTests: XCTestCase {
    private let keys = Keys()
    private let cloud = Devices()
    /// Everything the runtime was told, in order.
    private var said: [(profile: String, key: [UInt8]?)] = []
    private let token = Data([0x0a, 0xff, 0x01])
    private let ada = AccountId("ada")
    private let host = HostId(UUID(uuidString: "11111111-1111-1111-1111-111111111111")!)
    private let otherHost = HostId(UUID(uuidString: "22222222-2222-2222-2222-222222222222")!)

    /// A registrar as a launch makes one, against what the last one left.
    private func launch(environment: PushDevice.Environment = .production) -> PushRegistrar {
        let hosts = ["ada-profile": host, "bo-profile": otherHost]
        return PushRegistrar(
            keys: keys, cloud: cloud, environment: environment,
            host: { hosts[$0] },
            say: { [weak self] profile, key in
                self?.said.append((profile, key))
                return true
            })
    }

    private func adaAccount(wanted: Bool?) -> [PushAccount] {
        [PushAccount(account: ada, profile: "ada-profile", wanted: wanted)]
    }

    private func registered() async -> PushRegistrar {
        let pushes = launch()
        pushes.accounts(adaAccount(wanted: true))
        pushes.permitted(true)
        pushes.token(token)
        await pushes.settled()
        return pushes
    }

    func testAProAccountWithPermissionGetsAKeySaidAndADeviceRow() async throws {
        _ = await registered()
        let key = try XCTUnwrap(try keys.key(for: host.description))
        XCTAssertEqual(said.map(\.profile), ["ada-profile"], "said once, not per change")
        XCTAssertEqual(said.first?.key, [UInt8](key.publicKey.rawRepresentation))
        XCTAssertEqual(cloud.registered.count, 1)
        XCTAssertEqual(cloud.registered.first?.account, ada)
        XCTAssertEqual(cloud.registered.first?.host, host)
        XCTAssertEqual(cloud.registered.first?.device.token, "0aff01")
        XCTAssertEqual(cloud.registered.first?.device.environment, .production)
    }

    func testTheKeyIsSaidBeforeThereIsATokenAndTheRowFollowsIt() async {
        let pushes = launch()
        pushes.accounts(adaAccount(wanted: true))
        pushes.permitted(true)
        await pushes.settled()
        XCTAssertEqual(said.count, 1)
        XCTAssertTrue(cloud.registered.isEmpty)
        pushes.token(token)
        await pushes.settled()
        XCTAssertEqual(said.count, 1)
        XCTAssertEqual(cloud.registered.count, 1)
    }

    func testANewTokenReplacesTheRow() async {
        let pushes = await registered()
        pushes.token(Data([0x02]))
        await pushes.settled()
        XCTAssertEqual(cloud.registered.map(\.device.token), ["0aff01", "02"])
    }

    func testAFreeAccountGetsNothingAndIsToldNothing() async {
        let pushes = launch()
        pushes.accounts(adaAccount(wanted: false))
        pushes.permitted(true)
        pushes.token(token)
        await pushes.settled()
        XCTAssertEqual(keys.count, 0)
        XCTAssertTrue(said.isEmpty)
        XCTAssertTrue(cloud.registered.isEmpty)
        XCTAssertTrue(cloud.removed.isEmpty, "nothing was registered to take away")
    }

    func testEveryLaunchSaysTheKeyItHoldsAtOnceAndRegistersAgain() async throws {
        _ = await registered()
        let key = try XCTUnwrap(try keys.key(for: host.description))
        said = []
        let pushes = launch()
        // Before iOS has answered, and before anything is known of the
        // account: a host reading this phone's entry now must find the key.
        pushes.accounts(adaAccount(wanted: nil))
        await pushes.settled()
        XCTAssertEqual(said.first?.key, [UInt8](key.publicKey.rawRepresentation))
        XCTAssertEqual(cloud.registered.count, 1, "no token yet this launch")
        pushes.accounts(adaAccount(wanted: true))
        pushes.permitted(true)
        pushes.token(token)
        await pushes.settled()
        XCTAssertEqual(said.count, 1)
        XCTAssertEqual(cloud.registered.count, 2, "a token can change between launches")
        XCTAssertEqual(
            try keys.key(for: host.description)?.rawRepresentation, key.rawRepresentation,
            "a new key would strand every host holding the old one")
    }

    func testAnAccountNotHeardFromYetKeepsWhatItHas() async {
        let pushes = launch()
        pushes.accounts(adaAccount(wanted: nil))
        pushes.permitted(true)
        pushes.token(token)
        await pushes.settled()
        XCTAssertEqual(keys.count, 0, "unknown registers nothing")
        _ = await registered()
        let next = launch()
        next.accounts(adaAccount(wanted: nil))
        next.permitted(true)
        await next.settled()
        XCTAssertEqual(keys.count, 1, "and withdraws nothing")
        XCTAssertTrue(cloud.removed.isEmpty)
    }

    func testPermissionTurnedOffWithdrawsTheKeyAndTheRow() async throws {
        let pushes = await registered()
        pushes.permitted(false)
        await pushes.settled()
        XCTAssertEqual(said.last?.profile, "ada-profile")
        XCTAssertNil(try XCTUnwrap(said.last).key)
        XCTAssertEqual(keys.count, 0)
        XCTAssertEqual(cloud.removed.map(\.host), [host])
    }

    func testPermissionTurnedBackOnStartsAgainWithANewKey() async {
        let pushes = await registered()
        let first = said.first?.key
        pushes.permitted(false)
        await pushes.settled()
        pushes.permitted(true)
        await pushes.settled()
        XCTAssertEqual(keys.count, 1)
        XCTAssertNotNil(said.last?.key)
        XCTAssertNotEqual(said.last?.key, first)
        XCTAssertEqual(cloud.registered.count, 2)
    }

    func testLosingProWithdraws() async {
        let pushes = await registered()
        pushes.accounts(adaAccount(wanted: false))
        await pushes.settled()
        XCTAssertEqual(keys.count, 0)
        XCTAssertEqual(cloud.removed.count, 1)
    }

    func testWithdrawingIsDoneBeforeItReturnsAndStaysDoneUntilTheAccountHasLeft() async {
        let pushes = await registered()
        await pushes.withdraw(ada)
        XCTAssertEqual(keys.count, 0)
        XCTAssertEqual(cloud.removed.count, 1)
        // The list moves for some other reason while the sign-out is under way.
        pushes.accounts(adaAccount(wanted: nil))
        pushes.accounts(adaAccount(wanted: true))
        await pushes.settled()
        XCTAssertEqual(keys.count, 0, "it isn't registered again on its way out")
        pushes.accounts(adaAccount(wanted: false))
        await pushes.settled()
        XCTAssertEqual(cloud.removed.count, 1, "and it is taken away once")
        pushes.accounts(adaAccount(wanted: true))
        await pushes.settled()
        XCTAssertEqual(keys.count, 1, "signing back in registers again")
    }

    func testARowThatCantBeRemovedStillTakesTheKeyAway() async throws {
        let pushes = await registered()
        cloud.refuse(true)
        pushes.permitted(false)
        await pushes.settled()
        XCTAssertEqual(keys.count, 0)
        XCTAssertNil(try XCTUnwrap(said.last).key)
    }

    func testARowThatCouldntBeWrittenIsTriedWhenTheAppComesForward() async {
        cloud.refuse(true)
        let pushes = await registered()
        XCTAssertTrue(cloud.registered.isEmpty)
        cloud.refuse(false)
        // What iOS says hasn't changed, and reading it is still the moment.
        pushes.permitted(true)
        await pushes.settled()
        XCTAssertEqual(cloud.registered.count, 1)
        pushes.permitted(true)
        await pushes.settled()
        XCTAssertEqual(cloud.registered.count, 1, "and a row written isn't written again")
        XCTAssertEqual(said.count, 1)
    }

    func testAKeychainThatCantBeReadChangesNothing() async {
        _ = await registered()
        said = []
        keys.locked = true
        let pushes = launch()
        pushes.accounts(adaAccount(wanted: true))
        pushes.permitted(true)
        await pushes.settled()
        XCTAssertTrue(said.isEmpty)
        keys.locked = false
        XCTAssertEqual(keys.count, 1, "the key is neither replaced nor lost")
    }

    func testEachAccountHasItsOwnKeyAndRow() async throws {
        let pushes = launch()
        pushes.accounts(adaAccount(wanted: true) + [
            PushAccount(account: AccountId("bo"), profile: "bo-profile", wanted: true),
        ])
        pushes.permitted(true)
        pushes.token(token)
        await pushes.settled()
        XCTAssertEqual(keys.count, 2)
        XCTAssertEqual(Set(cloud.registered.map(\.host)), [host, otherHost])
        XCTAssertNotEqual(
            try keys.key(for: host.description)?.rawRepresentation,
            try keys.key(for: otherHost.description)?.rawRepresentation)
    }

    func testAnAccountIsWantedWhenSignedInWithPro() {
        let active = Entitlement.active(grant: .granted, renews: nil)
        let lapsed = Entitlement.lapsed(grant: .granted, endedAt: Date())
        XCTAssertEqual(PushAccount.wanted(signedIn: false, pro: true, entitlement: active), false)
        XCTAssertEqual(PushAccount.wanted(signedIn: true, pro: true, entitlement: .none), true)
        XCTAssertEqual(PushAccount.wanted(signedIn: true, pro: false, entitlement: active), false)
        XCTAssertEqual(PushAccount.wanted(signedIn: true, pro: nil, entitlement: active), true)
        XCTAssertEqual(PushAccount.wanted(signedIn: true, pro: nil, entitlement: lapsed), false)
        XCTAssertNil(PushAccount.wanted(signedIn: true, pro: nil, entitlement: .none))
    }
}

final class SealedPayloadTests: XCTestCase {
    private let host = "11111111-1111-1111-1111-111111111111"
    private let agent = "22222222-2222-2222-2222-222222222222"

    /// Sealed the way a daemon seals. `AmuxPushTests` holds this to what the
    /// daemon really writes; here it only needs to open.
    private func sealed(to key: Key) throws -> String {
        var sender = try HPKE.Sender(
            recipientKey: key.publicKey, ciphersuite: .Curve25519_SHA256_ChachaPoly,
            info: Data("amux push 1".utf8))
        let plain = """
            {"host":"\(host)","agent":"\(agent)","name":"asker","notice":"signed_out"}
            """
        let ciphertext = try sender.seal(Data(plain.utf8))
        return (sender.encapsulatedKey + ciphertext).base64EncodedString()
    }

    func testAWakesRawPayloadIsOpenedInTheApp() throws {
        let keys = Keys()
        let key = try keys.makeKey(for: "phone")
        let payload: [AnyHashable: Any] = [
            "aps": ["content-available": 1],
            "amux": ["to": "phone", "sealed": try sealed(to: key)],
        ]
        let expected = AgentKey(host: HostId(host)!, agent: UUID(uuidString: agent)!)
        XCTAssertEqual(PushPayload.agent(payload, keys: keys), expected)
        XCTAssertNil(PushPayload.agent(payload), "a sealed payload names nobody without its key")
        XCTAssertNil(PushPayload.agent(payload, keys: Keys()), "or with no key for it")
    }

    func testWhatTheExtensionLeavesIsReadWithoutAKey() {
        let payload: [AnyHashable: Any] = ["amux": ["host": host, "agent": agent]]
        let expected = AgentKey(host: HostId(host)!, agent: UUID(uuidString: agent)!)
        XCTAssertEqual(PushPayload.agent(payload), expected)
        XCTAssertEqual(PushPayload.agent(payload, keys: Keys()), expected)
    }
}
