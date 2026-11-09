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
    public static let all: [Story] = launch + setup + fleet + settings

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

    private static func card(_ number: UInt8, _ name: String, on host: String, _ attention: Attention) -> FleetCard {
        FleetCard(
            agent: AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: number, count: 16)),
            name: name, handle: name, kind: .claudeSdk, attention: attention, cwd: "/src/amux", phaseSinceMs: 0,
            host: host, hostReach: .online(.direct), children: 0, familyAttention: attention, members: 1,
            descendants: 0, backgroundJobs: 0, closeHeld: false, filing: .open, topLevel: true, branch: "main",
            exitCause: nil)
    }

    private static let agents = [
        card(1, "Fix the flaky relay test", on: machine, .needsYou),
        card(2, "Write the release notes", on: machine, .working),
        card(3, "Tidy the settings page", on: "build-box", .working),
        card(4, "Review the store migration", on: "build-box", .idle),
        card(5, "Bump the toolchain", on: machine, .exited),
    ]

    private static let fleet = [
        Story("fleet.no-agents") { MainStory(MainContent(profiles: [signedIn, work], agents: [])) },
        Story("fleet.busy") { MainStory(MainContent(profiles: [signedIn, work], agents: agents)) },
        Story("fleet.finish-setup") {
            MainStory(MainContent(profiles: [signedIn, work], agents: Array(agents.prefix(2)), toFinish: 3))
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

/// The main window with its first profile open and rows that can be
/// selected in the storybook.
private struct MainStory: View {
    let content: MainContent
    @State private var selected: String?
    @State private var agent: AgentKey?

    init(_ content: MainContent) {
        self.content = content
        _selected = State(initialValue: content.profiles.first?.id)
    }

    var body: some View { MainScreen(content: content, selected: $selected, selectedAgent: $agent) }
}
