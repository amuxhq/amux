import AmuxMacScreens
import AmuxValues
import Foundation
import SwiftUI

/// One named screen in one state, drawn from values alone with inert
/// actions: no daemon, no bridge and nothing of the person's.
public struct Story: Identifiable, Sendable {
    /// The window the screen belongs in.
    public enum Window: Sendable {
        case main
        case settings
    }

    public let name: String
    public let window: Window
    private let build: @MainActor @Sendable () -> AnyView

    public var id: String { name }

    /// What comes before the first dot: the stories of one screen.
    public var group: String { String(name.prefix { $0 != "." }) }

    @MainActor public func view() -> AnyView { build() }

    init(_ name: String, window: Window = .main, _ build: @escaping @MainActor @Sendable () -> some View) {
        self.name = name
        self.window = window
        self.build = {
            switch window {
            case .main: AnyView(build().frame(minWidth: 680, minHeight: 560))
            case .settings: AnyView(build())
            }
        }
    }
}

/// A story as `tests/stories.json` lists it.
public struct ListedStory: Decodable, Sendable {
    public let name: String
    public let shows: String
    let clients: [String]
}

public enum Stories {
    /// Every story, in the list's order.
    public static let all: [Story] = launch + setup + fleet + hosts + chat + settings

    /// The list's Mac stories, in its order.
    public static func listed(in list: Data) throws -> [ListedStory] {
        struct List: Decodable { let stories: [ListedStory] }
        return try JSONDecoder().decode(List.self, from: list).stories
            .filter { $0.clients.contains("mac") }
    }

    /// How the catalogue differs from the list's Mac stories: nothing when
    /// they are the same stories in the same order.
    public static func differences(from listed: [ListedStory]) -> [String] {
        let ours = all.map(\.name)
        let theirs = listed.map(\.name)
        var found: [String] = []
        let missing = theirs.filter { !ours.contains($0) }
        let unlisted = ours.filter { !theirs.contains($0) }
        if !missing.isEmpty { found.append("listed for the Mac but not drawn: \(missing)") }
        if !unlisted.isEmpty { found.append("drawn but not listed for the Mac: \(unlisted)") }
        if found.isEmpty, let at = ours.indices.first(where: { ours[$0] != theirs[$0] }) {
            found.append("story \(at + 1) is \(ours[at]) here but \(theirs[at]) in the list")
        }
        if Set(ours).count != ours.count { found.append("a story is named twice") }
        return found
    }

    // MARK: - Before amux answers

    private static let launch = [
        Story("app.installing") { LaunchScreen(state: .installing) },
        Story("app.connecting") { LaunchScreen(state: .attaching) },
        Story("app.not-running") {
            LaunchScreen(state: .failed("Could not reach the amux daemon at ~/.amux/amux.sock."))
        },
    ]

    // MARK: - Setting up

    private static let machine = "Ada’s MacBook Pro"
    private static let phone = PairedDevice(hostId: Array(repeating: 7, count: 16), name: "Ada’s iPhone")
    private static let window = PairingWindowView(
        pin: "418207", qrLink: "https://amux.sh/pair#a-link-for-the-storybook", ttlSeconds: 600)

    private static func home(_ path: String) -> String { NSHomeDirectory() + path }

    private static let setup = [
        Story("onboarding.pair") {
            SetupStory(SetupContent(step: .pair_phone, machineName: machine, pairing: PairingContent(window: window)))
        },
        Story("onboarding.pair-rename") {
            SetupStory(
                SetupContent(
                    step: .pair_phone, machineName: machine, pairing: PairingContent(window: window),
                    renaming: machine))
        },
        Story("onboarding.pairing") {
            SetupStory(
                SetupContent(
                    step: .pair_phone, machineName: machine,
                    pairing: PairingContent(window: window, joining: true)))
        },
        Story("onboarding.paired") {
            SetupStory(SetupContent(step: .keep_reachable, machineName: machine, paired: phone))
        },
        Story("onboarding.sign-in") {
            SetupStory(
                SetupContent(step: .sign_in, machineName: machine, also: phone),
                choices: SetupChoices(alsoTicked: true))
        },
        Story("onboarding.sign-in-waiting") {
            SetupStory(SetupContent(step: .sign_in, machineName: machine, signingIn: true))
        },
        Story("onboarding.sign-in-offer") {
            SetupStory(
                SetupContent(
                    step: .sign_in, machineName: machine,
                    offer: SignInOfferView(
                        hostId: phone.hostId, hostName: phone.name, email: "ada@example.com",
                        accountName: "Ada")))
        },
        Story("onboarding.repositories") {
            SetupStory(
                SetupContent(step: .repositories, machineName: machine),
                choices: SetupChoices(repositories: [
                    SetupRepository(path: home("/src/website"), ticked: true),
                    SetupRepository(path: home("/src/amux"), ticked: true),
                    SetupRepository(path: home("/scratch/notes"), ticked: false),
                ]))
        },
        Story("onboarding.improve") { SetupStory(SetupContent(step: .improve, machineName: machine)) },
        Story("onboarding.first-agent") { SetupStory(SetupContent(step: .first_agent, machineName: machine)) },
    ]

    // MARK: - The agent list

    private static func account(_ binding: AccountBinding) -> AccountView {
        AccountView(
            binding: binding, email: binding == .unbound ? "" : "ada@example.com",
            name: binding == .signedIn ? "Ada Lovelace" : "", picture: "",
            relay: binding == .signedIn ? .connected : .off, pro: binding == .signedIn ? true : nil)
    }

    private static let signedIn = ProfileView(id: "personal", label: "Personal", subject: "ada", account: account(.signedIn))
    private static let work = ProfileView(id: "work", label: "Work", subject: "", account: account(.unbound))

    private static let elsewhere = Array(repeating: 2, count: 16) as [UInt8]

    private static func card(
        _ number: UInt8, _ name: String, on host: String, _ attention: Attention, cwd: String = "/src/amux",
        filing: Filing = .open, children: UInt32 = 0, ago: Int64 = 0, exit: ExitCause? = nil
    ) -> FleetCard {
        FleetCard(
            agent: AgentKey(host: host == machine ? Array(repeating: 1, count: 16) : elsewhere, agent: Array(repeating: number, count: 16)),
            name: name, handle: name, kind: .claudeSdk, attention: attention, cwd: home(cwd),
            phaseSinceMs: Int64(Date.now.timeIntervalSince1970 * 1000) - ago * 1000,
            host: host, hostReach: .online(.direct), children: children, familyAttention: attention,
            members: children + 1, descendants: children, backgroundJobs: 0, closeHeld: false, filing: filing,
            topLevel: true, branch: "main", exitCause: exit)
    }

    private static func row(
        _ card: FleetCard, _ line: SecondLine, depth: UInt32 = 0, expanded: Bool = false
    ) -> FleetRow {
        FleetRow(card: card, depth: depth, expanded: expanded, secondLine: line, loud: nil)
    }

    private static func step(_ text: String) -> SecondLine {
        .step(
            ActivityLine(
                activity: Activity(kind: .running(key: "s"), sinceMs: 0, elapsedMs: 0, turnElapsedMs: 0),
                step: text))
    }

    private static let pinned = FleetSection(
        kind: .pinned, families: 1, foldedByDefault: false,
        rows: [
            row(card(6, "Ship 0.9.0", on: machine, .idle, filing: .pinned, ago: 3_600),
                .lastSaid("Everything on the list is done but the App Store build.")),
        ])

    private static let needsYou = FleetSection(
        kind: .needsYou, families: 1, foldedByDefault: false,
        rows: [
            row(card(1, "Fix the flaky relay test", on: machine, .needsYou, ago: 40),
                .said(.ask(key: "a", wants: .run(command: "cargo test -p relay"), why: "", more: 0))),
        ])

    private static let live = FleetSection(
        kind: .live, families: 4, foldedByDefault: false,
        rows: [
            row(card(2, "Write the release notes", on: machine, .working, cwd: "/src/website", ago: 12),
                step("Reading CHANGELOG.md")),
            row(card(3, "Tidy the settings page", on: "build-box", .working, children: 2, ago: 95),
                step("cargo build -p amux-ui"), expanded: true),
            row(card(7, "Check the toggles", on: "build-box", .working, ago: 30), step("Searching for “Toggle(”"),
                depth: 1),
            row(card(8, "Check the copy", on: "build-box", .idle, ago: 200), .lastSaid("Two labels say “Enabled”."),
                depth: 1),
            row(card(4, "Review the store migration", on: "build-box", .idle, ago: 7_200),
                .lastSaid("The migration is safe; two nits inside.")),
            row(card(5, "Bump the toolchain", on: machine, .exited, ago: 86_400, exit: .failed("rustup was not found")),
                .exited(.failed("rustup was not found"))),
        ])

    private static let settled = FleetSection(
        kind: .closed, families: 3, foldedByDefault: true,
        rows: [
            row(card(9, "Rename the crates", on: machine, .exited, filing: .closed, ago: 172_800, exit: .finished),
                .lastSaid("Renamed all twelve; the build is green.")),
            row(card(10, "Draft the launch post", on: machine, .exited, filing: .closed, ago: 259_200, exit: .finished),
                .lastSaid("The draft is in notes/launch.md.")),
            row(card(11, "Measure cold start", on: "build-box", .exited, filing: .closed, ago: 400_000, exit: .finished),
                .lastSaid("Cold start is 410 ms on the M1.")),
        ])

    private static let busy = FleetContent(sections: [pinned, needsYou, live, settled])

    private static let fleet = [
        Story("fleet.no-agents") { MainStory(MainContent(profiles: [signedIn, work], fleet: FleetContent())) },
        Story("fleet.busy") { MainStory(MainContent(profiles: [signedIn, work], fleet: busy)) },
        Story("fleet.finish-setup") {
            MainStory(
                MainContent(
                    profiles: [signedIn, work],
                    fleet: FleetContent(sections: [needsYou, FleetSection(
                        kind: .live, families: 1, foldedByDefault: false, rows: [live.rows[0]])]),
                    toFinish: 3))
        },
        Story("fleet.settled") {
            MainStory(MainContent(profiles: [signedIn, work], fleet: busy), place: Place(settled: true))
        },
    ]

    // MARK: - Hosts

    private static func host(
        _ number: UInt8, _ name: String, _ reach: Reach, platform: Platform = .macos, local: Bool = false
    ) -> HostView {
        HostView(
            hostId: Array(repeating: number, count: 16), name: name, local: local, trusted: true, candidate: false,
            reach: reach, addrs: [], relayOnly: false, pairingOpen: false, current: true, providers: [],
            claudeKind: .claudeSdk, lastDialError: nil, platform: platform, signedIn: true, through: nil,
            version: "0.9.0")
    }

    private static let thisMac = host(1, machine, .online(.direct), local: true)
    private static let paired = [
        host(2, "build-box", .online(.direct), platform: .linux),
        host(3, "Ada’s iPhone", .online(.relay), platform: .ios),
        host(4, "studio.local", .offline(.plain)),
    ]

    private static let hosts = [
        Story("hosts.list") {
            MainStory(
                MainContent(profiles: [signedIn], fleet: busy, hosts: paired, thisMac: thisMac),
                place: Place(surface: .hosts))
        },
        Story("hosts.empty") {
            MainStory(MainContent(profiles: [work], thisMac: thisMac), place: Place(surface: .hosts))
        },
    ]

    // MARK: - A chat

    private static var nowMs: Int64 { Int64(Date.now.timeIntervalSince1970 * 1000) }

    private static func r(
        _ id: String, _ kind: RowKind, run: Run? = nil, collapsed: Bool = false, attention: Bool = false
    ) -> Row {
        Row(
            id: id, order: UInt64(id.dropFirst()) ?? 0, atMs: nowMs, kind: kind, collapsed: collapsed,
            attention: attention, decision: nil, parent: nil, run: run)
    }

    private static func command(
        _ text: String, _ state: CallPhase, output: [String] = [], ms: Int64? = 1_200, exit: Int32? = 0
    ) -> RowKind {
        .command(
            command: text, state: state, outputHead: output, moreLines: 0, outputTrimmed: false, outputTail: [],
            durationMs: state == .running ? nil : ms, exitCode: state == .running ? nil : exit)
    }

    private static func text(_ words: String) -> [Segment] { [.text(words)] }

    private static func frame(
        _ card: FleetCard, _ phase: PhaseView, activity: ActivityKind? = nil, mode: Composer = .send
    ) -> ChatFrame {
        ChatFrame(
            agent: card.agent, name: card.name, kind: card.kind, phase: phase,
            composer: ComposerView(
                mode: mode, switchedAway: false,
                activity: activity.map {
                    Activity(kind: $0, sinceMs: 0, elapsedMs: 42_000, turnElapsedMs: 42_000)
                }),
            controls: ControlsSummary(effort: "High", mode: nil, model: "Opus 5.5", permission: nil),
            connection: .live, caughtUp: true, hasOlder: false, arrivalsHeld: false, queue: [], underway: [],
            refused: [], askInput: nil, context: nil, ended: nil,
            git: Git(
                baseBranch: "main", branch: "relay-flake",
                onBranch: ChangeTotals(files: 3, added: 48, removed: 12),
                uncommitted: ChangeTotals(files: 2, added: 30, removed: 4)),
            signIn: nil)
    }

    private static let conversationRows: [Row] = [
        r("r1", .prompt(text: text("The relay test fails about one run in twenty on CI. Can you find out why?"), steered: false)),
        r("r2", .prose(text: text("I'll start with the test and the last few CI logs."), streaming: false, workingNote: true)),
        r("r3", command("cargo nextest run -p relay reconnect", .succeeded),
          run: Run(first: "r3", last: "r5", steps: 3, live: false, openBelow: false, unresolvedFailure: false,
                   open: false, counts: nil, recent: 2), collapsed: true),
        r("r4", .explore(verb: .read, subject: "crates/relay/tests/reconnect.rs", state: .succeeded, result: ""),
          run: Run(first: "r3", last: "r5", steps: 3, live: false, openBelow: false, unresolvedFailure: false,
                   open: false, counts: nil, recent: 1), collapsed: true),
        r("r5", .explore(verb: .search, subject: "sleep(", state: .succeeded, result: ""),
          run: Run(first: "r3", last: "r5", steps: 3, live: false, openBelow: false, unresolvedFailure: false,
                   open: false, counts: RunCounts(commands: 1, edits: 0, reads: 1, searches: 1, subagents: 0, other: 0),
                   recent: 0)),
        r("r6", .prose(text: text("""
            Found it. The test cuts the link **before the relay has acknowledged the hello**, so on a slow \
            runner the reconnect races the first handshake.

            - `reconnect_after_cut` sleeps 50 ms instead of waiting for the link to settle
            - on CI the hello takes up to 80 ms

            The fix is to wait on the settled event:

            ```rust
            link.settled().await;
            relay.cut(&link);
            ```
            """), streaming: false, workingNote: false)),
        r("r7", .fileChange(
            files: [FileRow(path: "crates/relay/tests/reconnect.rs", change: .edited, added: 6, removed: 3, line: 41)],
            state: .succeeded)),
        r("r8", command("cargo nextest run -p relay reconnect --retries 0", .succeeded,
                        output: ["    Starting 4 tests across 1 binary", "        PASS [   0.412s] relay::reconnect", "     Summary [   1.204s] 4 tests run: 4 passed"])),
        r("r9", .turnEnd(failed: false, durationMs: 194_000)),
    ]

    private static let workingRows: [Row] = [
        r("r1", .prompt(text: text("Draft the release notes for 0.9.0 from the changelog."), steered: false)),
        r("r2", .explore(verb: .read, subject: "CHANGELOG.md", state: .succeeded, result: ""),
          run: Run(first: "r2", last: "r5", steps: 5, live: true, openBelow: false, unresolvedFailure: false,
                   open: false, counts: nil, recent: 3), collapsed: true),
        r("r3", .explore(verb: .read, subject: "docs/guide/README.md", state: .succeeded, result: ""),
          run: Run(first: "r2", last: "r5", steps: 5, live: true, openBelow: false, unresolvedFailure: false,
                   open: false, counts: nil, recent: 2)),
        r("r4", .explore(verb: .search, subject: "BREAKING", state: .succeeded, result: ""),
          run: Run(first: "r2", last: "r5", steps: 5, live: true, openBelow: false, unresolvedFailure: false,
                   open: false, counts: nil, recent: 1)),
        r("r5", command("git log --oneline v0.8.7..HEAD", .running),
          run: Run(first: "r2", last: "r5", steps: 5, live: true, openBelow: false, unresolvedFailure: false,
                   open: false, counts: nil, recent: 0)),
    ]

    private static var flaky: FleetRow { needsYou.rows[0] }
    private static var notes: FleetRow { live.rows[0] }

    private static let askRows: [Row] = Array(conversationRows.prefix(6)) + [
        r("r7", command("cargo test -p relay -- --test-threads 1", .asking), attention: true),
    ]

    private static let review = ReviewDoc(
        base: "main", head: "relay-flake",
        files: [
            ReviewFile(
                path: "crates/relay/tests/reconnect.rs", status: .modified, added: 6, removed: 3, binary: false,
                hunks: [
                    Hunk(
                        skips: true, scope: "async fn reconnect_after_cut() {",
                        lines: [
                            DiffLine(kind: .context, text: "    let relay = Relay::start().await;", comments: [], newLine: 38, oldLine: 38),
                            DiffLine(kind: .context, text: "    let link = relay.dial(&host).await?;", comments: [], newLine: 39, oldLine: 39),
                            DiffLine(kind: .removed, text: "    // Give the hello time to land.", comments: [], newLine: nil, oldLine: 40),
                            DiffLine(kind: .removed, text: "    tokio::time::sleep(Duration::from_millis(50)).await;", comments: [], newLine: nil, oldLine: 41),
                            DiffLine(kind: .removed, text: "    relay.cut(&link);", comments: [], newLine: nil, oldLine: 42),
                            DiffLine(kind: .added, text: "    // Cut only once the relay has acknowledged the hello:", comments: [], newLine: 40, oldLine: nil),
                            DiffLine(kind: .added, text: "    // a slow runner otherwise races the first handshake.", comments: [], newLine: 41, oldLine: nil),
                            DiffLine(kind: .added, text: "    link.settled().await;", comments: ["Is there a timeout on settled()? A link that never settles would hang the test."], newLine: 42, oldLine: nil),
                            DiffLine(kind: .added, text: "    relay.cut(&link);", comments: [], newLine: 43, oldLine: nil),
                            DiffLine(kind: .context, text: "", comments: [], newLine: 44, oldLine: 43),
                            DiffLine(kind: .context, text: "    let back = link.reconnected().await?;", comments: [], newLine: 45, oldLine: 44),
                        ]),
                ],
                comments: [], oldPath: nil),
            ReviewFile(
                path: "crates/relay/src/link.rs", status: .modified, added: 3, removed: 0, binary: false,
                hunks: [
                    Hunk(
                        skips: true, scope: "impl Link {",
                        lines: [
                            DiffLine(kind: .context, text: "    /// Resolves once the relay acknowledged the hello.", comments: [], newLine: 120, oldLine: 120),
                            DiffLine(kind: .added, text: "    pub async fn settled(&self) {", comments: [], newLine: 121, oldLine: nil),
                            DiffLine(kind: .added, text: "        self.hello.wait().await", comments: [], newLine: 122, oldLine: nil),
                            DiffLine(kind: .added, text: "    }", comments: [], newLine: 123, oldLine: nil),
                        ]),
                ],
                comments: [], oldPath: nil),
        ],
        added: 9, removed: 3)

    private static func chatStory(_ name: String, _ row: FleetRow, _ chat: @escaping @MainActor () -> StillChat)
        -> Story
    {
        Story(name) {
            MainStory(
                MainContent(profiles: [signedIn], fleet: busy, hosts: paired, thisMac: thisMac),
                place: Place(agent: row.card.agent), chat: chat())
        }
    }

    private static let chat = [
        chatStory("chat.loading", notes) { StillChat(rows: [], frame: nil, loading: true) },
        chatStory("chat.working", notes) {
            StillChat(rows: workingRows, frame: frame(notes.card, .working, activity: .running(key: "r5")))
        },
        chatStory("chat.conversation", flaky) {
            StillChat(
                rows: conversationRows, frame: frame(flaky.card, .idle),
                changes: ChangeTotals(files: 1, added: 6, removed: 3))
        },
        chatStory("chat.asking", flaky) {
            StillChat(
                rows: askRows, frame: frame(flaky.card, .needsYou),
                ask: AskCard(
                    kind: .claudeSdk, key: "a", itemKey: "r7", position: 1, count: 1,
                    body: .command(
                        command: "cargo test -p relay -- --test-threads 1", cwd: home("/src/amux"),
                        reason: "", description: "Run the relay tests one at a time to rule out cross-test interference"),
                    choices: [
                        Choice(outcome: .allowOnce, primary: true, takesNote: false),
                        Choice(
                            outcome: .allowSimilar(prefix: ["cargo", "test"]), primary: false, takesNote: false),
                        Choice(outcome: .deny, primary: false, takesNote: true),
                    ],
                    questionNote: true, questionSkip: true, questionReply: true, stopsTurn: true, defaultToNo: false,
                    state: .open),
                changes: ChangeTotals(files: 1, added: 6, removed: 3))
        },
        Story("chat.overview") {
            MainStory(
                MainContent(profiles: [signedIn], fleet: busy, hosts: paired, thisMac: thisMac),
                place: Place(agent: flaky.card.agent, inspector: .changes),
                chat: {
                    let chat = StillChat(
                        rows: conversationRows, frame: frame(flaky.card, .idle),
                        changes: ChangeTotals(files: 1, added: 6, removed: 3))
                    chat.reviewing = StillReview(doc: review, selected: ReviewSpot(file: 0, hunk: 0, line: 6))
                    return chat
                }())
        },
    ]

    // MARK: - Settings

    private static let machineSettings = MachineSettings(
        hostName: machine, keepAwake: true,
        repositories: [home("/src/amux"), home("/src/website")], telemetry: true, autoReports: false,
        startAtLogin: true)

    private static let settings = [
        Story("settings.machine", window: .settings) {
            SettingsScreen(state: .ready(machineSettings), account: SettingsAccount(profile: signedIn))
        },
        Story("settings.machine-signed-out", window: .settings) {
            SettingsScreen(state: .ready(machineSettings), account: SettingsAccount(profile: work))
        },
    ]
}

/// A setting-up screen whose choices can be flipped in the storybook.
private struct SetupStory: View {
    let content: SetupContent
    @State private var choices: SetupChoices

    init(_ content: SetupContent, choices: SetupChoices = SetupChoices()) {
        self.content = content
        _choices = State(initialValue: choices)
    }

    var body: some View { SetupScreen(content: content, choices: $choices) }
}

/// The main window with its first profile open, somewhere the storybook
/// can move around, and a still chat for the agent it opens.
private struct MainStory: View {
    let content: MainContent
    let chat: StillChat?
    @State private var place: Place

    init(_ content: MainContent, place: Place = Place(), chat: StillChat? = nil) {
        self.content = content
        self.chat = chat
        _place = State(initialValue: place)
    }

    var body: some View {
        MainScreen(content: content, place: $place) {
            if let chat, let agent = place.agent {
                ChatPane(
                    chat: chat, row: content.fleet.sections.flatMap(\.rows).first { $0.card.agent == agent },
                    inspectorOpen: place.inspector != nil, openInspector: { place.inspector = $0 })
            } else {
                EmptyCentre(surface: place.surface)
            }
        } inspector: {
            if let chat, let tab = place.inspector {
                InspectorPane(tab: tab, chat: chat, family: nil)
            }
        }
    }
}
