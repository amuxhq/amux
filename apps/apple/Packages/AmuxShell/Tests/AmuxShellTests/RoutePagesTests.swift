import AmuxCore
import AmuxFeatures
import SwiftUI
import UIKit
import XCTest

@testable import AmuxShell

/// Every route the shell can push opens a page with something on it. A
/// host's row once pushed a placeholder that drew only the word "host",
/// with no working way back.
@MainActor
final class RoutePagesTests: XCTestCase {
    private let machine = HostId(UUID(uuidString: "00000000-0000-0000-0000-00000000000A")!)

    /// One of every route. The switch has no default, so a new route fails
    /// to compile here until it is listed, and so tested.
    private func every() -> [Route] {
        let agent = AgentKey(host: machine, agent: UUID())
        let invitation = PairingInvitation(
            host: machine, cloudURL: nil, addrs: ["192.168.1.2:4433"], secret: [1, 2, 3], payload: "{}")
        // Signing in and subscribing are pushed from Agents here: pushed from
        // Settings they open inside its sheet, a presentation of its own whose
        // elements never reach the root this test reads. The pages are the
        // same either way.
        let routes: [Route] = [
            .conversation(agent), .changes(agent), .newAgent, .pairByCode(machine),
            .pairConfirmation(invitation), .host(machine), .signIn(.agents), .paywall(.agents),
        ]
        for route in routes {
            switch route {
            case .conversation, .changes, .newAgent, .pairByCode, .pairConfirmation, .host, .signIn,
                 .paywall:
                break
            }
        }
        return routes
    }

    func testEveryPushableRouteOpensABuiltPage() async throws {
        for route in every() {
            let router = Router()
            var drawn: [IdentifiedElement] = []
            let shell = Shell(
                router: router, accounts: AccountRegistry(), stores: StoreBundle(account: AccountId("pages")),
                signIn: SignInStore(), paywall: PaywallStore(), deletion: DeletionStore()
            ) { _ in }
            let root = shell
                .reportingIdentifiedElements()
                .onPreferenceChange(IdentifiedElements.self) { elements in
                    MainActor.assumeIsolated { drawn = elements }
                }
            let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 402, height: 874))
            window.rootViewController = UIHostingController(rootView: root)
            window.isHidden = false
            router.select(route.tab)
            try await Task.sleep(for: .milliseconds(300))
            let before = Set(drawn.map(\.identifier))
            router.open(route)
            try await Task.sleep(for: .milliseconds(800))
            let opened = Set(drawn.map(\.identifier)).subtracting(before)
            XCTAssertFalse(
                opened.contains { $0.hasPrefix("page.") },
                "\(route.name) opened a placeholder page: \(opened.sorted())")
            XCTAssertFalse(opened.isEmpty, "\(route.name) opened a page with nothing on it")
            window.isHidden = true
            window.rootViewController = nil
        }
    }
}
