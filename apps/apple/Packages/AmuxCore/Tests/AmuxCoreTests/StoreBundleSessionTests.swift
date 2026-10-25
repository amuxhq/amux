import Foundation
import XCTest

@testable import AmuxCore

/// A session the bundle opened: it catches up the moment it opens, as the
/// runtime's does once the agent's host answers, unless a test holds it
/// behind, and says when it closed.
final class CaughtUpSession: OpenChat, @unchecked Sendable {
    let id: UInt64
    let agent: AgentKey
    var closed = false
    var current = true
    /// Prompts on their way, as the frame lists them, and their drafts.
    var underway: [SentPrompt] = []
    var drafts: [[UInt8]: Draft] = [:]
    /// What the profile's daemon kept for the agent as the chat opened, and
    /// where the chat keeps its draft from then on.
    var kept: Draft?
    var keeps: (@MainActor (Draft) -> Void)?

    init(id: UInt64, agent: AgentKey) {
        self.id = id
        self.agent = agent
    }

    func close() { closed = true }
    func opening() -> ChatDelta {
        ChatDelta(
            chat: id, rows: [], ask: AskMoved(card: nil), frame: frame(), keys: .reload(["a"]),
            overview: nil, settings: nil)
    }

    /// The frame as it now stands, as the delta that hands it over.
    func moved() -> ChatDelta {
        ChatDelta(
            chat: id, rows: [], ask: nil, frame: frame(), keys: nil, overview: nil, settings: nil)
    }

    func follow(_ following: Bool) {}
    func rows(for keys: [String]) -> [Row] { [] }
    private func frame() -> ChatFrame {
        ChatFrame(
            agent: agent, name: "a", kind: .claudeSdk, phase: .needsYou,
            composer: ComposerView(mode: .send, switchedAway: false, activity: nil), controls: ControlsSummary(effort: nil, mode: nil, model: nil, permission: nil), connection: .live, caughtUp: current,
            hasOlder: false, arrivalsHeld: false, queue: [], underway: underway, refused: [], askInput: nil, context: nil, ended: nil, git: nil,
            signIn: nil, waiting: nil)
    }
    func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> { .failure(RuntimeFailure("no")) }
    func answer(_ ask: String, choice: Int, note: String?) async -> ActOutcome? { nil }
    func answer(_ ask: String, responses: [QuestionResponse]) async -> ActOutcome? { nil }
    func replyInstead(_ ask: String, text: String, soFar: [QuestionResponse]) async -> ActOutcome? { nil }
    func toggleRun(_ member: String) {}
    func answerForm(_ ask: String, values: [FormValue]) async -> ActOutcome? { nil }
    func withdraw(_ input: [UInt8]) async -> ActOutcome? { nil }
    func sendNow(_ input: [UInt8]) async -> ActOutcome? { nil }
    func resend(_ input: [UInt8]) async -> SendOutcome? { nil }
    func discard(_ input: [UInt8]) {}
    func draft(of input: [UInt8]) -> Draft? { drafts[input] }
    func interrupt() async -> ActOutcome? { nil }
    func change(_ setting: SettingChange) async -> ActOutcome? { nil }
    func resume(with draft: Draft) async -> ActOutcome? { nil }
    func pageOlder(_ rows: UInt32) async -> PageOutcome? { nil }
    func putBlob(_ data: Data, name: String, mime: String) async -> Result<BlobRef, RuntimeFailure> {
        .failure(RuntimeFailure("no"))
    }
    func blob(_ hash: [UInt8]) -> Data? { nil }
    func review(_ comparison: Comparison) async -> Result<FrozenReview, RuntimeFailure> { .failure(RuntimeFailure("no")) }
    func openOverview(_ comparison: Comparison) async -> Result<Overview, RuntimeFailure> { .failure(RuntimeFailure("no")) }
    func keptDraft() -> Draft? { kept }
    func keep(_ draft: Draft) { MainActor.assumeIsolated { keeps?(draft) } }
}

@MainActor
final class StoreBundleSessionTests: XCTestCase {
    private var clock = Cards.now
    private var opened: [CaughtUpSession] = []
    private var current = true
    /// The drafts the phone's daemon keeps, by agent, across every bundle a
    /// test opens, as a relaunch finds them; an empty draft is forgotten.
    private var daemon: [AgentKey: Draft] = [:]

    private func bundle(
        account: String = "a",
        acting: (@MainActor (AgentAct, AgentKey) async -> Result<Nothing, RuntimeFailure>)? = nil
    ) -> StoreBundle {
        StoreBundle(
            account: AccountId(account), clock: { [unowned self] in clock },
            sessionRetention: .seconds(300),
            opener: { [unowned self] agent in
                let session = CaughtUpSession(id: UInt64(opened.count + 1), agent: agent)
                session.current = current
                session.kept = daemon[agent]
                session.keeps = { [unowned self] draft in
                    daemon[agent] = draft.parts.isEmpty ? nil : draft
                }
                opened.append(session)
                return session
            },
            acting: acting)
    }

    private func sessions(for agent: AgentKey) -> [CaughtUpSession] { opened.filter { $0.agent == agent } }

    /// The fleet moving again just after the launch's first frame went up,
    /// before the delta held for that frame is applied, is not undone by it.
    func testAFleetMoveAfterTheFirstFrameIsNotOverwrittenByTheOneHeldForIt() async {
        let stores = bundle()
        let fleet = { (name: String) in
            Moved.fleet(FleetDelta(hosts: [Cards.host()], view: Cards.view([Cards.row(1, name)])))
        }
        stores.moved(fleet("first"))
        XCTAssertTrue(stores.fleet.presenting, "the first frame is going up")
        stores.moved(fleet("held"))
        stores.fleet.finishPresenting()
        stores.moved(fleet("newer"))
        for _ in 0..<10 { await Task.yield() }
        XCTAssertEqual(stores.fleet.rows.map(\.name), ["newer"])
    }

    func testAnAgentThatNeedsThePersonOpensNothingUntilItsChatIsShown() throws {
        let stores = bundle()
        let asking = Cards.key(1)
        stores.fleet.show(
            Cards.view([Cards.row(1, "asking", attention: .needsYou), Cards.row(2, "idle"), Cards.row(3, "working", attention: .working)]),
            hosts: [Cards.host()])
        stores.keepSessions()
        XCTAssertTrue(opened.isEmpty, "a row that needs the person opens no session ahead of a tap")

        _ = try stores.chat(asking)
        XCTAssertEqual(opened.map(\.agent), [asking], "its page opens the one session, like any other chat")
    }

    func testAViewedChatStaysForTheRetentionAndThenCloses() throws {
        let stores = bundle()
        let viewed = Cards.key(2)
        stores.fleet.show(Cards.view([Cards.row(2, "idle")]), hosts: [Cards.host()])
        _ = try stores.chat(viewed)
        stores.leave(viewed)

        clock = Cards.now.addingTimeInterval(299)
        stores.keepSessions()
        XCTAssertTrue(stores.holds(viewed), "coming back within the retention is instant")
        XCTAssertFalse(sessions(for: viewed)[0].closed)
        _ = try stores.chat(viewed)
        XCTAssertEqual(sessions(for: viewed).count, 1, "no second session is opened")
        stores.leave(viewed)

        clock = Cards.now.addingTimeInterval(299 + 301)
        stores.keepSessions()
        XCTAssertFalse(stores.holds(viewed))
        XCTAssertTrue(sessions(for: viewed)[0].closed)
    }

    func testAViewedChatThatStillNeedsThePersonClosesAfterTheRetention() throws {
        let stores = bundle()
        let asking = Cards.key(1)
        stores.fleet.show(Cards.view([Cards.row(1, "asking", attention: .needsYou)]), hosts: [Cards.host()])
        _ = try stores.chat(asking)
        stores.leave(asking)

        clock = Cards.now.addingTimeInterval(301)
        stores.keepSessions()
        XCTAssertFalse(stores.holds(asking), "only the shown chat and chats viewed within the retention hold one")
        XCTAssertTrue(sessions(for: asking)[0].closed)
    }

    func testAShownChatIsNeverClosedUnderIt() throws {
        let stores = bundle()
        let shown = Cards.key(2)
        stores.fleet.show(Cards.view([Cards.row(2, "idle")]), hosts: [Cards.host()])
        _ = try stores.chat(shown)
        clock = Cards.now.addingTimeInterval(3600)
        stores.keepSessions()
        XCTAssertTrue(stores.holds(shown))
    }

    func testAChatAPushIsWarmingOutlivesTheFleetReadsUntilItIsCurrent() async {
        current = false
        let stores = bundle()
        let pushed = Cards.key(1)
        stores.fleet.show(Cards.view([Cards.row(1, "asking", attention: .needsYou)]), hosts: [Cards.host()])
        let warming = Task { await stores.warm(pushed, within: .seconds(10)) }
        while opened.isEmpty { await Task.yield() }

        // The fleet is read while the chat is still catching up.
        stores.keepSessions()
        XCTAssertFalse(opened[0].closed, "the chat a push is bringing current keeps its session")

        opened[0].current = true
        stores.moved(.chat(opened[0].moved()))
        let warmed = await warming.value
        XCTAssertTrue(warmed)
        XCTAssertTrue(opened[0].closed, "once current it is kept or closed like any other chat")
    }

    /// A chat holding a prompt for a host that cannot be reached stays open
    /// past its retention, until the prompt is sent: the prompt is in the
    /// kept draft while it is held, and only the open chat takes it out
    /// again once it goes. Closed under it, the chat's draft would keep words
    /// the fleet's session went on to send.
    func testAChatHoldingAPromptStaysOpenUntilItIsSent() throws {
        let stores = bundle(account: "held-\(UUID().uuidString)")
        let agent = Cards.key(2)
        stores.fleet.show(Cards.view([Cards.row(2, "idle")]), hosts: [Cards.host()])
        let model = try stores.chat(agent)
        let session = sessions(for: agent)[0]
        session.drafts[[1]] = Draft(words: "Run the tests.", attachments: [])
        session.underway = [
            SentPrompt(inputId: [1], text: [.text("Run the tests.")], lands: .feed, underway: .held)
        ]
        model.apply(session.moved())
        stores.leave(agent)

        clock = Cards.now.addingTimeInterval(301)
        stores.keepSessions()
        XCTAssertTrue(stores.holds(agent), "a chat holding a prompt stays open")

        session.underway[0].underway = .sending(waiting: false)
        model.apply(session.moved())
        stores.keepSessions()
        XCTAssertFalse(stores.holds(agent), "once the prompt is sent the chat closes as usual")
        XCTAssertNil(daemon[agent], "and the kept draft no longer holds it")
    }

    /// A half-typed message outlives its chat's session: the phone keeps
    /// drafts across the session closing after its retention.
    func testADraftOutlivesItsChatsSessionClosing() throws {
        let stores = bundle(account: "drafts-\(UUID().uuidString)")
        let agent = Cards.key(2)
        stores.fleet.show(Cards.view([Cards.row(2, "idle")]), hosts: [Cards.host()])
        try stores.chat(agent).draft = "half typed, do not lose me"
        stores.leave(agent)
        clock = Cards.now.addingTimeInterval(301)
        stores.keepSessions()
        XCTAssertFalse(stores.holds(agent), "the session closed after its retention")
        XCTAssertEqual(
            try stores.chat(agent).draft, "half typed, do not lose me",
            "the draft went with the chat's session")
    }

    /// A half-typed message outlives the app: closing every chat and opening
    /// the account again, as a relaunch does, gives the draft back.
    func testADraftOutlivesTheAppClosing() throws {
        let account = "relaunch-\(UUID().uuidString)"
        let agent = Cards.key(2)
        let before = bundle(account: account)
        before.fleet.show(Cards.view([Cards.row(2, "idle")]), hosts: [Cards.host()])
        try before.chat(agent).draft = "half typed before the app closed"
        before.closeChats()

        let after = bundle(account: account)
        after.fleet.show(Cards.view([Cards.row(2, "idle")]), hosts: [Cards.host()])
        XCTAssertEqual(
            try after.chat(agent).draft, "half typed before the app closed",
            "the draft was lost when the app was reopened")
    }

    /// A deleted agent's draft goes with it: the daemon forgets it, and the
    /// chat still on screen until its page leaves keeps nothing typed into
    /// it from then on.
    func testADeletedAgentsDraftIsForgotten() async throws {
        let account = "deleted-\(UUID().uuidString)"
        let agent = Cards.key(2)
        let stores = bundle(account: account, acting: { [unowned self] _, deleted in
            daemon[deleted] = nil
            return .success(Nothing())
        })
        stores.fleet.show(Cards.view([Cards.row(2, "idle")]), hosts: [Cards.host()])
        try stores.chat(agent).draft = "for an agent about to go"

        guard case .success = await stores.delete(agent) else { return XCTFail("the delete failed") }
        try stores.chat(agent).draft = "typed as the page left"
        XCTAssertNil(daemon[agent], "the deleted agent's draft is still kept")
        stores.closeChats()

        let after = bundle(account: account)
        after.fleet.show(Cards.view([Cards.row(2, "idle")]), hosts: [Cards.host()])
        XCTAssertEqual(try after.chat(agent).draft, "", "the deleted agent's draft came back")
    }

    /// A delete that fails leaves the agent, and its draft, as they were.
    func testAFailedDeleteKeepsTheDraft() async throws {
        let agent = Cards.key(2)
        let stores = bundle(
            account: "undeleted-\(UUID().uuidString)",
            acting: { _, _ in .failure(RuntimeFailure("desk cannot be reached")) })
        stores.fleet.show(Cards.view([Cards.row(2, "idle")]), hosts: [Cards.host()])
        try stores.chat(agent).draft = "still wanted"

        guard case .failure = await stores.delete(agent) else { return XCTFail("the delete went through") }
        XCTAssertNotNil(daemon[agent], "a failed delete dropped the draft")
    }
}
