import AmuxCore
import AmuxValues
import AppKit
import Foundation
import Observation
import os

/// The Mac app's whole state: amux installed and attached, the daemon's
/// profiles, the one on screen and its agents.
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
    /// The open profile's agents, by name, in the order its fleet lists them.
    private(set) var agents: [String] = []
    private(set) var signingIn = false
    private(set) var problem: String?
    /// A newer version of the app, staged and swapped in when the app quits.
    private(set) var update: AppUpdate?

    @ObservationIgnored private var runtime: Runtime?
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
        let bundled = Bundle.main.bundleURL.appending(path: "Contents/Helpers/amux")
        do {
            // An installation `AMUX_CONFIG` names is a checkout's or a
            // test's, run by its own build: the app attaches to it or says it
            // is down, and never installs the default amux for it or starts
            // the installed one on its root.
            if Self.config == nil {
                let installed = try await Runtime.install(InstallOptions(
                    bundled: FileManager.default.isExecutableFile(atPath: bundled.path)
                        ? bundled.path : nil,
                    config: nil,
                    loginItem: true))
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
        profile?.close()
        profile = nil
        agents = []
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
                list(opened.fleetOpening().view)
            case .failure(let error):
                Self.log.error("open failed: \(error.description, privacy: .public)")
                problem = error.description
            }
        }
    }

    private func moved(_ moved: Moved, in id: String) {
        guard selected == id, case .fleet(let delta) = moved else { return }
        list(delta.view)
    }

    private func list(_ view: FleetView?) {
        guard let view else {
            Self.log.notice("fleet: no view yet")
            return
        }
        agents = view.sections.flatMap(\.rows).map { $0.card.name.isEmpty ? "agent" : $0.card.name }
        Self.log.notice("agents: \(self.agents.count, privacy: .public)")
    }

    /// Signs in at amux.sh in the system's browser and hands the account to
    /// the daemon, which picks the profile it lands on.
    func signIn() async {
        guard let runtime, !signingIn else { return }
        signingIn = true
        problem = nil
        defer { signingIn = false }
        do {
            let account = try await cloud.signIn(.adding, presenting: WebSignIn())
            guard let handover = await cloud.handOver(account.id) else {
                problem = "amux.sh signed in without a token to keep"
                return
            }
            await cloud.forgetSession(account.id)
            switch await runtime.bind(
                nil, cloud: handover.cloud, client: handover.client,
                refreshToken: handover.refreshToken)
            {
            case .success(let bound): selected = bound.id
            case .failure(let error): problem = error.description
            }
        } catch .cancelled {
            return
        } catch {
            problem = String(describing: error)
        }
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
