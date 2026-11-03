import AmuxCore
import AmuxValues
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

    @ObservationIgnored private var runtime: Runtime?
    @ObservationIgnored private var profile: Profile?
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
        phase = .installing
        let bundled = Bundle.main.bundleURL.appending(path: "Contents/Helpers/amux")
        do {
            let installed = try await Runtime.install(InstallOptions(
                bundled: FileManager.default.isExecutableFile(atPath: bundled.path)
                    ? bundled.path : nil,
                config: Self.config,
                loginItem: true))
            Self.log.notice("amux: \(String(describing: installed), privacy: .public)")
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

    /// Signs the profile on screen out; it stays tied to its account.
    func signOut() async {
        guard let runtime, let selected else { return }
        if case .failure(let error) = await runtime.signOut(selected) {
            problem = error.description
        }
    }
}
