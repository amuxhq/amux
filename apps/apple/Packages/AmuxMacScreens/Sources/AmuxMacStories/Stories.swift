import AppKit
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
        /// A sheet, drawn alone at its own size.
        case sheet
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
            case .settings, .sheet: AnyView(build())
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
    public static let all: [Story] = launch + setup + fleet + hosts + newAgent + chat + settings

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
            topLevel: true, native: false, branch: "main", exitCause: exit, repository: repository(cwd))
    }

    /// The repository the runtime would name for a folder under /src, as if
    /// each had an origin at github.com/amuxhq: the tints are the runtime's
    /// for those remotes.
    static func repository(_ cwd: String) -> Repository? {
        let name = (cwd as NSString).lastPathComponent
        let tints: [String: UInt8] = ["amux": 5, "website": 7, "console": 6, "amuxcloud": 0, "skills": 3]
        let letters = name.filter { $0.isLetter || $0.isNumber }.lowercased()
        guard let first = letters.first, let last = letters.last else { return nil }
        return Repository(
            key: "github.com/amuxhq/\(name)", name: name,
            mark: letters.count == 1 ? String(first) : "\(first)\(last)", tint: tints[name] ?? 7)
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
            row(card(3, "Tidy the settings page", on: "build-box", .working, cwd: "/src/console", children: 2, ago: 95),
                step("cargo build -p amux-ui"), expanded: true),
            row(card(7, "Check the toggles", on: "build-box", .working, cwd: "/src/console", ago: 30), step("Searching for “Toggle(”"),
                depth: 1),
            row(card(8, "Check the copy", on: "build-box", .idle, cwd: "/src/console", ago: 200), .lastSaid("Two labels say “Enabled”."),
                depth: 1),
            row(card(4, "Review the store migration", on: "build-box", .idle, cwd: "/src/amuxcloud", ago: 7_200),
                .lastSaid("The migration is safe; two nits inside.")),
            row(card(5, "Bump the toolchain", on: machine, .exited, ago: 86_400, exit: .failed("rustup was not found")),
                .exited(.failed("rustup was not found"))),
        ])

    private static let settled = FleetSection(
        kind: .closed, families: 3, foldedByDefault: true,
        rows: [
            row(card(9, "Rename the crates", on: machine, .exited, filing: .closed, ago: 172_800, exit: .finished),
                .lastSaid("Renamed all twelve; the build is green.")),
            row(card(10, "Draft the launch post", on: machine, .exited, cwd: "/src/website", filing: .closed, ago: 259_200, exit: .finished),
                .lastSaid("The draft is in notes/launch.md.")),
            row(card(11, "Measure cold start", on: "build-box", .exited, filing: .closed, ago: 400_000, exit: .finished),
                .lastSaid("Cold start is 410 ms on the M1.")),
        ])

    private static let busy = FleetContent(sections: [pinned, needsYou, live, settled])

    private static let fleet = [
        Story("fleet.no-agents") { MainStory(MainContent(profiles: [signedIn, work], fleet: FleetContent(), thisMac: thisMac)) },
        Story("fleet.busy") { MainStory(MainContent(profiles: [signedIn, work], fleet: busy, thisMac: thisMac)) },
        Story("fleet.finish-setup") {
            MainStory(
                MainContent(
                    profiles: [signedIn, work],
                    fleet: FleetContent(sections: [needsYou, FleetSection(
                        kind: .live, families: 1, foldedByDefault: false, rows: [live.rows[0]])]),
                    thisMac: thisMac, toFinish: 3))
        },
        Story("fleet.settled") {
            MainStory(MainContent(profiles: [signedIn, work], fleet: busy, thisMac: thisMac), place: Place(settled: true))
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

    /// build-box, with what it says of itself.
    private static var buildBox: HostView {
        var box = paired[0]
        box.providers = [
            ProviderSignIn(provider: .claude, signedIn: true), ProviderSignIn(provider: .codex, signedIn: false),
        ]
        box.addrs = ["192.168.1.24:47821", "[fe80::1c2b:3a]:47821"]
        return box
    }

    private static let hosts = [
        Story("hosts.list") {
            MainStory(
                MainContent(profiles: [signedIn], fleet: busy, hosts: [buildBox] + paired.dropFirst(), thisMac: thisMac),
                place: Place(surface: .hosts, host: buildBox.hostId))
        },
        Story("hosts.empty") {
            MainStory(MainContent(profiles: [work], thisMac: thisMac), place: Place(surface: .hosts))
        },
    ]

    // MARK: - Starting an agent

    @MainActor static func starting() -> StillNewAgent {
        StillNewAgent(
            hosts: [thisMac, buildBox] + paired.dropFirst(), machine: thisMac.hostId,
            repositories: [
                Directory(path: home("/src/amux"), name: "amux"),
                Directory(path: home("/src/website"), name: "website"),
                Directory(path: home("/src/skills"), name: "skills"),
            ],
            recent: [
                Directory(path: home("/src/amux-a2a"), name: "amux-a2a"),
                Directory(path: home("/Desktop/notes"), name: "notes"),
            ],
            directory: home("/src/amux"))
    }

    /// What a Claude agent offers to change, as a chat's settings hold it.
    @MainActor static var claude: SettingsView { starting().settings! }

    private static let newAgent = [
        Story("new-agent.claude") {
            MainStory(MainContent(profiles: [signedIn], fleet: busy, hosts: paired, thisMac: thisMac))
        },
        Story("new-agent.browsing", window: .sheet) {
            DirectoryChooser(surface: starting())
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color(nsColor: .windowBackgroundColor))
        },
    ]

    // MARK: - A chat

    private static var nowMs: Int64 { Int64(Date.now.timeIntervalSince1970 * 1000) }

    /// A row; `turn` is its part in the finished turn `end` closes, as the
    /// runtime places it for a client folding turns.
    private static func r(
        _ id: String, _ kind: RowKind, run: Run? = nil, collapsed: Bool = false, attention: Bool = false,
        turn: TurnPart? = nil, end: String = "r9"
    ) -> Row {
        Row(
            id: id, order: UInt64(id.dropFirst()) ?? 0, atMs: nowMs, kind: kind, collapsed: collapsed,
            attention: attention, decision: nil, group: nil, opens: nil, parent: nil, run: run,
            turn: turn.map { TurnPlace(end: end, open: false, part: $0) })
    }

    private static func command(
        _ text: String, _ state: CallPhase, output: [String] = [], ms: Int64? = 1_200, exit: Int32? = 0
    ) -> RowKind {
        .command(
            command: text, description: "", state: state, outputHead: output, moreLines: 0, outputTrimmed: false, outputTail: [],
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
                onBranch: ChangeTotals(files: 3, added: 48, removed: 12), remote: "github.com/amuxhq/amux",
                uncommitted: ChangeTotals(files: 2, added: 30, removed: 4)),
            signIn: nil)
    }

    private static let conversationRows: [Row] = [
        r("r1", .prompt(text: text("The relay test fails about one run in twenty on CI. Can you find out why?"), steered: false),
          turn: .prompt(worked: Worked(failed: false, durationMs: 194_000))),
        r("r2", .prose(text: text("I'll start with the test and the last few CI logs."), streaming: false, workingNote: true),
          turn: .work),
        r("r3", command("cargo nextest run -p relay reconnect", .succeeded),
          run: Run(first: "r3", last: "r5", steps: 3, live: false, openBelow: false, liveShown: false, unresolvedFailure: false,
                   open: false, counts: nil, hidden: nil, recent: 2), collapsed: true, turn: .work),
        r("r4", .explore(verb: .read, subject: "crates/relay/tests/reconnect.rs", state: .succeeded, result: ""),
          run: Run(first: "r3", last: "r5", steps: 3, live: false, openBelow: false, liveShown: false, unresolvedFailure: false,
                   open: false, counts: nil, hidden: nil, recent: 1), collapsed: true, turn: .work),
        r("r5", .explore(verb: .search, subject: "sleep(", state: .succeeded, result: ""),
          run: Run(first: "r3", last: "r5", steps: 3, live: false, openBelow: false, liveShown: false, unresolvedFailure: false,
                   open: false, counts: RunCounts(commands: 1, edits: 0, reads: 1, searches: 1, lists: 0, fetches: 0, subagents: 0, other: 0), hidden: nil,
                   recent: 0), turn: .work),
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
            """), streaming: false, workingNote: false), turn: .answer),
        r("r7", .fileChange(files: [reconnectEdit], state: .succeeded), turn: .work),
        r("r8", command("cargo nextest run -p relay reconnect --retries 0", .succeeded,
                        output: ["    Starting 4 tests across 1 binary", "        PASS [   0.412s] relay::reconnect", "     Summary [   1.204s] 4 tests run: 4 passed"]),
          turn: .work),
        r("r9", .turnEnd(failed: false, durationMs: 194_000), turn: .end(answer: ["r6"], files: [reconnectEdit])),
    ]

    private static let reconnectEdit = FileRow(
        path: "crates/relay/tests/reconnect.rs", change: .edited, added: 6, removed: 3, line: 41)

    private static let workingRows: [Row] = [
        r("r1", .prompt(text: text("Draft the release notes for 0.9.0 from the changelog."), steered: false)),
        r("r2", .explore(verb: .read, subject: "CHANGELOG.md", state: .succeeded, result: ""),
          run: liveRun(recent: 3), collapsed: true),
        r("r3", .explore(verb: .read, subject: "docs/guide/README.md", state: .succeeded, result: ""),
          run: liveRun(recent: 2), collapsed: true),
        r("r4", .explore(verb: .search, subject: "BREAKING", state: .succeeded, result: ""),
          run: liveRun(recent: 1), collapsed: true),
        r("r5", command("git log --oneline v0.8.7..HEAD", .running),
          run: liveRun(
              recent: 0, shown: true,
              hidden: RunCounts(
                  commands: 0, edits: 0, reads: 2, searches: 1, lists: 0, fetches: 0, subagents: 0, other: 0))),
    ]

    /// A step of the run under way in `workingRows`: its newest step shows,
    /// under what the steps before it did.
    private static func liveRun(recent: UInt32, shown: Bool = false, hidden: RunCounts? = nil) -> Run {
        Run(
            first: "r2", last: "r5", steps: 4, live: true, openBelow: false, liveShown: shown,
            unresolvedFailure: false, open: false, counts: nil, hidden: hidden, recent: recent)
    }

    private static var flaky: FleetRow { needsYou.rows[0] }
    private static var notes: FleetRow { live.rows[0] }

    /// The same turn before it ended: nothing in it is placed in a turn yet.
    private static let askRows: [Row] = conversationRows.prefix(6).map { row in
        var row = row
        row.turn = nil
        return row
    } + [
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

    // MARK: - Subagents

    /// An agent that handed its audit to four of Claude's subagents.
    private static var auditor: FleetRow {
        row(card(12, "Audit the pairing code", on: machine, .working, children: 4, ago: 50),
            .step(ActivityLine(
                activity: Activity(kind: .subagents(count: 3), sinceMs: 0, elapsedMs: 0, turnElapsedMs: 0),
                step: nil)),
            expanded: true)
    }

    /// Its subagents, by the titles Claude gave them: each its own agent,
    /// with no folder, listed under the parent that runs it.
    private static let delegated: [(UInt8, String, Attention, String)] = [
        (21, "Survey the relay's retry path", .working, "sub-3f9a01c2"),
        (22, "Check the Windows build", .needsYou, "sub-81c4d2e7"),
        (23, "Find every INVALID_PIN reader", .exited, "sub-0b6e9f14"),
        (24, "Read the pairing spec", .working, "sub-d27a5c90"),
    ]

    private static var subagents: [FleetRow] {
        delegated.map { number, title, attention, handle in
            var member = card(number, title, on: machine, attention, ago: 40, exit: attention == .exited ? .finished : nil)
            member.handle = handle
            member.topLevel = false
            member.native = true
            member.branch = nil
            let line: SecondLine = switch attention {
            case .needsYou: .said(.ask(key: "a", wants: .run(command: "cargo check --target x86_64-pc-windows-msvc"), why: "", more: 0))
            case .exited: .lastSaid("Three readers, all on the same string.")
            default: step("Reading crates/relay-core/src/dial.rs")
            }
            return row(member, line, depth: 1)
        }
    }

    private static var delegatingFleet: FleetContent {
        FleetContent(sections: [
            needsYou,
            FleetSection(kind: .live, families: 2, foldedByDefault: false, rows: [auditor] + subagents + [live.rows[0]]),
        ])
    }

    private static func call(_ id: String, _ title: String, _ agent: UInt8, group: SubagentGroup?) -> Row {
        Row(
            id: id, order: UInt64(id.dropFirst()) ?? 0, atMs: nowMs,
            kind: .subagent(description: title, continued: false, state: .running, answer: "", durationMs: nil),
            collapsed: false, attention: false, decision: nil, group: group,
            opens: Array(repeating: agent, count: 16), parent: nil, run: nil, turn: nil)
    }

    /// The turn that started them: its calls fold into one group, which a
    /// click opens.
    private static var delegatingRows: [Row] {
        let group = SubagentGroup(
            first: "d4", agents: delegated.map { Array(repeating: $0.0, count: 16) }, open: false)
        return [
            r("d1", .prompt(text: text("Audit the pairing code four ways at once and report back."), steered: false)),
            r("d2", .thinking(text: "", open: false, durationMs: 3_000)),
            r("d3", .prose(text: text("Starting four subagents, one per area."), streaming: false, workingNote: false)),
        ] + delegated.enumerated().map { index, agent in
            call("d\(index + 4)", agent.1, agent.0, group: group)
        }
    }

    /// What the fleet says of them, as the runtime hands it with the chat.
    private static var delegatedAgents: ChatAgents {
        ChatAgents(
            agents: delegated.map { number, title, attention, _ in
                let phase: PhaseView = switch attention {
                case .needsYou: .needsYou
                case .exited: .exited(cause: .finished)
                default: .working
                }
                return OpenedAgent(
                    agent: AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: number, count: 16)),
                    name: title, kind: .claudeSdk, phase: phase)
            },
            groups: [GroupView(first: "d4", state: GroupState(
                agents: 4, running: 2, needsYou: 1, idle: 0, finished: 1, ended: 0, failed: 0, unknown: 0))])
    }

    /// The first subagent's own chat: the task its parent gave it, its
    /// thinking, its tools and what it found so far.
    private static let subagentRows: [Row] = [
        r("u1", .prompt(text: text("Survey the relay's retry path: where a dropped link is retried, and what bounds it."), steered: false)),
        r("u2", .thinking(text: "The retry lives in the link layer; start at the dialler.", open: false, durationMs: 2_000)),
        r("u3", .explore(verb: .search, subject: "retry_after", state: .succeeded, result: "")),
        r("u4", .explore(verb: .read, subject: "crates/relay-core/src/dial.rs", state: .succeeded, result: "")),
        r("u5", command("cargo test -p relay-core retry", .succeeded, ms: 6_100)),
        r("u6", .prose(text: text("The dialler retries a dropped link with backoff capped at 30 s and gives up at the link's deadline. The cap is `MAX_BACKOFF` in dial.rs."), streaming: false, workingNote: false)),
        r("u7", .explore(verb: .read, subject: "crates/relay-core/src/link.rs", state: .running, result: "")),
    ]

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
            StillChat(rows: workingRows, frame: frame(notes.card, .working, activity: .running(key: "r5")), settings: claude)
        },
        chatStory("chat.conversation", flaky) {
            StillChat(
                rows: conversationRows, frame: frame(flaky.card, .idle),
                changes: ChangeTotals(files: 1, added: 6, removed: 3), settings: claude)
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
                changes: ChangeTotals(files: 1, added: 6, removed: 3), settings: claude)
        },
        Story("chat.subagents") {
            MainStory(
                MainContent(profiles: [signedIn], fleet: delegatingFleet, hosts: paired, thisMac: thisMac),
                place: Place(agent: auditor.card.agent),
                chat: StillChat(
                    rows: delegatingRows,
                    frame: frame(auditor.card, .working, activity: .subagents(count: 3)),
                    settings: claude, agents: delegatedAgents))
        },
        Story("chat.subagent") {
            MainStory(
                MainContent(profiles: [signedIn], fleet: delegatingFleet, hosts: paired, thisMac: thisMac),
                place: Place(agent: subagents[0].card.agent),
                chat: {
                    var native = frame(
                        subagents[0].card, .working, activity: .running(key: "u7"),
                        mode: .native(stop: .offered(key: "toolu_02")))
                    native.git = nil
                    native.controls = ControlsSummary(effort: nil, mode: nil, model: nil, permission: nil)
                    return StillChat(rows: subagentRows, frame: native)
                }(),
                family: FamilyHeader(children: [], attention: .working, needYou: 0, parent: auditor.card))
        },
        Story("chat.overview") {
            MainStory(
                MainContent(profiles: [signedIn], fleet: busy, hosts: paired, thisMac: thisMac),
                place: Place(agent: flaky.card.agent, changes: true),
                chat: {
                    let chat = StillChat(
                        rows: conversationRows, frame: frame(flaky.card, .idle),
                        changes: ChangeTotals(files: 1, added: 6, removed: 3), settings: claude)
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
    let family: FamilyHeader?
    @State private var place: Place
    @State private var starting = Stories.starting()

    init(_ content: MainContent, place: Place = Place(), chat: StillChat? = nil, family: FamilyHeader? = nil) {
        self.content = content
        self.chat = chat
        self.family = family
        _place = State(initialValue: place)
    }

    var body: some View {
        MainScreen(content: content, place: $place) {
            if place.surface == .agents, let chat, let agent = place.agent {
                ChatPane(
                    chat: chat, row: content.fleet.sections.flatMap(\.rows).first { $0.card.agent == agent },
                    family: family, changesOpen: place.changes,
                    actions: AgentActions(
                        openChanges: { place.changes = true }, reveal: {}, openEditor: {}, editor: "VS Code",
                        editorIcon: appIcon("com.microsoft.VSCode"), terminalIcon: appIcon("com.apple.Terminal"),
                        finderIcon: appIcon("com.apple.finder")))
            } else if place.surface == .agents, place.agent == nil {
                NewAgentPage(surface: starting)
            } else if place.surface == .hosts, let id = place.host,
                let host = ([content.thisMac].compactMap { $0 } + content.hosts).first(where: { $0.hostId == id })
            {
                HostPage(
                    host: host,
                    agents: content.fleet.sections.flatMap(\.rows).filter { $0.card.agent.host == id },
                    now: content.fleet.now)
            } else {
                EmptyCentre(surface: place.surface)
            }
        } inspector: {
            if let chat, place.changes {
                ChangesPane(chat: chat) { place.changes = false }
            }
        }
    }
}

/// An installed app's icon, as the app's own card shows it.
@MainActor
private func appIcon(_ bundle: String) -> NSImage? {
    NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundle).map {
        NSWorkspace.shared.icon(forFile: $0.path)
    }
}
