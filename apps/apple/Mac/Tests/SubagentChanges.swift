import AmuxMacScreens
import AmuxValues
import AppKit
import SwiftUI
import XCTest

/// The changes pane is the open agent's own folder. A provider subagent has
/// none, so opening one from a parent whose changes are open leaves the
/// pane shut, and going back finds the parent's changes still open.
///
/// The screen is drawn into a window that is never put on screen.
@MainActor
final class SubagentChanges: XCTestCase {
    private let parent = AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: 1, count: 16))
    private let subagent = AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: 2, count: 16))

    override func setUp() async throws {
        NSApplication.shared.setActivationPolicy(.prohibited)
    }

    func testOpeningASubagentLeavesTheChangesShutUntilItsParentIsBack() async throws {
        var place = Place(agent: parent)
        place.changes = true
        let onParent = await inspectorDrawn(place)
        XCTAssertTrue(onParent, "the parent's changes are open")
        XCTAssertTrue(place.showsChanges(native: false))

        place.agent = subagent
        let onSubagent = await inspectorDrawn(place)
        XCTAssertFalse(onSubagent, "a provider subagent has no changes to show")
        XCTAssertFalse(place.showsChanges(native: true))
        XCTAssertTrue(place.changes, "the choice stands for the next agent with a folder")

        place.goBack(where: { _ in true })
        XCTAssertEqual(place.agent, parent)
        let back = await inspectorDrawn(place)
        XCTAssertTrue(back, "back on the parent, its changes are open again")
    }

    /// Whether the main screen at `place` draws its inspector.
    private func inspectorDrawn(_ place: Place) async -> Bool {
        let drawn = Drawn()
        let screen = MainScreen(content: content, place: .constant(place)) {
            Color.clear
        } inspector: {
            Color.clear.onAppear { drawn.inspector = true }
        }
        let size = CGSize(width: 1_400, height: 900)
        let host = NSHostingView(rootView: screen.frame(width: size.width, height: size.height))
        host.frame = CGRect(origin: .zero, size: size)
        let window = NSWindow(contentRect: host.frame, styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        host.layoutSubtreeIfNeeded()
        for _ in 0..<20 where !drawn.inspector {
            try? await Task.sleep(for: .milliseconds(20))
            host.layoutSubtreeIfNeeded()
        }
        window.contentView = nil
        window.close()
        return drawn.inspector
    }

    /// A parent on this Mac and the provider subagent it started.
    private var content: MainContent {
        let rows = [row(parent, "lead", native: false), row(subagent, "survey part 1", native: true)]
        return MainContent(
            profiles: [],
            fleet: FleetContent(sections: [FleetSection(kind: .live, families: 1, foldedByDefault: false, rows: rows)]))
    }

    private func row(_ agent: AgentKey, _ name: String, native: Bool) -> FleetRow {
        FleetRow(
            card: FleetCard(
                agent: agent, name: name, handle: name, kind: .claudeSdk, attention: .working, cwd: "/src/amux",
                phaseSinceMs: 0, host: "this Mac", hostReach: .online(.direct), children: native ? 0 : 1,
                familyAttention: .working, members: native ? 1 : 2, descendants: native ? 0 : 1,
                backgroundJobs: 0, closeHeld: false, filing: .open, topLevel: !native, native: native,
                branch: nil, exitCause: nil, repository: nil),
            depth: native ? 1 : 0, expanded: true, secondLine: .blank, loud: nil)
    }
}

/// What a drawing did, read after it.
@MainActor
private final class Drawn {
    var inspector = false
}
