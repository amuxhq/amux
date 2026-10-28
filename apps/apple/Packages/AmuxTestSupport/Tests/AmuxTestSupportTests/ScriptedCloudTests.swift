import AmuxCore
import XCTest
@testable import AmuxTestSupport

final class ScriptedCloudTests: XCTestCase {
    private let ada = AccountId("ada")

    func testItAnswersWhatTheStateSaysAndRecordsWhatItWasAsked() async throws {
        let cloud = ScriptedCloudService(state: ScriptedCloudState(
            entitlement: .active(grant: .purchased(.appStore), renews: ScriptedCloudService.now),
            token: "connect-me",
            deletion: .deleted))

        let account = try await cloud.signIn(.adding, presenting: ScriptedWebAuth())
        XCTAssertEqual(account.email, "ada@example.com")
        let entitlement = try await cloud.entitlement(ada)
        XCTAssertEqual(entitlement, .active(grant: .purchased(.appStore), renews: ScriptedCloudService.now))
        let handed = await cloud.handOver(ada)
        XCTAssertEqual(handed?.refreshToken, "connect-me")
        XCTAssertEqual(handed?.client, "cli", "the scripted relay's identity endpoint is the CLI's")
        let facts = try await cloud.account(ada)
        XCTAssertEqual(facts.entitlement, .active(grant: .purchased(.appStore), renews: ScriptedCloudService.now))
        let deletion = try await cloud.requestDeletion(ada, confirmedEmail: "ada@example.com")
        XCTAssertEqual(deletion, .deleted)

        XCTAssertEqual(cloud.calls, [
            .signIn(.adding),
            .entitlement(ada),
            .handOver(ada),
            .account(ada),
            .requestDeletion(ada, confirmedEmail: "ada@example.com"),
        ])
    }

    func testEveryFailureIsDeclaredRatherThanImprovised() async {
        let cancelled = ScriptedCloudService(state: ScriptedCloudState(signIn: .cancelled))
        await assert(CloudError.cancelled) { try await cancelled.signIn(.adding, presenting: ScriptedWebAuth()) }

        let refused = ScriptedCloudService(state: ScriptedCloudState(signIn: .refused("no such account")))
        await assert(CloudError.refused("no such account")) {
            try await refused.signIn(.adding, presenting: ScriptedWebAuth())
        }

        let offline = ScriptedCloudService(state: ScriptedCloudState(signIn: .offline, token: nil))
        await assert(CloudError.network("offline")) { try await offline.signIn(.adding, presenting: ScriptedWebAuth()) }
        let nothing = await offline.handOver(ada)
        XCTAssertNil(nothing, "no token, nothing to hand over")
    }

    /// Latency is a state, not an accident: a screen that only exists while a
    /// request is in flight cannot be captured unless the test can hold the
    /// request open.
    func testLatencyIsHonoured() async throws {
        let cloud = ScriptedCloudService(state: ScriptedCloudState(latency: .milliseconds(120)))
        let started = ContinuousClock.now
        _ = try await cloud.entitlement(ada)
        XCTAssertGreaterThanOrEqual(ContinuousClock.now - started, .milliseconds(100))
    }

    func testAWebSignInThatIsCancelledNeverReturnsAnAccount() async throws {
        // The app hands off to the browser and is told what came back; it has
        // no credential field of its own to fall back on.
        let cloud = ScriptedCloudService()
        let account = try await cloud.signIn(.adding, presenting: ScriptedWebAuth(outcome: .cancelled))
        XCTAssertEqual(account.id, ScriptedCloudState.ada.id)
        XCTAssertEqual(cloud.calls, [.signIn(.adding)])
    }

    func testTheStateCanBeRewrittenBetweenCalls() async throws {
        let cloud = ScriptedCloudService(state: .unsubscribed)
        let before = try await cloud.entitlement(ada)
        XCTAssertEqual(before, .none)
        cloud.scripted.entitlement = .active(grant: .purchased(.web), renews: nil)
        let after = try await cloud.entitlement(ada)
        XCTAssertEqual(after, .active(grant: .purchased(.web), renews: nil))
        cloud.reset()
        XCTAssertTrue(cloud.calls.isEmpty)
    }

    func testAPurchaseItTakesEntitlesTheAccountAndOneItRefusesDoesNot() async throws {
        let refusing = ScriptedCloudService(
            state: ScriptedCloudState(entitlement: .none, purchase: .refused("not this one")))
        try? await refusing.recordPurchase(ada, signedTransaction: "signed.jws")
        let stillNone = try await refusing.entitlement(ada)
        XCTAssertEqual(stillNone, .none)

        let cloud = ScriptedCloudService(state: .unsubscribed)
        try await cloud.recordPurchase(ada, signedTransaction: "signed.jws")
        let after = try await cloud.entitlement(ada)
        XCTAssertEqual(after, .active(grant: .purchased(.appStore), renews: nil))
    }

    private func assert(
        _ expected: CloudError, _ body: () async throws -> some Sendable,
        file: StaticString = #filePath, line: UInt = #line
    ) async {
        do {
            _ = try await body()
            XCTFail("expected \(expected)", file: file, line: line)
        } catch let error as CloudError {
            XCTAssertEqual(error, expected, file: file, line: line)
        } catch {
            XCTFail("expected \(expected), got \(error)", file: file, line: line)
        }
    }
}
