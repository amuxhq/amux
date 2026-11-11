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
    public static let all: [Story] = launch + setup + fleet + hosts + settings

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
/// can move around.
private struct MainStory: View {
    let content: MainContent
    @State private var place: Place

    init(_ content: MainContent, place: Place = Place()) {
        self.content = content
        _place = State(initialValue: place)
    }

    var body: some View {
        MainScreen(content: content, place: $place) {
            EmptyCentre(surface: place.surface)
        }
    }
}
