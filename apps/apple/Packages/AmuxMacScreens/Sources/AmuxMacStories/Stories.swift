import AppKit
import AmuxMacScreens
import AmuxFixtures
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
            case .main: AnyView(build().frame(minWidth: MainWindowSize.least.width, minHeight: MainWindowSize.least.height))
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
                    SetupRepository(path: home("/src/website"), ticked: true, repository: repository(home("/src/website"))),
                    SetupRepository(path: home("/src/amux"), ticked: true, repository: repository(home("/src/amux"))),
                    SetupRepository(path: home("/scratch/notes"), ticked: false, repository: repository(home("/scratch/notes"))),
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
        filing: Filing = .open, children: UInt32 = 0, ago: Int64 = 0, exit: ExitCause? = nil,
        branch: String = "main", reach: Reach = .online(.direct), agent: HostProvider = agents[0]
    ) -> FleetCard {
        FleetCard(
            agent: AgentKey(host: host == machine ? Array(repeating: 1, count: 16) : elsewhere, agent: Array(repeating: number, count: 16)),
            name: name, handle: name, kind: (["claude": .claudeSdk, "codex": .codex] as [String: Kind])[agent.provider] ?? .acp,
            provider: agent.provider, providerName: agent.name, attachable: agent.capabilities.attachable,
            attention: attention, cwd: home(cwd),
            phaseSinceMs: Int64(Date.now.timeIntervalSince1970 * 1000) - ago * 1000,
            host: host, hostReach: reach, children: children, familyAttention: attention,
            members: children + 1, descendants: children, backgroundJobs: 0, closeHeld: false, filing: filing,
            topLevel: true, native: false, branch: branch, exitCause: exit, repository: repository(cwd))
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
                step: text, subagents: nil))
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
            row(card(1, "Fix the flaky relay test", on: machine, .needsYou, ago: 40, branch: "relay-flake"),
                .said(.ask(key: "a", wants: .run(command: "cargo test -p relay"), why: "", more: 0))),
        ])

    private static let live = FleetSection(
        kind: .live, families: 5, foldedByDefault: false,
        rows: [
            row(card(2, "Write the release notes", on: machine, .working, cwd: "/src/website", ago: 12, branch: "release-notes"),
                step("git log --oneline v0.8.7..HEAD")),
            row(card(3, "Tidy the settings page", on: "build-box", .working, cwd: "/src/console", children: 2, ago: 95, branch: "settings-tidy"),
                step("cargo build -p amux-ui")),
            row(card(4, "Review the store migration", on: "build-box", .idle, cwd: "/src/amuxcloud", ago: 7_200, agent: agents[2]),
                .lastSaid("The migration is safe; two nits inside.")),
            row(card(7, "Profile the phone build", on: "studio", .working, cwd: "/src/amux", ago: 1_800, branch: "phone-profile", reach: .away),
                step("Recording a trace")),
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

    /// Deleting an agent whose worktree goes, once its host answers with
    /// `answer`; a host that never answers when nil.
    @MainActor private static func deleting(answer: DeleteWarning?) -> some View {
        let agent = card(12, "Fix the scroll jump", on: machine, .idle, ago: 600)
        return DeleteSheet(card: agent, ask: { _ in
            guard let answer else {
                try? await Task.sleep(for: .seconds(86_400))
                return .success(nil)
            }
            return .success(answer)
        }, cancel: {}, confirm: { _ in })
            .background(Color(nsColor: .windowBackgroundColor))
    }

    private static let fleet = [
        Story("fleet.no-agents") { MainStory(MainContent(profiles: [signedIn, work], fleet: FleetContent(), thisMac: thisMac)) },
        Story("fleet.busy") { MainStory(MainContent(profiles: [signedIn, work], fleet: busy, thisMac: thisMac)) },
        Story("fleet.delete-changes", window: .sheet) {
            deleting(answer: .goes(WorkspaceGoes(
                branch: "fix-scroll", branchCommits: 3, uncommitted: ChangeTotals(files: 3, added: 42, removed: 7))))
        },
        Story("fleet.delete-clean", window: .sheet) {
            deleting(answer: .goes(WorkspaceGoes(branch: "fix-scroll", branchCommits: 0, uncommitted: nil)))
        },
        Story("fleet.delete-checking", window: .sheet) { deleting(answer: nil) },
        Story("fleet.finish-setup") {
            MainStory(
                MainContent(
                    profiles: [signedIn, work],
                    fleet: FleetContent(sections: [needsYou, FleetSection(
                        kind: .live, families: 1, foldedByDefault: false, rows: [live.rows[0]])]),
                    thisMac: thisMac, toFinish: 3))
        },
        Story("fleet.working") {
            MainStory(MainContent(profiles: [signedIn, work], fleet: busy, thisMac: thisMac), place: Place(working: true))
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
            reach: reach, sameNetwork: false, link: 0, addrs: [], relayOnly: false, pairingOpen: false, current: true, providers: agents,
            lastDialError: nil, platform: platform, signedIn: true, through: nil,
            version: "0.9.0")
    }

    /// What a host offers: Claude and Codex, two of the built-in agents
    /// that speak ACP, one of them signed out there, and one a person added
    /// to the host's config.
    static let agents = [
        offered("claude", "Claude", hint: "Run claude and sign in with /login.", can: .claude),
        offered("codex", "Codex", hint: "Run codex login.", can: .codex),
        offered("opencode", "OpenCode", hint: "Run opencode auth login in the terminal", can: .acp),
        offered("grok", "Grok", hint: "Sign in with Grok", signedIn: false, can: .acp),
        offered("local-llm", "Local Llama", hint: "", can: .acp),
    ]

    private static func offered(
        _ id: String, _ name: String, hint: String, signedIn: Bool = true, can: Capabilities
    ) -> HostProvider {
        HostProvider(
            provider: id, name: name, signInHint: hint, version: "", capabilities: can, signedIn: signedIn)
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
        box.providers = agents.map { agent in
            var agent = agent
            if agent.provider == "codex" { agent.signedIn = false }
            return agent
        }
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
                Directory(path: home("/src/amux"), name: "amux", repository: repository(home("/src/amux"))),
                Directory(path: home("/src/website"), name: "website", repository: repository(home("/src/website"))),
                Directory(path: home("/src/skills"), name: "skills", repository: repository(home("/src/skills"))),
            ],
            recent: [
                Directory(path: home("/src/amux-a2a"), name: "amux-a2a", repository: repository(home("/src/amux-a2a"))),
                Directory(path: home("/Desktop/notes"), name: "notes", repository: repository(home("/Desktop/notes"))),
            ],
            directory: home("/src/amux"))
        .with(history: [
            Directory(path: home("/src/dotfiles"), name: "dotfiles", repository: repository(home("/src/dotfiles"))),
        ])
    }

    /// What a Claude agent offers to change, as a chat's settings hold it.
    @MainActor static var claude: SettingsView { starting().settings! }

    private static let newAgent = [
        Story("new-agent.claude") {
            MainStory(MainContent(profiles: [signedIn], fleet: busy, hosts: paired, thisMac: thisMac))
        },
        Story("new-agent.picker", window: .sheet) {
            AgentPicker(surface: starting())
                .background(Color(nsColor: .windowBackgroundColor))
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

    private static func text(_ words: String) -> [ProsePart] { [.fixture(words)] }

    private static func frame(
        _ card: FleetCard, _ phase: PhaseView, activity: ActivityKind? = nil, mode: Composer = .send,
        chip: ReviewChip? = nil
    ) -> ChatFrame {
        ChatFrame(
            agent: card.agent, name: card.name,
            provider: ProviderLabel(provider: card.provider, name: card.providerName),
            capabilities: agents.first { $0.provider == card.provider }?.capabilities ?? .claude, phase: phase,
            composer: ComposerView(
                mode: mode, switchedAway: false,
                // The turn started when the agent went to work, so the chat's
                // clock and the list's agree; the step at work began 4s ago.
                activity: activity.map {
                    let now = Int64(Date.now.timeIntervalSince1970 * 1000)
                    return Activity(
                        kind: $0, sinceMs: now - 4_000, elapsedMs: 0,
                        turnElapsedMs: max(0, now - 4_000 - card.phaseSinceMs))
                }),
            controls: ControlsSummary(effort: "High", mode: nil, model: "Opus 5.5", permission: nil),
            connection: .live, caughtUp: true, hasOlder: false, arrivalsHeld: false, queue: [], underway: [],
            refused: [], askInput: nil, context: nil, ended: nil,
            git: Git(
                baseBranch: "main", branch: card.branch ?? "main",
                checkout: Checkout(root: card.cwd, main: nil, remote: "github.com/amuxhq/amux"),
                onBranch: ChangeTotals(files: 3, added: 48, removed: 12),
                uncommitted: ChangeTotals(files: 2, added: 30, removed: 4)),
            reviewChip: chip, signIn: nil)
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
                   open: false, counts: RunCounts(commands: 1, edits: 0, reads: 1, searches: 1, lists: 0, fetches: 0, subagents: 0, other: 0, denied: 0), hidden: nil,
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
        path: "crates/relay/tests/reconnect.rs", change: .edited, added: 3, removed: 2, line: 40)

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
                  commands: 0, edits: 0, reads: 2, searches: 1, lists: 0, fetches: 0, subagents: 0, other: 0, denied: 0))),
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

    // MARK: - Subagents

    /// An agent that handed its audit to four of Claude's subagents.
    /// One of them waits on the person, which the head's row says.
    private static var auditor: FleetRow {
        var head = card(12, "Audit the pairing code", on: machine, .working, children: 4, ago: 50, branch: "pairing-audit")
        head.familyAttention = .needsYou
        return row(head, .said(.ask(key: "a", wants: .run(command: "cargo check --target x86_64-pc-windows-msvc"), why: "", more: 0)))
    }

    /// Its subagents, by the titles Claude gave them: each its own agent,
    /// with no folder, reached from the parent's chat rather than listed.
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
            FleetSection(kind: .live, families: 2, foldedByDefault: false, rows: [auditor, live.rows[0]]),
        ])
    }

    /// The delegating fleet, with the subagents its head stands for.
    private static var delegating: MainContent {
        let members = subagents
        let head = auditor.card.agent
        return MainContent(
            profiles: [signedIn], fleet: delegatingFleet, hosts: paired, thisMac: thisMac,
            member: { agent in members.first { $0.card.agent == agent } },
            head: { agent in members.contains { $0.card.agent == agent } ? head : nil })
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
                agents: 4, running: 2, needsYou: 1, idle: 0, finished: 1, ended: 0, failed: 0, unknown: 0))],
            live: GroupState(
                agents: 3, running: 2, needsYou: 1, idle: 0, finished: 0, ended: 0, failed: 0, unknown: 0))
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
                changes: ChangeTotals(files: 1, added: 3, removed: 2), settings: claude)
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
                    questionNote: true, questionSkip: true, questionReply: true, replyInstead: false, stopsTurn: true, defaultToNo: false,
                    state: .open),
                changes: ChangeTotals(files: 1, added: 3, removed: 2), settings: claude)
        },
        Story("chat.subagents") {
            MainStory(
                delegating,
                place: Place(agent: auditor.card.agent),
                chat: StillChat(
                    rows: delegatingRows,
                    frame: frame(auditor.card, .working, activity: .subagents(count: 3)),
                    settings: claude, agents: delegatedAgents))
        },
        Story("chat.subagent") {
            MainStory(
                delegating,
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
                        changes: ChangeTotals(files: 3, added: 18, removed: 6), settings: claude)
                    chat.page = StillReviewPage(page: firstLook())
                    return chat
                }())
        },
    ]

    // MARK: - The agent's review

    /// The relay flake's chat with its review open in the tool window.
    private static func reviewStory(
        _ name: String, rows: [Row] = conversationRows, chip: ReviewChip? = nil,
        _ page: @escaping @MainActor @Sendable () -> StillReviewPage
    ) -> Story {
        Story(name) {
            MainStory(
                MainContent(profiles: [signedIn], fleet: busy, hosts: paired, thisMac: thisMac),
                place: Place(agent: flaky.card.agent, changes: true),
                chat: {
                    let chat = StillChat(
                        rows: rows, frame: frame(flaky.card, .idle, chip: chip),
                        changes: ChangeTotals(files: 3, added: 18, removed: 6), settings: claude)
                    chat.page = page()
                    chat.reviewChip = chip
                    return chat
                }())
        }
    }

    private static let snapshot = "5e1d0c94b2a7"
    private static let base = "a41f2b7e9013"
    private static let lastRound = "c03b9e2d17f4"
    private static let test = "crates/relay/tests/reconnect.rs"
    private static let link = "crates/relay/src/link.rs"

    /// The relay flake's fix on a first look, everything against main: the
    /// lock file folded, the link's new call, and the test's edit with a
    /// draft on the line that waits for the link to settle.
    private static func firstLook(banners: [ReviewBanner] = []) -> ReviewPage {
        ReviewPage(
            banners: banners,
            rows: [
                fileHead("Cargo.lock", added: 12, removed: 4, flags: [.lockfile], folded: true),
                fileHead(link, added: 3, removed: 0),
                hunkHead(link, old: 120, new: 120, against: base, scope: ["impl Link"]),
                kept(link, 120, 120, "    /// Resolves once the relay acknowledged the hello."),
                put(link, 121, "    pub async fn settled(&self) {"),
                put(link, 122, "        self.hello.wait().await"),
                put(link, 123, "    }"),
                gap(link, "end", hidden: nil, down: (124, 143, nil)),
                fileHead(test, added: 3, removed: 2, comments: 1),
                gap(test, "top", hidden: 37, up: (36, 37, "async fn reconnect_after_cut")),
                hunkHead(test, old: 38, new: 38, against: base, scope: ["async fn reconnect_after_cut"]),
                kept(test, 38, 38, "    let relay = Relay::start().await;"),
                kept(test, 39, 39, "    let link = relay.dial(&host).await?;"),
                cut(test, 40, "    // Give the hello time to land.", changed: ["Give the hello time to land."]),
                cut(test, 41, "    tokio::time::sleep(Duration::from_millis(50)).await;"),
                put(test, 40, "    // Cut only once the relay has acknowledged the hello:",
                    changed: ["Cut only once the relay has acknowledged the hello:"]),
                put(test, 41, "    // a slow runner otherwise races the first handshake."),
                put(test, 42, "    link.settled().await;", commented: true),
                comment(
                    "draft-settled", test,
                    "Is there a timeout on settled()? A link that never settles would hang the test.",
                    .draft, .lines(start: 42, end: 42)),
                kept(test, 42, 43, "    relay.cut(&link);"),
                kept(test, 43, 44, ""),
                kept(test, 44, 45, "    let back = link.reconnected().await?;"),
                gap(test, "end", hidden: nil, down: (46, 65, "async fn reconnect_after_cut")),
            ],
            toFetch: [], opening: false, empty: nil,
            header: header(view: .all, files: 3, added: 18, removed: 6, drafts: 1, sent: 0))
    }

    /// The row the second comment on the first look is being written under:
    /// the test's line after the reconnect.
    private static var firstLookWriting: Int {
        firstLook().rows.firstIndex { $0.key == "l/\(test)/44/45" } ?? 0
    }

    /// The next look, after one round: since the last review the agent put
    /// a timeout round the wait, so the comment sent on that line is
    /// outdated and folded, the one sent on the dial sits with its code as
    /// it was, its code nearby changed, and a new draft is on the new line.
    private static let sinceLast = ReviewPage(
        banners: [],
        rows: [
            fileHead(test, added: 1, removed: 1, changedSinceLast: true, comments: 3),
            comment(
                "sent-settled", test,
                "Is there a timeout on settled()? A link that never settles would hang the test.",
                .sent(round: 1), .outdated, scope: ["async fn reconnect_after_cut"],
                snippet: [
                    line(test, .added, "    // a slow runner otherwise races the first handshake.", new: 41),
                    line(test, .added, "    link.settled().await;", new: 42),
                ],
                folded: true),
            gap(test, "top", hidden: 40, up: (21, 40, nil)),
            comment(
                "sent-dial", test, "Name the host in the dial, so a failure says which one it was.",
                .sent(round: 1), .lines(start: 39, end: 39), changedNear: true,
                scope: ["async fn reconnect_after_cut"],
                snippet: [
                    line(test, .context, "async fn reconnect_after_cut() -> Result<()> {", old: 37, new: 37),
                    line(test, .context, "    let relay = Relay::start().await;", old: 38, new: 38),
                    line(test, .context, "    let link = relay.dial(&host).await?;", old: 39, new: 39),
                ]),
            hunkHead(test, old: 41, new: 41, against: lastRound, scope: ["async fn reconnect_after_cut"]),
            kept(test, 41, 41, "    // a slow runner otherwise races the first handshake."),
            cut(test, 42, "    link.settled().await;", changed: ["link.settled()"]),
            put(test, 42, "    timeout(SETTLE, link.settled()).await??;",
                changed: ["timeout(SETTLE, link.settled())", "??"], commented: true),
            comment(
                "draft-timeout", test, "Good. Say in a comment what SETTLE is sized for.", .draft,
                .lines(start: 42, end: 42)),
            kept(test, 43, 43, "    relay.cut(&link);"),
            kept(test, 44, 44, ""),
            kept(test, 45, 45, "    let back = link.reconnected().await?;"),
            gap(test, "end", hidden: nil, down: (46, 65, "async fn reconnect_after_cut")),
        ],
        toFetch: [], opening: false, empty: nil,
        header: header(
            view: .sinceLast, files: 1, added: 1, removed: 1, drafts: 1, sent: 2, rounds: 1, sinceFiles: 1))

    /// The first round as its chip in the chat opens it: its whole diff,
    /// from main to the round's snapshot, with the two comments it sent on
    /// the lines they were written on.
    private static let sentRound = ReviewPage(
        banners: [],
        rows: [
            fileHead("Cargo.lock", added: 12, removed: 4, flags: [.lockfile], folded: true),
            fileHead(link, added: 3, removed: 0),
            hunkHead(link, old: 120, new: 120, against: base, scope: ["impl Link"]),
            kept(link, 120, 120, "    /// Resolves once the relay acknowledged the hello."),
            put(link, 121, "    pub async fn settled(&self) {"),
            put(link, 122, "        self.hello.wait().await"),
            put(link, 123, "    }"),
            fileHead(test, added: 3, removed: 2, comments: 2),
            hunkHead(test, old: 38, new: 38, against: base, scope: ["async fn reconnect_after_cut"]),
            kept(test, 38, 38, "    let relay = Relay::start().await;"),
            kept(test, 39, 39, "    let link = relay.dial(&host).await?;", commented: true),
            comment(
                "sent-dial", test, "Name the host in the dial, so a failure says which one it was.",
                .sent(round: 1), .lines(start: 39, end: 39), scope: ["async fn reconnect_after_cut"]),
            cut(test, 40, "    // Give the hello time to land.", changed: ["Give the hello time to land."]),
            cut(test, 41, "    tokio::time::sleep(Duration::from_millis(50)).await;"),
            put(test, 40, "    // Cut only once the relay has acknowledged the hello:",
                changed: ["Cut only once the relay has acknowledged the hello:"]),
            put(test, 41, "    // a slow runner otherwise races the first handshake."),
            put(test, 42, "    link.settled().await;", commented: true),
            comment(
                "sent-settled", test,
                "Is there a timeout on settled()? A link that never settles would hang the test.",
                .sent(round: 1), .lines(start: 42, end: 42), scope: ["async fn reconnect_after_cut"]),
            kept(test, 42, 43, "    relay.cut(&link);"),
            kept(test, 43, 44, ""),
            kept(test, 44, 45, "    let back = link.reconnected().await?;"),
        ],
        toFetch: [], opening: false, empty: nil,
        header: header(
            view: .all, files: 3, added: 18, removed: 6, drafts: 0, sent: 2, at: lastRound))

    /// A review with nothing changed and no comments.
    private static let nothingToReview = ReviewPage(
        banners: [], rows: [], toFetch: [], opening: false, empty: .nothing,
        header: header(view: .all, files: 0, added: 0, removed: 0, drafts: 0, sent: 0, allFiles: 0))

    /// The page for an agent whose computer runs no reviews: what the
    /// runtime hands over once the host answers that it offers none.
    private static let notOffered = ReviewPage(
        banners: [.notOffered], rows: [], toFetch: [], opening: false, empty: nil, header: nil)

    private static func header(
        view: ReviewView, files: UInt32, added: UInt32, removed: UInt32, drafts: UInt32, sent: UInt32,
        rounds: UInt32 = 0, allFiles: UInt32 = 3, sinceFiles: UInt32 = 0, at: String = snapshot
    ) -> ReviewHeader {
        ReviewHeader(
            baseBranch: "main", baseCommit: base, snapshot: at, view: view,
            comparedWith: view == .all ? base : lastRound, comparedBase: view == .all ? "" : base,
            views: [ReviewViewTab(view: .all, files: allFiles)]
                + (rounds > 0 ? [ReviewViewTab(view: .sinceLast, files: sinceFiles)] : []),
            rounds: rounds, files: files, added: added, removed: removed, drafts: drafts, sent: sent,
            discardDrafts: drafts > 0, startOver: true)
    }

    /// The chat after the first round went: the person's review sent with
    /// a word, and the agent's answer.
    private static let reviewedRows: [Row] = conversationRows + [
        r("r10", .prompt(
            text: [
                .fixture("Two things on the test."),
                .attachment(.sentReview(
                    comments: 2,
                    review: SentReviewRef(
                        reviewId: Array("review".utf8), round: 1, inputId: Array("r10".utf8)),
                    text: sentText)),
            ],
            steered: false)),
        r("r11", .prose(text: text("""
            Both done: the dial names its host in the error, and the wait for the link to settle is \
            bounded by `SETTLE`, so a link that never settles fails the test instead of hanging it.
            """), streaming: false, workingNote: false)),
    ]

    /// What the agent read for the first round.
    private static let sentText = """
        2 comments from a review of your changes.

        ## crates/relay/tests/reconnect.rs, line 39, in async fn reconnect_after_cut

        ```diff
         async fn reconnect_after_cut() -> Result<()> {
             let relay = Relay::start().await;
             let link = relay.dial(&host).await?;
        ```

        Name the host in the dial, so a failure says which one it was.

        ## crates/relay/tests/reconnect.rs, line 42, in async fn reconnect_after_cut

        ```diff
        +    // a slow runner otherwise races the first handshake.
        +    link.settled().await;
        ```

        Is there a timeout on settled()? A link that never settles would hang the test.

        ---
        Line numbers are those of snapshot \(lastRound), a commit of the working tree taken for this \
        review and kept outside the repository, so the files may have changed since. Paths are \
        relative to the repository's top, ~/src/amux. To see what has changed since the snapshot, \
        untracked files included, run this; it writes nothing, and after the diff it lists the files \
        made since the snapshot:

        ```sh
        GIT_INDEX_FILE=~/.amux/agents/flaky/review/index GIT_ALTERNATE_OBJECT_DIRECTORIES=~/.amux/agents/flaky/review/objects git -C ~/src/amux diff \(lastRound) && GIT_INDEX_FILE=~/.amux/agents/flaky/review/index git -C ~/src/amux ls-files --others --exclude-standard
        ```
        """

    private static func fileHead(
        _ path: String, added: UInt32, removed: UInt32, flags: [ReviewFileFlag] = [], folded: Bool = false,
        changedSinceLast: Bool = false, comments: UInt32 = 0
    ) -> ReviewRow {
        ReviewRow(
            key: "f/\(path)",
            of: .file(ReviewFileHead(
                path: path, status: .modified, added: added, removed: removed, binary: false,
                language: path.hasSuffix(".rs") ? "rust" : "", flags: flags, folded: folded,
                changedSinceLast: changedSinceLast, inChange: true, comments: comments, outline: [], oldPath: nil)))
    }

    private static func hunkHead(_ path: String, old: UInt32, new: UInt32, against: String, scope: [String]) -> ReviewRow {
        ReviewRow(
            key: "h/\(path)/\(old)/\(new)",
            of: .hunk(ReviewHunkHead(path: path, oldCommit: against, oldStart: old, newStart: new, scope: scope)))
    }

    /// Lines of the snapshot not shown: `down` and `up` load from the code
    /// above and below, each its first and last line and the definition
    /// whose edge it reaches.
    private static func gap(
        _ path: String, _ at: String, hidden: UInt32?, down: (UInt32, UInt32, String?)? = nil,
        up: (UInt32, UInt32, String?)? = nil
    ) -> ReviewRow {
        let ask = { (lines: (UInt32, UInt32, String?)) in
            ReviewContextAsk(path: path, commit: snapshot, start: lines.0, end: lines.1, definition: lines.2)
        }
        return ReviewRow(
            key: "g/\(path)/\(at)",
            of: .gap(ReviewGap(path: path, loading: false, down: down.map(ask), hidden: hidden, up: up.map(ask))))
    }

    private static func comment(
        _ id: String, _ path: String, _ words: String, _ state: ReviewCommentState, _ at: ReviewCommentAt,
        changedNear: Bool = false, scope: [String] = [], snippet: [ReviewCodeLine] = [], folded: Bool = false
    ) -> ReviewRow {
        ReviewRow(
            key: "c/\(id)",
            of: .comment(ReviewCommentRow(
                id: Array(id.utf8), path: path, text: words, state: state, at: at, changedNear: changedNear,
                scope: scope, snippet: snippet, folded: folded)))
    }

    private static func kept(
        _ path: String, _ old: UInt32, _ new: UInt32, _ code: String, commented: Bool = false
    ) -> ReviewRow {
        ReviewRow(
            key: "l/\(path)/\(old)/\(new)",
            of: .line(line(path, .context, code, old: old, new: new, commented: commented)))
    }

    private static func put(
        _ path: String, _ new: UInt32, _ code: String, changed: [String] = [], commented: Bool = false
    ) -> ReviewRow {
        ReviewRow(
            key: "l/\(path)/+\(new)",
            of: .line(line(path, .added, code, new: new, changed: changed, commented: commented)))
    }

    private static func cut(_ path: String, _ old: UInt32, _ code: String, changed: [String] = []) -> ReviewRow {
        ReviewRow(key: "l/\(path)/-\(old)", of: .line(line(path, .removed, code, old: old, changed: changed)))
    }

    private static func line(
        _ path: String, _ kind: LineKind, _ code: String, old: UInt32? = nil, new: UInt32? = nil,
        changed: [String] = [], commented: Bool = false
    ) -> ReviewCodeLine {
        ReviewCodeLine(
            path: path, kind: kind, text: code, runs: rust(code, changed: changed), commented: commented,
            extra: false, newLine: new, oldLine: old)
    }

    private static let keywords: Set<String> = [
        "as", "async", "await", "fn", "for", "if", "impl", "in", "let", "match", "mut", "pub", "return", "self",
        "struct", "use",
    ]

    /// Rust as the host's highlighter would colour it, near enough for a
    /// story: keywords, calls, types, numbers, strings and comments; the
    /// stretches in `changed` marked as the words that differ.
    private static func rust(_ code: String, changed: [String]) -> [ReviewRun] {
        let chars = Array(code)
        var kinds = [TokenKind](repeating: .plain, count: chars.count)
        var at = 0
        while at < chars.count {
            let c = chars[at]
            if c == "/", at + 1 < chars.count, chars[at + 1] == "/" {
                for i in at..<chars.count { kinds[i] = .comment }
                break
            } else if c == "\"" {
                var end = at + 1
                while end < chars.count, chars[end] != "\"" { end += 1 }
                for i in at...min(end, chars.count - 1) { kinds[i] = .string }
                at = end + 1
            } else if c.isLetter || c == "_" {
                var end = at
                while end < chars.count, chars[end].isLetter || chars[end].isNumber || chars[end] == "_" { end += 1 }
                let word = String(chars[at..<end])
                let kind: TokenKind =
                    keywords.contains(word)
                    ? .keyword
                    : (end < chars.count && (chars[end] == "(" || chars[end] == "!"))
                        ? .function : (c.isUppercase ? .type : .plain)
                for i in at..<end { kinds[i] = kind }
                at = end
            } else if c.isNumber {
                kinds[at] = .number
                at += 1
            } else {
                at += 1
            }
        }
        var marked = [Bool](repeating: false, count: chars.count)
        for stretch in changed {
            guard let range = code.range(of: stretch) else { continue }
            let start = code.distance(from: code.startIndex, to: range.lowerBound)
            for i in start..<(start + stretch.count) { marked[i] = true }
        }
        var runs: [ReviewRun] = []
        for (index, char) in chars.enumerated() {
            if let last = runs.last, last.kind == kinds[index], last.changed == marked[index] {
                runs[runs.count - 1].text.append(char)
            } else {
                runs.append(ReviewRun(text: String(char), kind: kinds[index], changed: marked[index]))
            }
        }
        return runs
    }

    // MARK: - Settings

    private static let machineSettings = MachineSettings(
        hostName: machine, keepAwake: true,
        repositories: [home("/src/amux"), home("/src/website")], telemetry: true, autoReports: false,
        experiments: [
            ExperimentView(
                name: "workspaces", version: "workspaces/1",
                description: "Run each agent in a workspace that amux makes, starts and stops with its agents.",
                enabled: true, applying: false),
            ExperimentView(
                name: "suspension", version: "suspension/1",
                description: "Suspend idle agents and resume them when a message arrives.",
                enabled: false, applying: false),
        ],
        startAtLogin: true)

    /// Suspension just switched on, while amux restarts to run it.
    private static var applyingSettings: MachineSettings {
        var settings = machineSettings
        settings.experiments[1].enabled = true
        settings.experiments[1].applying = true
        return settings
    }

    private static let settings = [
        reviewStory("review.page", chip: ReviewChip(comments: 1, moved: false)) {
            // A second comment being written under the draft's lines.
            StillReviewPage(
                page: firstLook(), selected: firstLookWriting...firstLookWriting,
                words: "Assert the reconnect lands within the backoff's first step, too.")
        },
        reviewStory("review.since-last", rows: reviewedRows, chip: ReviewChip(comments: 1, moved: false)) {
            StillReviewPage(page: sinceLast)
        },
        reviewStory("review.drafts-stale", chip: ReviewChip(comments: 1, moved: true)) {
            StillReviewPage(page: firstLook(banners: [.changedSinceYouStarted]))
        },
        reviewStory("review.empty") { StillReviewPage(page: nothingToReview) },
        // A host with no reviews: the page says so, with nothing of the
        // review to draw.
        reviewStory("review.not-offered") { StillReviewPage(page: notOffered) },
        Story("review.round", window: .sheet) {
            ReviewRoundSheet(
                round: 1, text: sentText, page: sentRound, read: { sentRound }, openReview: {}, close: {})
        },
        Story("settings.machine", window: .settings) {
            SettingsScreen(state: .ready(machineSettings), account: SettingsAccount(profile: signedIn))
        },
        Story("settings.machine-signed-out", window: .settings) {
            SettingsScreen(state: .ready(machineSettings), account: SettingsAccount(profile: work))
        },
        Story("settings.experiment-applying", window: .settings) {
            // Scrolled to the experiments, at the end of the form.
            SettingsScreen(state: .ready(applyingSettings), account: SettingsAccount(profile: signedIn))
                .defaultScrollAnchor(.bottom)
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
    /// The content, with the open agent's branch, changes and relatives
    /// for the bars, as the app reads them off its chat.
    private var opened: MainContent {
        var opened = content
        if place.surface == .agents, place.agent != nil {
            opened.opened = OpenAgentBar(
                branch: chat?.frame?.git?.branch, changes: chat?.changes, family: family, actions: actions)
        }
        return opened
    }

    private var actions: AgentActions {
        AgentActions(
            openChanges: { place.changes = true }, reveal: {}, openEditor: {}, editor: "VS Code",
            editorIcon: appIcon("com.microsoft.VSCode"), terminalIcon: appIcon("com.apple.Terminal"),
            finderIcon: appIcon("com.apple.finder"))
    }

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
        MainScreen(content: opened, place: $place) {
            if place.surface == .agents, let chat, let agent = place.agent {
                ChatPane(chat: chat, row: content.row(agent), actions: actions)
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
            if let chat, content.showsChanges(place) {
                ChangesPane(chat: chat)
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

extension Capabilities {
    /// What headless Claude, Codex and an ACP agent that reopens its
    /// conversations can do, for the stories.
    static let claude = Capabilities(
        stopsJob: true, switchesBack: false, attachable: false, resumable: true, sendsNow: true, images: true,
        files: true, passesUnknownCommands: true)
    static let codex = Capabilities(
        stopsJob: true, switchesBack: false, attachable: true, resumable: true, sendsNow: true, images: true,
        files: true, passesUnknownCommands: false)
    static let acp = Capabilities(
        stopsJob: false, switchesBack: false, attachable: false, resumable: true, sendsNow: false, images: true,
        files: true, passesUnknownCommands: true)
}
