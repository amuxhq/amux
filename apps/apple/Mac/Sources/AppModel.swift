import AmuxCore
import AmuxMacScreens
import AmuxValues
import AmuxWords
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
    /// What the open profile's screens draw: its fleet, hosts, chats and
    /// the New Agent page, in the stores the phone draws from too.
    private(set) var stores: StoreBundle?
    /// Where the main window is: its surface, the agent or host open, and
    /// which panes show.
    var place = Place() {
        didSet {
            guard place.agent != oldValue.agent else { return }
            showChat(place.agent, leaving: oldValue.agent)
            // Leaving the new-agent page leaves what was typed there.
            if place.agent != nil { starting = nil }
        }
    }
    /// The open agent's chat, while one is open.
    private(set) var chat: MacChat?
    /// The new-agent page's store, while the page is open.
    var starting: MacNewAgent?
    @ObservationIgnored private var firstMessage: Task<Void, Never>?
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
            tickAges()
            // The first profile list reaches the main actor while attaching,
            // before there was a runtime to open its first profile from.
            open(selected)
        } catch {
            let reason = (error as? RuntimeFailure)?.description ?? String(describing: error)
            Self.log.error("attach failed: \(reason, privacy: .public)")
            phase = .failed(reason)
        }
    }

    /// Moves the fleet's clock on every half minute, so a row's age counts
    /// while the window shows it.
    private func tickAges() {
        Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(30))
                guard let self else { return }
                self.stores?.fleet.refreshOrder(now: Date())
            }
        }
    }

    /// The daemon's profile list moved.
    private func show(_ listed: [ProfileView]) {
        profiles = listed
        Self.log.notice("profiles: \(listed.count, privacy: .public)")
        if let stores, let account = listed.first(where: { $0.id == selected })?.account {
            stores.show(account: account)
        }
        if selected.map({ id in !listed.contains { $0.id == id } }) ?? true {
            selected = listed.first?.id
        }
    }

    /// Puts a profile's fleet on screen in place of the one there.
    private func open(_ id: String?) {
        setup?.close()
        setup = nil
        chat = nil
        stores?.closeChats()
        stores?.attach(nil)
        stores = nil
        profile?.close()
        profile = nil
        place.agent = nil
        place.host = nil
        place.settled = false
        guard let id, let runtime else { return }
        let stores = StoreBundle(account: AccountId(id))
        if let account = profiles.first(where: { $0.id == id })?.account {
            stores.show(account: account)
        }
        // Told each time the fleet lands, which can be a moment after it
        // moved while the window's first frame goes up.
        stores.saw = { [weak self] _, _ in self?.setup?.fleetMoved() }
        self.stores = stores
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
                stores.attach(opened)
                Self.log.notice("agents: \(stores.fleet.rows.count, privacy: .public)")
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
        case .fleet, .chat: stores?.moved(moved)
        case .signInOffers(let offers):
            stores?.moved(moved)
            setup?.offersMoved(offers)
        case .onboarding(let state): setup?.stepsMoved(state)
        case .pairing(let status): setup?.pairingMoved(status)
        }
    }

    /// The open profile's agents, in the order its fleet lists them.
    var agents: [FleetCard] { stores?.fleet.rows.map(\.card) ?? [] }

    /// The machines the open profile trusts, this Mac apart.
    var hosts: [HostView] { stores?.hosts.hosts ?? [] }

    /// The agents on this Mac.
    var localAgents: [AgentKey] {
        guard let local = stores?.hosts.local?.hostId else { return [] }
        return agents.map(\.agent).filter { $0.host == local }
    }

    /// The profile on screen, as the daemon lists it.
    var current: ProfileView? { profiles.first { $0.id == selected } }

    /// Selects an agent in the list, saying why.
    func show(_ agent: AgentKey, saying note: String?) {
        place.surface = .agents
        place.agent = agent
        self.note = note
    }

    /// Opens the agent's chat in the centre, leaving the one there: its
    /// session stays open a while, so going back to it is instant.
    private func showChat(_ agent: AgentKey?, leaving old: AgentKey?) {
        if let old { stores?.leave(old) }
        chat = nil
        guard let agent, let stores else { return }
        do {
            let model = try stores.chat(agent)
            let host = self.row(agent).map { PlaceNames.host($0.hostName) } ?? "its device"
            chat = MacChat(model: model, host: host)
        } catch {
            problem = error.description
        }
    }

    /// The open agent's parent and children.
    func family(_ agent: AgentKey) -> FamilyHeader? {
        _ = stores?.applied
        return profile?.family(agent)
    }

    /// An agent's row: the sidebar's, or for a family's member, which the
    /// sidebar leaves to its head, one made from its card.
    func row(_ agent: AgentKey) -> FleetRow? {
        stores?.fleet.row(agent) ?? member(agent)
    }

    /// A member of a family, as a row: the runtime lists only heads.
    func member(_ agent: AgentKey) -> FleetRow? {
        _ = stores?.applied
        guard let card = profile?.card(agent) else { return nil }
        return FleetRow(card: card, depth: 1, expanded: false, secondLine: .blank, loud: nil)
    }

    /// The head of the family an agent is in: its eldest ancestor this Mac
    /// knows, whose row stands for it in the sidebar.
    func head(of agent: AgentKey) -> AgentKey? {
        _ = stores?.applied
        var at = agent
        var seen: Set<AgentKey> = [agent]
        while let parent = profile?.family(at)?.parent?.agent, seen.insert(parent).inserted {
            at = parent
        }
        return at == agent ? nil : at
    }

    /// Opens the new-agent page, on this Mac to begin with.
    func newAgent() {
        place.surface = .agents
        place.agent = nil
        prepareNewAgent()
    }

    /// Readies the new-agent page's store, once per visit: its host's
    /// folders and offers are asked for as it opens.
    func prepareNewAgent() {
        guard starting == nil, let stores else { return }
        stores.startNewAgent(on: stores.hosts.local?.id)
        starting = MacNewAgent(stores: stores)
    }

    /// Whether the changes pane shows for the agent open now: a provider
    /// subagent has none of its own.
    var changesShown: Bool {
        place.showsChanges(native: place.agent.flatMap { row($0) }?.native == true)
    }

    /// Opens an agent's chat from elsewhere: the host page, a relative on
    /// the card, or the new-agent page once its agent started.
    func openChat(_ agent: AgentKey) {
        place.surface = .agents
        place.agent = agent
    }

    /// The new-agent page's agent started: its chat opens and the first
    /// message goes to it as soon as the chat can take it.
    func started(_ agent: AgentKey) {
        let prompt = starting?.prompt.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        starting = nil
        openChat(agent)
        guard !prompt.isEmpty else { return }
        firstMessage = Task { [weak self] in
            for _ in 0..<300 {
                guard let self, !Task.isCancelled, place.agent == agent else { return }
                if let chat, chat.model.frame?.caughtUp == true, chat.model.frame?.composer.mode == .send {
                    chat.draft = prompt
                    chat.send()
                    return
                }
                try? await Task.sleep(for: .milliseconds(100))
            }
        }
    }

    // MARK: - Opening an agent elsewhere

    /// Whether the agent has a terminal interface of its own to attach to
    /// here: a provider subagent has no interface of its own, an agent whose
    /// program runs only behind amux's chat has none, and one on another
    /// computer has its interface only there, where `amux attach` refuses
    /// to open it from this Mac.
    func attachable(_ agent: AgentKey) -> Bool {
        runsHere(agent) && (row(agent).map { !$0.native && $0.card.attachable } ?? false)
    }

    /// The agent runs on this Mac.
    func runsHere(_ agent: AgentKey) -> Bool {
        stores?.hosts.local.map { $0.hostId == agent.host } ?? false
    }

    /// The agent's own interface in Terminal: a script that runs
    /// `amux attach` there, opened as Terminal opens any `.command` file.
    func openTerminal(_ agent: AgentKey) {
        guard attachable(agent), let row = self.row(agent) else { return }
        var script = "#!/bin/zsh -l\n"
        if let config = Self.config { script += "export AMUX_CONFIG=\(Self.quoted(config))\n" }
        script += "exec amux attach \(Self.quoted(row.card.handle)) --in native\n"
        let url = FileManager.default.temporaryDirectory.appending(path: "amux-attach-\(row.card.handle).command")
        do {
            try script.write(to: url, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
            NSWorkspace.shared.open(url)
        } catch {
            problem = "Couldn’t open Terminal: \(error.localizedDescription)"
        }
    }

    private static func quoted(_ text: String) -> String {
        "'" + text.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    /// The editor a developer most likely uses, among those installed.
    static let editor: (name: String, url: URL)? = {
        let known = [
            ("VS Code", "com.microsoft.VSCode"), ("Cursor", "com.todesktop.230313mzl4w4u92"), ("Zed", "dev.zed.Zed"),
            ("Sublime Text", "com.sublimetext.4"), ("Xcode", "com.apple.dt.Xcode"),
        ]
        for (name, id) in known {
            if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: id) { return (name, url) }
        }
        return nil
    }()

    /// An installed app's own icon, for the line that opens it.
    static func appIcon(_ bundle: String) -> NSImage? {
        NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundle).map {
            NSWorkspace.shared.icon(forFile: $0.path)
        }
    }

    /// What the card offers for an agent on this Mac: its folder in Finder
    /// and in the editor. An agent elsewhere has neither here.
    func local(_ agent: AgentKey) -> String? {
        guard let local = stores?.hosts.local?.hostId, agent.host == local,
            let row = self.row(agent), !row.workingDirectory.isEmpty
        else { return nil }
        return row.workingDirectory
    }

    func reveal(_ path: String) {
        NSWorkspace.shared.activateFileViewerSelecting([URL(filePath: path)])
    }

    func openInEditor(_ path: String) {
        guard let editor = Self.editor else { return reveal(path) }
        NSWorkspace.shared.open(
            [URL(filePath: path)], withApplicationAt: editor.url, configuration: NSWorkspace.OpenConfiguration())
    }

    func dismissProblem() { problem = nil }

    /// Stops trusting a paired host, leaving its page.
    func revoke(_ host: [UInt8]) {
        if let id = HostId(bytes: host) { stores?.revoke(id) }
        if place.host == host { place.host = nil }
    }

    /// An agent the person asked to settle when settling it cuts work short
    /// or ends a conversation nothing can reopen, so settling it waits for
    /// them to say yes.
    var settling: AgentKey?

    /// What settling `agent` costs, as its question says it, by the
    /// fleet's shared rule; nil when settling it goes at once.
    func settleQuestion(_ agent: AgentKey) -> String? {
        guard let warning = stores?.settleWarning(agent), warning.asks else { return nil }
        let row = row(agent)
        let cuts = ChatWords.closeCuts(name: row?.name ?? agent.description, warning)
        let after = ChatWords.settleEndsForGood(provider: row?.card.providerName ?? "", warning)
            ?? "Its chat and edits stay, and a message resumes it."
        return [cuts, "Settling stops it now.", after].compactMap { $0 }.joined(separator: " ")
    }

    /// Acts as the person asked, except that settling is asked about first
    /// when it cuts work short or ends the conversation for good.
    func request(_ act: AgentAct, on agent: AgentKey) async {
        if act == .close, settleQuestion(agent) != nil {
            settling = agent
            return
        }
        await self.act(act, on: agent)
    }

    /// Pins, settles, renames or deletes an agent; a refusal is said on the
    /// window.
    func act(_ act: AgentAct, on agent: AgentKey) async {
        guard let stores else { return }
        let outcome: Result<Nothing, RuntimeFailure> =
            switch act {
            case .rename(let name): await stores.rename(name, of: agent)
            case .close: await stores.close(agent)
            case .cancelClose: await stores.cancelClose(agent)
            case .pin: await stores.pin(agent)
            case .unfile: await stores.unfile(agent)
            case .delete: await stores.delete(agent)
            case .deleteDiscardingChanges: await stores.delete(agent, discardingChanges: true)
            }
        switch outcome {
        case .success:
            if act == .delete || act == .deleteDiscardingChanges, place.agent == agent { place.agent = nil }
        case .failure(let error): problem = error.description
        }
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
        guard let runtime else { return "amux isn’t running" }
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
                return .failed("Sign-in didn’t finish. Try again.")
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
