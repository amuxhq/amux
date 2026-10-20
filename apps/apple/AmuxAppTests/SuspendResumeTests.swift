import AmuxCore
import Foundation
import XCTest

/// The app put away, then ended by the system while it was suspended: the
/// machines the phone held are what it opens on next time. Run in the app's
/// own process on a scratch installation, through the coordinator the app
/// drives and the moment it tells the runtime of.
@MainActor
final class SuspendResumeTests: XCTestCase {
    private var support: URL!

    override func setUp() async throws {
        support = FileManager.default.temporaryDirectory
            .appendingPathComponent("suspend-\(UUID().uuidString)", isDirectory: true)
    }

    override func tearDown() async throws {
        try? FileManager.default.removeItem(at: support)
    }

    private func coordinator() -> RuntimeCoordinator {
        RuntimeCoordinator(
            registry: AccountRegistry(), support: support, deviceName: "Test Phone",
            signedOut: StoreBundle(account: AccountId("signed-out")),
            options: .init(discoveryScope: "suspend-\(UUID().uuidString)", lanBind: "127.0.0.1:0"))
    }

    private func trusts(_ coordinator: RuntimeCoordinator, _ host: [UInt8]) -> Bool {
        coordinator.profile?.hosts().contains { $0.hostId == host && $0.trusted } == true
    }

    private func eventually(
        _ what: String, _ check: @MainActor () -> Bool, file: StaticString = #filePath,
        line: UInt = #line
    ) async {
        let deadline = Date().addingTimeInterval(30)
        while !check() {
            if Date() > deadline {
                XCTFail("never saw \(what)", file: file, line: line)
                return
            }
            try? await Task.sleep(for: .milliseconds(20))
        }
    }

    func testAPhoneSuspendedThenEndedOpensOnTheMachinesItHeld() async throws {
        let first = coordinator()
        first.start()
        guard let runtime = await first.started(), let phone = first.profile else {
            first.stop()
            return XCTFail("the runtime did not start")
        }
        // A machine to hold: another profile of this installation, paired by
        // the link it offers.
        guard case .success(let desk) = await runtime.createProfile(),
              case .success(let link) = await runtime.offerPairing(desk.id),
              case .success(let pending) = await phone.beginPair(.link(link)),
              case .success(let paired) = await phone.confirmPair(pending) else {
            first.stop()
            return XCTFail("the desk did not pair")
        }
        await eventually("the desk trusted") { trusts(first, paired.hostId) }

        let checkpoint = first.directory
            .appendingPathComponent("profiles/\(phone.id)/core.checkpoint")
        try? FileManager.default.removeItem(at: checkpoint)
        await first.suspending()
        XCTAssertTrue(
            FileManager.default.fileExists(atPath: checkpoint.path),
            "the suspend left no checkpoint of the phone's core")
        first.stop()

        let second = coordinator()
        defer { second.stop() }
        second.start()
        guard await second.started() != nil else { return XCTFail("the runtime did not start again") }
        XCTAssertEqual(second.profile?.id, phone.id)
        XCTAssertTrue(trusts(second, paired.hostId), "the relaunched phone forgot the desk")
    }
}
