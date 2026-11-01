import Foundation
import XCTest

@testable import AmuxCore

/// The phone's network changing reaches the runtime: the coordinator
/// watches only while a runtime runs, and feeds it one change for each the
/// watch tells.
@MainActor
final class NetworkWatchTests: XCTestCase {
    private var support: URL!

    override func setUp() async throws {
        support = FileManager.default.temporaryDirectory
            .appendingPathComponent("network-\(UUID().uuidString)", isDirectory: true)
    }

    override func tearDown() async throws {
        try? FileManager.default.removeItem(at: support)
    }

    private func changes(_ coordinator: RuntimeCoordinator) -> Int {
        coordinator.fedLifecycles?.filter { $0 == .networkChanged }.count ?? 0
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

    func testEveryChangeWhileTheRuntimeRunsIsFedToItOnce() async {
        let watch = FakeNetworkWatch()
        let coordinator = RuntimeCoordinator(
            registry: AccountRegistry(), support: support, deviceName: "Test Phone",
            signedOut: StoreBundle(account: AccountId("signed-out")),
            options: .init(discoveryScope: "network-\(UUID().uuidString)", lanBind: "127.0.0.1:0"),
            networkWatch: watch)
        defer { coordinator.stop() }
        XCTAssertNotNil(coordinator.fedLifecycles, "a test build keeps what was fed")

        // Before the runtime starts nothing watches, and a change is lost.
        XCTAssertFalse(watch.started)
        watch.change()
        coordinator.start()
        let runtime = await coordinator.started()
        XCTAssertNotNil(runtime)
        XCTAssertTrue(watch.started, "the watch starts with the runtime")
        try? await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(changes(coordinator), 0, "nothing was fed before the runtime started")

        watch.change()
        await eventually("the first change fed") { self.changes(coordinator) == 1 }
        watch.change()
        watch.change()
        await eventually("each change fed") { self.changes(coordinator) == 3 }
        try? await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(changes(coordinator), 3, "once per change")

        // The runtime took each: feeding one more answers as fed.
        if let runtime {
            let fed = await runtime.feed(.networkChanged)
            if case .failure(let why) = fed { XCTFail("the runtime refused the change: \(why)") }
        }

        coordinator.stop()
        XCTAssertFalse(watch.started, "the watch stops with the runtime")
        watch.change()
        try? await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(changes(coordinator), 3, "nothing is fed once it stopped")
    }
}

/// A watch a test drives by hand: each `change()` while started is one
/// change, told at once on the caller's thread.
final class FakeNetworkWatch: NetworkPathWatching, @unchecked Sendable {
    private let lock = NSLock()
    private var onChange: (@Sendable () -> Void)?

    /// Whether the coordinator is watching.
    var started: Bool { lock.withLock { onChange != nil } }

    func start(_ onChange: @escaping @Sendable () -> Void) {
        lock.withLock { self.onChange = onChange }
    }

    func stop() {
        lock.withLock { onChange = nil }
    }

    /// The network changed; nothing hears it unless the watch is started.
    func change() {
        let told = lock.withLock { onChange }
        told?()
    }
}
