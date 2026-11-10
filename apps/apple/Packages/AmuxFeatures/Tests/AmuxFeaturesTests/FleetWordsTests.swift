import AmuxCore
import AmuxPush
import AmuxValues
import Foundation
import XCTest

@testable import AmuxFeatures

/// A fleet row's second line and a chat's overview, in the words the
/// terminal uses for the same facts.
final class FleetWordsTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_790_000_000)
    private var nowMs: Int64 { Int64(now.timeIntervalSince1970 * 1_000) }

    private func said(_ line: SecondLine, kind: Kind = .claudeSdk) -> String? {
        FleetWords.secondLine(line, kind: kind, now: now)?.text
    }

    private func asks(_ wants: Wants, more: UInt32 = 0) -> SecondLine {
        .said(.ask(key: "k", wants: wants, why: "", more: more))
    }

    func testAnAskSaysWhatItAsksAndHowManyMoreWait() {
        XCTAssertEqual(said(asks(.run(command: "cargo test\n--all"))), "Wants to run cargo test")
        XCTAssertEqual(
            said(asks(.change(path: "src/a.rs", files: 1, created: true), more: 2)),
            "Wants to create src/a.rs · 2 more")
        XCTAssertEqual(
            said(asks(.change(path: "src/a.rs", files: 1, created: nil))), "Wants to write src/a.rs")
        XCTAssertEqual(said(asks(.plan(title: ""))), "Has a plan for you to decide")
        XCTAssertEqual(said(asks(.question(text: "Which base?"))), "Which base?")
        XCTAssertEqual(
            said(asks(.other(server: "linear", text: "Sign in\nnow"))), "linear: Sign in")
        XCTAssertEqual(
            FleetWords.secondLine(asks(.plan(title: "")), kind: .codex, now: now)?.ink, .ask)
    }

    /// Background jobs lead an idle or working line, as the terminal's row
    /// counts them, and never displace an ask.
    func testBackgroundJobsLeadTheLine() {
        func with(_ line: SecondLine, _ jobs: UInt32) -> String? {
            FleetWords.secondLine(line, kind: .codex, now: now, backgroundJobs: jobs)?.text
        }
        XCTAssertEqual(with(.lastSaid("Started the server."), 1), "1 background job · Started the server.")
        XCTAssertEqual(with(.blank, 3), "3 background jobs running")
        let thinking = Activity(kind: .thinking, sinceMs: 0, elapsedMs: 0, turnElapsedMs: 0)
        XCTAssertEqual(
            with(.step(AmuxValues.ActivityLine(activity: thinking, step: nil)), 2),
            "2 background jobs · \(said(.step(AmuxValues.ActivityLine(activity: thinking, step: nil)))!)")
        XCTAssertEqual(with(asks(.plan(title: "")), 2), "Has a plan for you to decide")
        XCTAssertEqual(with(.lastSaid("Done."), 0), "Done.")
    }

    func testAWorkingAgentNamesItsStep() {
        let running = Activity(kind: .running(key: "s1"), sinceMs: 0, elapsedMs: 0, turnElapsedMs: 0)
        XCTAssertEqual(said(.step(AmuxValues.ActivityLine(activity: running, step: "npm test\nmore"))), "npm test")
        let thinking = Activity(kind: .thinking, sinceMs: 0, elapsedMs: 0, turnElapsedMs: 0)
        XCTAssertEqual(said(.step(AmuxValues.ActivityLine(activity: thinking, step: nil))), "Thinking")
    }

    func testAStuckAgentWarnsAndSaysWhenItCanGoOn() {
        let signedOut = FleetWords.secondLine(
            .said(.signedOut), kind: .codex, now: now)
        XCTAssertEqual(signedOut?.text, "Signed out of Codex")
        XCTAssertEqual(signedOut?.ink, .warning)
        XCTAssertEqual(
            said(.said(.signedOut)), "Signed out of Claude")
        XCTAssertEqual(said(.said(.usageLimit(resetsAtMs: nil))), "Usage limit reached")
        XCTAssertEqual(
            said(.said(.usageLimit(resetsAtMs: nowMs - 1))), "Usage limit reached",
            "a reset already past is not promised")
        XCTAssertTrue(
            said(.said(.usageLimit(resetsAtMs: nowMs + 3_600_000)))?
                .hasPrefix("Usage limit reached · resets ") == true)
        let failed = FleetWords.secondLine(
            .said(.failed(message: "Connection refused\nretry")), kind: .codex, now: now)
        XCTAssertEqual(failed?.text, "Connection refused")
        XCTAssertEqual(failed?.ink, .warning)
    }

    func testWhatTheStateWordAlreadySaysIsNotSaidAgain() {
        XCTAssertNil(said(.exited(.finished)))
        XCTAssertNil(said(.exited(.ended)))
        XCTAssertNil(said(.blank))
        XCTAssertNil(said(.lastSaid("")))
        let failed = FleetWords.secondLine(.exited(.failed("code 1")), kind: .codex, now: now)
        XCTAssertEqual(failed?.text, "Stopped · code 1")
        XCTAssertEqual(failed?.ink, .error)
        XCTAssertEqual(said(.lastSaid("Done: the tests pass.")), "Done: the tests pass.")
    }

    func testTheOverviewWordsUsageAndChanges() {
        let near = UsageWindowView(
            label: .weekly(model: "Fable"), state: .nearLimit,
            resetsAtMs: nowMs + 4 * 86_400_000, usedPercent: 83.6)
        XCTAssertEqual(ChatWords.usageLabel(near.label), "Fable weekly limit")
        XCTAssertEqual(ChatWords.used(near), "84% used")
        XCTAssertEqual(
            ChatWords.usageDetail(near, now: now),
            "near · resets \(now.addingTimeInterval(4 * 86_400).formatted(.dateTime.weekday(.abbreviated)))")
        let unknown = UsageWindowView(label: .fiveHour, state: .unspecified, resetsAtMs: nil, usedPercent: 10)
        XCTAssertNil(ChatWords.usageDetail(unknown, now: now))
        XCTAssertEqual(ChatWords.comparison(.uncommitted, base: "main"), "Uncommitted")
        XCTAssertEqual(ChatWords.comparison(.onBranch, base: "main"), "vs main")
        XCTAssertEqual(ChatWords.counts(added: 25, removed: 8), "+25 \u{2212}8")
        XCTAssertEqual(ChatWords.counts(added: 0, removed: 12), "\u{2212}12")
        XCTAssertEqual(ChatWords.ran(since: nowMs - 14 * 60_000, now: now), "14m")
    }
}
