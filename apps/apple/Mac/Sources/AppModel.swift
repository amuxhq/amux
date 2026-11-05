import AmuxCore
import AmuxValues
import AppKit
import Foundation
import Observation
import os

/// The Mac app's whole state: amux installed and attached, the daemon's
/// profiles, the one on screen and its agents, the installation's settings
/// and the profile's setting up.
///
/// Everything is amux's. The app installs nothing of its own beyond amux,
/// keeps no store, and signs in only to hand the account to the daemon,
/// which holds the refresh token from then on.
@MainActor
@Observable
final class AppModel {
    enum Phase: Equatable {
        case installing
        case attaching
        case attached
        case failed(String)
    }

    private(set) var phase: Phase = .installing
    private(set) var profiles: [ProfileView] = []
    /// The profile on screen.
    var selected: String? {
        didSet { if selected != oldValue { open(selected) } }
    }
    /// The open profile's agents, in the order its fleet lists them.
    private(set) var agents: [FleetCard] = []
    /// Every machine the open profile knows, this Mac among them.
    private(set) var hosts: [HostView] = []
    /// The agent selected in the list.
    var selectedAgent: AgentKey?
    /// The installation's settings, as the daemon last answered them.
    private(set) var settings: MachineSettings?
    /// Setting up the open profile.
    private(set) var setup: Setup?
    /// Said once about something that just happened, such as the agent the
    /// phone started.
    private(set) var note: String?
    private(set) var signingIn = false
    private(set) var problem: String?
    /// A newer version of the app, staged and swapped in when the app quits.
    private(set) var update: AppUpdate?

    @ObservationIgnored private(set) var runtime: Runtime?
    @ObservationIgnored private var profile: Profile?
    @ObservationIgnored private var checking: Task<Void, Never>?
    @ObservationIgnored private let cloud = AmuxCloudService(endpoint: AppModel.endpoint)
    private static let log = Logger(subsystem: "sh.amux.desktop", category: "app")

    /// The account service the app signs in at. `AMUX_CLOUD` points a
    /// development build at another one, as it does the daemon.
    private static var endpoint: CloudEndpoint {
        var endpoint = CloudEndpoint.desktop
        if let base = environment("AMUX_CLOUD").flatMap(URL.init(string:)) {
            endpoint.base = base
        }
        return endpoint
    }

    /// The installation's config file: the default one unless `AMUX_CONFIG`
    /// names another, as it does for every amux command.
    private static var config: String? { environment("AMUX_CONFIG") }

    private static func environment(_ name: String) -> String? {
        ProcessInfo.processInfo.environment[name].flatMap { $0.isEmpty ? nil : $0 }
    }

    /// Installs amux when this Mac has none, starts it when it is stopped,
    /// and attaches to it.
    func start() async {
        guard runtime == nil else { return }
        // Before anything that can fail: an app that cannot install or
        // attach to the amux it finds may be the one a newer app fixes.
        checkForUpdates()
        phase = .installing
        do {
            // An installation `AMUX_CONFIG` names is a checkout's or a
            // test's, run by its own build: the app attaches to it or says it
            // is down, and never installs the default amux for it or starts
            // the installed one on its root.
            if Self.config == nil {
                let installed = try await Runtime.install(InstallOptions(config: nil))
                Self.log.notice("amux: \(String(describing: installed), privacy: .public)")
            }
            phase = .attaching
            let config = AttachConfig(config: Self.config, tail: nil)
            let attached = try await Task.detached { () throws(RuntimeFailure) -> Runtime in
                try Runtime.attach(config) { [weak self] listed in self?.show(listed) }
            }.value
            runtime = attached
            phase = .attached
            // The first profile list reaches the main actor while attaching,
            // before there was a runtime to open its first profile from.
            open(selected)
        } catch {
            let reason = (error as? RuntimeFailure)?.description ?? String(describing: error)
            Self.log.error("attach failed: \(reason, privacy: .public)")
            phase = .failed(reason)
        }
    }

    /// The daemon's profile list moved.
    private func show(_ listed: [ProfileView]) {
        profiles = listed
        Self.log.notice("profiles: \(listed.count, privacy: .public)")
        if selected.map({ id in !listed.contains { $0.id == id } }) ?? true {
            selected = listed.first?.id
        }
    }

    /// Puts a profile's fleet on screen in place of the one there.
    private func open(_ id: String?) {
        setup?.close()
        setup = nil
        profile?.close()
        profile = nil
        agents = []
        hosts = []
        selectedAgent = nil
        guard let id, let runtime else { return }
        Task {
            let opened = await Task.detached { () -> Result<Profile, RuntimeFailure> in
                Result { () throws(RuntimeFailure) in
                    try runtime.open(id) { [weak self] moved in self?.moved(moved, in: id) }
                }
            }.value
            switch opened {
            case .success(let opened):
                // Another profile was picked while this one opened.
                guard selected == id else { opened.close(); return }
                profile = opened
                list(opened.fleetOpening())
                setup = Setup(app: self, profile: opened)
                // The steps arrive with a later step; the screens they open
                // name this Mac by its settings.
                await loadSettings()
                opened.watchOnboarding()
            case .failure(let error):
                Self.log.error("open failed: \(error.description, privacy: .public)")
                problem = error.description
            }
        }
    }

    private func moved(_ moved: Moved, in id: String) {
        guard selected == id else { return }
        switch moved {
        case .fleet(let delta): list(delta)
        case .chat: break
        case .signInOffers(let offers): setup?.offersMoved(offers)
        case .onboarding(let state): setup?.stepsMoved(state)
        case .pairing(let status): setup?.pairingMoved(status)
        }
    }

    private func list(_ delta: FleetDelta) {
        if let hosts = delta.hosts { self.hosts = hosts }
        guard let view = delta.view else {
            if delta.hosts == nil { Self.log.notice("fleet: no view yet") }
            return
        }
        agents = view.sections.flatMap(\.rows).map(\.card)
        Self.log.notice("agents: \(self.agents.count, privacy: .public)")
        setup?.fleetMoved()
    }

    /// The agents on this Mac.
    var localAgents: [AgentKey] {
        guard let local = hosts.first(where: \.local)?.hostId else { return [] }
        return agents.map(\.agent).filter { $0.host == local }
    }

    /// The profile on screen, as the daemon lists it.
    var current: ProfileView? { profiles.first { $0.id == selected } }

    /// Selects an agent in the list, saying why.
    func show(_ agent: AgentKey, saying note: String?) {
        selectedAgent = agent
        self.note = note
    }

    // MARK: - Settings

    /// Reads the installation's settings again.
    func loadSettings() async {
        guard let runtime else { return }
        switch await runtime.settings() {
        case .success(let settings): self.settings = settings
        case .failure(let error):
            Self.log.error("settings: \(error.description, privacy: .public)")
        }
    }

    /// Changes the installation's settings, each in force at once; answers
    /// why it did not, or nil once it did.
    @discardableResult
    func apply(_ change: SettingsChange) async -> String? {
        guard let runtime else { return "amux is not running" }
        switch await runtime.updateSettings(change) {
        case .success(let settings):
            self.settings = settings
            return nil
        case .failure(let error):
            Self.log.error("settings change: \(error.description, privacy: .public)")
            return error.description
        }
    }

    /// Opens a page of the profile's account service in the browser,
    /// already signed in, so the browser needs no sign-in of its own.
    func openAccountPage(_ path: String, for profile: String) async {
        guard let runtime else { return }
        switch await runtime.webSignInLink(profile, path: path) {
        case .success(let link):
            if let url = URL(string: link) { NSWorkspace.shared.open(url) }
        case .failure(let error):
            Self.log.error("account page: \(error.description, privacy: .public)")
            problem = error.description
        }
    }

    /// What a browser sign-in came to.
    enum SignedIn: Equatable {
        case signedIn
        /// The person closed the browser without finishing.
        case cancelled
        case failed(String)
    }

    /// Signs the profile on screen in at amux.sh in the system's browser and
    /// hands the account to the daemon. The profile is the one the person
    /// is setting up or looking at, so what it already holds, such as the
    /// phone just paired, is theirs and stays with it.
    @discardableResult
    func signIn() async -> SignedIn {
        guard let runtime, let selected, !signingIn else { return .cancelled }
        signingIn = true
        problem = nil
        defer { signingIn = false }
        do {
            let account = try await cloud.signIn(.adding, presenting: WebSignIn())
            guard let handover = await cloud.handOver(account.id) else {
                return .failed("amux.sh signed in without a token to keep")
            }
            await cloud.forgetSession(account.id)
            switch await runtime.bind(
                selected, cloud: handover.cloud, client: handover.client,
                refreshToken: handover.refreshToken)
            {
            case .success: return .signedIn
            case .failure(let error): return .failed(error.description)
            }
        } catch .cancelled {
            return .cancelled
        } catch {
            return .failed(String(describing: error))
        }
    }

    /// Signs in from the main window or Settings, where a failure is said
    /// on the window.
    func signInFromMenu() async {
        if case .failed(let why) = await signIn() { problem = why }
    }

    /// How often the app looks for a newer version of itself, as often as
    /// amux's supervisor looks for one of amux.
    private static let updateInterval: Duration = .seconds(60 * 60)

    /// Looks for a newer app now and every hour after, while there is
    /// nothing staged. An app that could not replace its own bundle (opened
    /// from its disk image, or moved by Gatekeeper's App Translocation)
    /// looks for none.
    private func checkForUpdates() {
        guard checking == nil, Self.replaceable else { return }
        let check = UpdateCheck(current: Self.version, dir: Self.updates.path, config: Self.config)
        checking = Task { [weak self] in
            while !Task.isCancelled {
                do {
                    if let found = try await Runtime.checkForUpdate(check) {
                        Self.log.notice("update staged: \(found.version, privacy: .public)")
                        self?.update = found
                        return
                    }
                } catch {
                    let reason = (error as? RuntimeFailure)?.description ?? String(describing: error)
                    Self.log.error("update check failed: \(reason, privacy: .public)")
                }
                try? await Task.sleep(for: Self.updateInterval)
            }
        }
    }

    /// Quits and opens the updated app once this one has gone.
    func restartToUpdate() {
        guard update != nil else { return }
        // The new app opens with the installation and account service this
        // one was given, so a checkout's app relaunches onto the checkout's
        // daemon rather than the default installation.
        let carried = ["AMUX_CONFIG", "AMUX_CLOUD"].compactMap { name in
            Self.environment(name).map { ["--env", "\(name)=\($0)"] }
        }.flatMap(\.self)
        let relaunch = Process()
        relaunch.executableURL = URL(filePath: "/bin/sh")
        relaunch.arguments = [
            "-c",
            #"pid=$1; app=$2; shift 2; while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open "$@" "$app""#,
            "relaunch", String(ProcessInfo.processInfo.processIdentifier),
            Bundle.main.bundleURL.path,
        ] + carried
        do {
            try relaunch.run()
        } catch {
            Self.log.error("relaunch failed: \(error.localizedDescription, privacy: .public)")
        }
        NSApplication.shared.terminate(nil)
    }

    /// Closes what setting up holds open, such as a pairing window. Called
    /// as the app quits.
    func close() {
        setup?.close()
    }

    /// Puts the staged app in place of this one, which keeps running from
    /// the files it has open until it exits. Called as the app quits.
    func applyUpdate() {
        guard let update else { return }
        do {
            _ = try FileManager.default.replaceItemAt(
                Bundle.main.bundleURL, withItemAt: URL(filePath: update.app))
            Self.log.notice("updated to \(update.version, privacy: .public)")
        } catch {
            Self.log.error("update failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    /// The running app's version, as its bundle carries it.
    private static var version: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.0.0"
    }

    /// Where updates are staged.
    private static var updates: URL {
        URL.applicationSupportDirectory.appending(path: "sh.amux.desktop/Updates")
    }

    /// Whether this app can replace its own bundle.
    private static var replaceable: Bool {
        let bundle = Bundle.main.bundleURL
        return !bundle.path.contains("/AppTranslocation/")
            && FileManager.default.isWritableFile(atPath: bundle.deletingLastPathComponent().path)
    }

    /// Signs the profile on screen out; it stays tied to its account.
    func signOut() async {
        guard let runtime, let selected else { return }
        if case .failure(let error) = await runtime.signOut(selected) {
            problem = error.description
        }
    }
}
