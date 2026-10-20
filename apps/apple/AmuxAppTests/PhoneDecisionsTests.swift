import AmuxCore
import AmuxFeatures
import SwiftUI
import UIKit
import XCTest
@testable import Amux

/// Screens as a person meets them, drawn whole in a window: what the chat's
/// menus offer, what its buttons say, what a stop warns of, and how an ask
/// card, the home screen and the new-agent form read in the states QA found
/// them wrong.
@MainActor
final class PhoneDecisionsTests: XCTestCase {
    private var window: UIWindow?
    private let probe = Probe()
    private var acted: [ChatAction] = []

    @MainActor
    private final class Probe {
        var elements: [IdentifiedElement] = []
    }

    override func tearDown() async throws {
        window?.isHidden = true
        window?.rootViewController = nil
        window = nil
    }

    private func host<Content: View>(_ content: Content, size: DynamicTypeSize = .large) throws {
        let probe = self.probe
        let root = content
            .dynamicTypeSize(size)
            .reportingIdentifiedElements()
            .onPreferenceChange(IdentifiedElements.self) { declared in
                MainActor.assumeIsolated { probe.elements = declared }
            }
        let controller = UIHostingController(rootView: root)
        let scene = try XCTUnwrap(
            UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: 402, height: 874)
        window.rootViewController = controller
        window.makeKeyAndVisible()
        self.window = window
    }

    private func chat(
        _ source: ScriptedChat, family: FamilyHeader? = nil, size: DynamicTypeSize = .large
    ) async throws -> ChatModel {
        let model = ChatModel(source: source, loadingHintAfter: .zero)
        // In a navigation stack, as the app pushes it: the chat's menu and
        // title are the system bar's, which draws only inside one.
        try host(
            NavigationStack {
                ChatScreen(model: model, subject: CatalogFixtures.subject, family: family) { [unowned self] action in
                    acted.append(action)
                }
            }, size: size)
        await spin()
        return model
    }

    private func spin(_ seconds: Double = 0.5) async {
        try? await Task.sleep(for: .seconds(seconds))
    }

    private func element(_ identifier: String) -> IdentifiedElement? {
        probe.elements.first { $0.identifier == identifier }
    }

    /// Builds the screen's accessibility tree, which a SwiftUI screen only
    /// keeps while something reads it: the debug door's driver does, and a
    /// test turns on the same automation reading for itself alone.
    private func accessible() async throws {
        let handle = try XCTUnwrap(dlopen("/usr/lib/libAccessibility.dylib", RTLD_NOW))
        let symbol = try XCTUnwrap(dlsym(handle, "_AXSSetAutomationEnabled"))
        let automation = unsafeBitCast(symbol, to: (@convention(c) (Int32) -> Void).self)
        automation(1)
        addTeardownBlock { automation(0) }
        await spin()
    }

    /// Acts on a control the way VoiceOver does, which is the one way to
    /// press a SwiftUI button from inside the process.
    private func activate(_ identifier: String) throws {
        let window = try XCTUnwrap(window)
        let declared = try XCTUnwrap(element(identifier), "\(identifier) is not drawn")
        let control = VisibleTree.find(identifier, in: window)
            ?? VisibleTree.element(
                at: CGPoint(x: declared.frame.midX, y: declared.frame.midY), in: window, saying: declared.label)
        XCTAssertTrue(control?.accessibilityActivate() ?? false, "\(identifier) did not activate")
    }

    private func menuItem(_ title: String) throws -> (UIButton, UIAction)? {
        MenuSource.item(title, in: try XCTUnwrap(window))
    }

    private var oneChild: FamilyHeader {
        FamilyHeader(
            children: [CatalogFixtures.family.children[1]], attention: .working, needYou: 0, parent: nil)
    }

    // MARK: - Stop, delete, interrupt

    /// A live agent is stopped, not deleted; only an exited one is deleted.
    func testALiveAgentOffersStopAndOnlyAnExitedOneOffersDelete() async throws {
        _ = try await chat(ScriptedChat(rows: CatalogFixtures.conversation, frame: ScriptedChat.frame(phase: .working)))
        XCTAssertNotNil(try menuItem("Stop Agent"), "a live agent's menu offers Stop Agent")
        XCTAssertNil(try menuItem("Delete Agent"), "a live agent's menu offers Delete Agent; only an exited agent may be deleted")

        tearDownWindow()
        _ = try await chat(ScriptedChat(
            rows: CatalogFixtures.conversation, frame: ScriptedChat.frame(phase: .exited(cause: .ended))))
        XCTAssertNotNil(try menuItem("Delete Agent"), "an exited agent's menu offers Delete Agent")
        XCTAssertNil(try menuItem("Stop Agent"), "an exited agent has nothing to stop")
    }

    /// The square button under a working turn ends the turn, not the agent,
    /// and says so in words.
    func testTheComposersTurnButtonSaysInterrupt() async throws {
        _ = try await chat(ScriptedChat(rows: CatalogFixtures.conversation, frame: ScriptedChat.frame(phase: .working)))
        let button = try XCTUnwrap(element("chat.stop"), "a working chat with no draft shows its square button")
        XCTAssertEqual(button.label, "Interrupt", "the button that ends the turn reads \(String(reflecting: button.label))")
    }

    /// The ask card's way of ending the turn is called Interrupt too.
    func testTheAskCardsTurnButtonSaysInterrupt() async throws {
        _ = try await chat(ScriptedChat(
            rows: CatalogFixtures.conversation + [CatalogFixtures.askedCommand],
            frame: ScriptedChat.frame(phase: .needsYou),
            card: CatalogFixtures.card(
                .claudeSdk, .command(command: "cargo test -p amux-ui", cwd: "~/src/amux", reason: "", description: "Run the spec suite"),
                CatalogFixtures.sdkPermissionChoices)))
        XCTAssertNil(try menuItem("Stop the turn"), "the ask card still offers \"Stop the turn\"")
        XCTAssertNotNil(try menuItem("Interrupt"), "the ask card offers Interrupt to end the turn")
    }

    /// Stopping is always "stop now"; when it cuts work, the confirm says
    /// what: the turn under way, its background jobs and its sub-agents.
    func testStoppingAWorkingAgentNamesWhatTheStopCuts() async throws {
        _ = try await chat(
            ScriptedChat(
                rows: CatalogFixtures.conversation, frame: ScriptedChat.frame(name: "desk-1", phase: .working),
                strip: ScriptedChat.strip(background: 2)),
            family: oneChild)
        let (button, stop) = try XCTUnwrap(try menuItem("Stop Agent"))
        MenuSource.run(stop, from: button)
        await spin()
        XCTAssertFalse(acted.contains(.stopAgent), "the agent was stopped with no confirm naming what it cut")
        let said = probe.elements.map { [$0.label, $0.value].compactMap { $0 }.joined(separator: " ") }
            .joined(separator: " | ")
        XCTAssertTrue(
            said.contains("working") && said.contains("2 background jobs") && said.contains("1 sub-agent"),
            "the stop confirm names the turn, its 2 background jobs and its sub-agent: \(said)")
    }

    /// Terminal Claude's own view resumed another conversation: the chat
    /// says so above the composer and offers the way back, at a large text
    /// size too.
    func testAChatWhoseClaudeViewSwitchedAwaySaysSoAndOffersSwitchBack() async throws {
        _ = try await chat(
            ScriptedChat(
                rows: CatalogFixtures.conversation,
                frame: ScriptedChat.frame(kind: .claudePty, phase: .idle, switchedAway: true)),
            size: .accessibility3)
        let notice = try XCTUnwrap(element("chat.switchedAway"), "the chat does not say its conversation left")
        XCTAssertEqual(notice.label, "Claude’s own view switched to another conversation")
        let back = try XCTUnwrap(element("chat.switchBack"), "the chat offers no way back")
        XCTAssertTrue(
            try XCTUnwrap(window).bounds.contains(back.frame),
            "Switch Back is off screen at accessibility3: \(back.frame)")
    }

    /// A prompt held while its host is away is drawn pending, with Cancel.
    func testAHeldPromptIsDrawnPendingWithCancel() async throws {
        let held = SentPrompt(inputId: [7], text: [.text("Run the tests.")], lands: .queue, underway: .held)
        _ = try await chat(ScriptedChat(
            rows: CatalogFixtures.conversation,
            frame: ScriptedChat.frame(phase: .idle, underway: [held])))
        let row = try XCTUnwrap(element("chat.underway.0"), "the held prompt is not drawn")
        XCTAssertTrue((row.value ?? "").contains("sends when"), "the held prompt says \(row.value ?? "")")
        XCTAssertNotNil(element("chat.underway.0.cancel"), "the held prompt has no Cancel")
    }

    // MARK: - Ask cards

    /// At a large text size a long question card squeezed into the space
    /// left overprints its options and pushes Send and Reply instead off
    /// screen, so a person cannot answer it. The card must fit the screen,
    /// every option stay clear of the question and of each other, and the
    /// ways to answer stay on screen.
    func testALongQuestionAtALargeTextSizeCanStillBeReadAndAnswered() async throws {
        let options = [
            ("Managed PostgreSQL", "Hosted by the cloud provider, with point-in-time recovery and read replicas across zones"),
            ("SQLite embedded", "One file beside the binary, no server to run, replicated by shipping the write-ahead log"),
            ("MongoDB Atlas", "Document store with a hosted control plane and automatic sharding once the standby copies are"),
            ("Self-hosted MySQL", "Runs on our own machines with the operational burden of backups, upgrades and failover"),
        ].map { OptionView(label: $0.0, description: $0.1, preview: "", recommended: false) }
        let question = QuestionView(
            header: "Database",
            question: "Which database should the service use for its durable state, given that we need replication, "
                + "point-in-time recovery and something the team already knows how to operate?",
            multiSelect: false, options: options, allowOther: true, secret: false)
        _ = try await chat(
            ScriptedChat(
                rows: CatalogFixtures.conversation, frame: ScriptedChat.frame(phase: .needsYou),
                card: CatalogFixtures.card(.claudeSdk, .question([question]), [])),
            size: .accessibility3)
        let bounds = try XCTUnwrap(window).bounds
        let asked = try XCTUnwrap(element("ask.question"), "the card shows its question")
        let card = try XCTUnwrap(element("ask"), "the card is drawn")
        XCTAssertTrue(bounds.contains(card.frame), "the card is taller than the screen at accessibility3: \(card.frame)")
        let drawn = probe.elements.filter { $0.identifier.hasPrefix("ask.option.") }
        XCTAssertEqual(drawn.count, options.count + 1, "every option and Something else is drawn")
        for (index, option) in drawn.enumerated() {
            XCTAssertFalse(
                option.frame.intersects(asked.frame),
                "\(option.identifier) (\(option.label ?? "")) is drawn over the question at accessibility3")
            for other in drawn.dropFirst(index + 1) where option.frame.intersects(other.frame) {
                XCTFail("\(option.identifier) and \(other.identifier) are drawn over each other at accessibility3")
            }
        }
        for way in ["ask.skip", "ask.reply"] {
            let control = try XCTUnwrap(element(way), "the card offers \(way)")
            XCTAssertTrue(
                bounds.contains(control.frame),
                "\(way) is off screen at accessibility3, so the question cannot be answered: \(control.frame)")
        }
    }

    /// A question whose options carry previews shows one before anything is
    /// picked: a phone has no hover, and the terminal shows the first
    /// option's preview from the start.
    func testAQuestionWithPreviewsShowsOneBeforeAnythingIsPicked() async throws {
        _ = try await chat(ScriptedChat(
            rows: CatalogFixtures.conversation, frame: ScriptedChat.frame(phase: .needsYou),
            card: CatalogFixtures.card(.claudeSdk, .question([CatalogFixtures.previewQuestion]), [])))
        XCTAssertNotNil(element("ask.question"), "the card shows its question")
        XCTAssertNotNil(element("ask.preview"), "a question whose options carry previews shows none until one is picked")
    }

    /// A multi-select carries the options ticked and the words typed as
    /// "Something else" together: typing does not untick anything.
    func testAMultiSelectSendsItsTicksAndItsOwnWordsTogether() async throws {
        var sent: [AskAction] = []
        try host(AskCardView(card: CatalogFixtures.card(.claudeSdk, .question([CatalogFixtures.platformQuestion]), [])) {
            sent.append($0)
        })
        await spin()
        try await accessible()
        try activate("ask.option.0")
        try activate("ask.option.2")
        try activate("ask.option.other")
        await spin()
        let window = try XCTUnwrap(window)
        let field = try XCTUnwrap(
            DoorWindow.allTextViews(in: window).first as? (any UIKeyInput & UIResponder),
            "Something else opens no field")
        _ = field.becomeFirstResponder()
        field.insertText("FreeBSD")
        await spin()
        try activate("ask.next")
        await spin()
        XCTAssertEqual(
            sent, [.respond([QuestionResponse(note: nil, other: "FreeBSD", picked: [0, 2])])],
            "the ticks and the typed words do not both reach the answer")
    }

    /// An agent writes file names in its questions as code, between
    /// backticks. The card draws them as code, as the chat does, and never
    /// shows the backticks.
    func testAQuestionsCodeIsDrawnAsCodeWithoutItsBackticks() async throws {
        let question = QuestionView(
            header: "Exact target",
            question: "`q1/README.md` is absent; `../plan1/README.md` has two lines. Which should be used?",
            multiSelect: false,
            options: [
                OptionView(
                    label: "Create `README.md`", description: "Create the missing file with exactly one line: `UPDATED`.",
                    preview: "", recommended: true),
                OptionView(label: "Edit the other", description: "Change `../plan1/README.md` instead.", preview: "", recommended: false),
            ], allowOther: true, secret: false)
        try host(AskCardView(card: CatalogFixtures.card(.codex, .question([question]), [])) { _ in })
        await spin()
        try await accessible()
        let asked = try XCTUnwrap(element("ask.question"), "the card shows its question")
        XCTAssertEqual(
            asked.label, "q1/README.md is absent; ../plan1/README.md has two lines. Which should be used?")
        let said = VisibleTree.elements(of: try XCTUnwrap(window))
            .compactMap { [$0.label, $0.value].compactMap { $0 }.joined(separator: " ") }
            + probe.elements.compactMap(\.label)
        for text in said {
            XCTAssertFalse(text.contains("`"), "the card says \(String(reflecting: text))")
        }
        XCTAssertTrue(said.contains { $0.contains("Create README.md") }, "the first option is not drawn: \(said)")
    }

    /// A switch on an ask card is drawn whole, its rounded right end inside
    /// the card: the list the card's choices scroll in cut it flat.
    func testAnAskCardsSwitchesAreDrawnWhole() async throws {
        let plan = CatalogFixtures.card(.claudePty, .plan(body: CatalogFixtures.planBody, title: CatalogFixtures.planTitle), [
            Choice(outcome: .approvePlan(mode: "acceptEdits", modeName: "Accept edits"), primary: true, takesNote: false),
            Choice(outcome: .approvePlan(mode: "", modeName: ""), primary: false, takesNote: false),
            Choice(outcome: .sendBack, primary: false, takesNote: true),
        ])
        let form = CatalogFixtures.card(.claudeSdk, .form(
            server: "github", message: "Create the issue in which repository?", fields: CatalogFixtures.formFields), [
                Choice(outcome: .submit, primary: true, takesNote: false),
                Choice(outcome: .decline, primary: false, takesNote: false),
            ])
        for (name, card) in [("plan", plan), ("form", form)] {
            for size in [DynamicTypeSize.large, .xxxLarge] {
                try host(AskCardView(card: card) { _ in }.padding(.horizontal, 16), size: size)
                await spin()
                let window = try XCTUnwrap(window)
                let switches = Self.views(in: window).compactMap { $0 as? UISwitch }
                XCTAssertFalse(switches.isEmpty, "the \(name) card draws no switch")
                for control in switches {
                    let drawn = control.convert(control.bounds, to: window)
                    var above = control.superview
                    while let view = above {
                        if view.clipsToBounds {
                            let shown = view.convert(view.bounds, to: window)
                            XCTAssertTrue(
                                shown.contains(drawn),
                                "the \(name) card's switch at \(size) is cut: drawn \(drawn), shown \(shown)")
                        }
                        above = view.superview
                    }
                }
                tearDownWindow()
            }
        }
    }

    private static func views(in view: UIView) -> [UIView] {
        [view] + view.subviews.flatMap { views(in: $0) }
    }

    // MARK: - Hosts

    /// A host that dropped this phone is offered again from its page, and
    /// its page says why it is out of reach in words, not as the dial's raw
    /// error with an address and a code.
    func testAHostThatNoLongerTrustsThisPhoneOffersPairingAgainInWords() async throws {
        let desk = HostId(UUID(uuidString: "00000000-0000-0000-0000-00000000de5c")!)
        let hosts = HostsStore()
        hosts.show([HostView(
            hostId: desk.bytes, name: "desk", local: false, trusted: true, candidate: false,
            reach: .offline(.revoked), addrs: ["127.0.0.1:55186"], current: false, providers: [],
            lastDialError: "127.0.0.1:55186: code: 'The service is currently unavailable', message: \"connection lost\"",
            platform: "macOS", signedIn: nil, version: nil)])
        var asked: [HostPageAction] = []
        try host(HostPage(model: hosts, host: desk) { asked.append($0) })
        await spin()
        for element in probe.elements {
            let said = [element.label, element.value].compactMap { $0 }.joined(separator: " ")
            XCTAssertFalse(
                said.contains("code:") || said.contains("55186:"),
                "\(element.identifier) shows the raw dial error: \(said)")
        }
        let again = try XCTUnwrap(element("host.pairAgain"), "the page offers no way to pair again")
        XCTAssertEqual(again.label, "Pair Again")
        try await accessible()
        try activate("host.pairAgain")
        await spin()
        XCTAssertEqual(asked, [.pairAgain(desk)])
    }

    // MARK: - Home and new agent

    /// An idle agent whose only work is in the background says how many
    /// jobs run there on its home row, as the terminal's row does.
    func testAHomeRowCountsItsBackgroundJobs() async throws {
        let card = FleetCard(
            agent: AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: 9, count: 16)),
            name: "cx", kind: .codex, attention: .idle, cwd: "~/src", phaseSinceMs: 0,
            host: "desk", hostReach: .online(.direct), children: 0, familyAttention: .idle,
            members: 1, descendants: 0, backgroundJobs: 2, stopHeld: false, branch: nil, exitCause: nil)
        let fleet = FleetStore()
        fleet.show(
            FleetView(
                sections: [FleetSection(kind: .live, families: 1, foldedByDefault: false, rows: [
                    FleetRow(card: card, depth: 0, expanded: false, secondLine: .lastSaid("Started the dev server."), loud: nil),
                ])],
                needYou: 0, working: 0),
            hosts: [])
        try host(AgentsHome(model: fleet, accounts: AccountRegistry(), hosts: HostsStore()) { _ in })
        await spin()
        let row = try XCTUnwrap(
            probe.elements.first { $0.identifier.hasPrefix("home.row.") }, "the agent's row is not drawn")
        XCTAssertTrue(
            (row.label ?? "").contains("2 background jobs · Started the dev server."),
            "the row does not count its background jobs: \(row.label ?? "")")
    }

    /// A reopened app draws its home before its saved hosts are read; it must
    /// not say nothing is paired until it knows.
    func testHomeSaysNothingIsPairedOnlyOnceItHasReadItsHosts() async throws {
        let fleet = FleetStore()
        try host(AgentsHome(model: fleet, accounts: AccountRegistry(), hosts: HostsStore()) { _ in })
        await spin()
        XCTAssertNil(
            element("home.empty.firstRun"),
            "before its hosts are read, home says \"No agents yet\" and offers to pair")
        fleet.show(FleetView(sections: [], needYou: 0, working: 0), hosts: [])
        await spin()
        XCTAssertNotNil(element("home.empty.firstRun"), "with no hosts read, home invites pairing")
    }

    /// "desk cannot be reached" from a refused start goes once desk is back.
    func testTheNewAgentFormDropsAReachRefusalOnceTheHostIsBack() async throws {
        let desk = HostId(UUID(uuidString: "00000000-0000-0000-0000-00000000de5c")!)
        func view(_ reach: Reach) -> HostView {
            HostView(
                hostId: desk.bytes, name: "desk", local: false, trusted: true, candidate: false, reach: reach,
                addrs: [], current: true, providers: [], lastDialError: nil, platform: "macOS",
                signedIn: nil, version: nil)
        }
        let hosts = HostsStore()
        hosts.show([view(.offline(.plain))])
        let form = NewAgentStore()
        form.open(on: desk)
        form.starts()
        form.started(.failure(RuntimeFailure("desk cannot be reached")))
        try host(NewAgent(model: form, hosts: hosts) { _ in })
        await spin()
        hosts.show([view(.online(.direct))])
        await spin()
        XCTAssertNil(
            element("new-agent.refusal"),
            "desk is back (direct) and the form still says: \(element("new-agent.refusal")?.value ?? "")")
    }

    private func tearDownWindow() {
        window?.isHidden = true
        window?.rootViewController = nil
        window = nil
        probe.elements = []
    }
}
