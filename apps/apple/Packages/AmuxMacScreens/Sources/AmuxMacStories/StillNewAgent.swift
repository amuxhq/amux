import AmuxMacScreens
import AmuxValues
import Foundation
import Observation

/// Starting an agent that holds still: what a host listed and offered, which
/// the New Agent page can be clicked through, with nothing ever started.
@MainActor
@Observable
final class StillNewAgent: NewAgentSurface {
    let hosts: [HostView]
    private(set) var machine: [UInt8]?
    let listing: ListingState
    let repositories: [Directory]
    let recent: [Directory]
    var history: [Directory] = []
    private(set) var directory: String
    var query = ""
    var typed = ""
    private(set) var chosen: String?
    private var choices: [String: String] = [:]
    var newWorktree = false
    var name = ""
    var prompt = ""
    let failure: String? = nil

    init(
        hosts: [HostView], machine: [UInt8]?, repositories: [Directory], recent: [Directory], directory: String
    ) {
        self.hosts = hosts
        self.machine = machine
        self.repositories = repositories
        self.recent = recent
        self.directory = directory
        listing = .ready
    }

    func point(at host: [UInt8]) { machine = host }
    func reached(_ online: Bool) {}

    var likely: [Directory] { recent + repositories }
    func choose(directory: String) { self.directory = directory }
    /// The same, with Claude Code and Codex's own recent folders.
    func with(history: [Directory]) -> Self {
        self.history = history
        return self
    }

    func isPinned(_ path: String) -> Bool { repositories.contains { $0.path == path } }
    func pin(_ directory: Directory, _ on: Bool) {}
    var searching: Bool { !query.trimmingCharacters(in: .whitespaces).isEmpty }
    var found: [Directory] {
        let wanted = query.trimmingCharacters(in: .whitespaces).lowercased()
        return (repositories + recent + history).filter { $0.path.lowercased().contains(wanted) }
    }

    /// The chosen host's agents, as its entry lists them.
    var agents: [HostProvider] { hosts.first { $0.hostId == machine }?.providers ?? [] }
    var provider: String? { chosen ?? agents.first?.provider }

    func choose(provider: String) {
        chosen = provider
        choices = [:]
    }

    var offer: OfferState? { .offered }

    var settings: SettingsView? {
        // Claude's settings, and for any other agent the models it listed in
        // its last conversation there, as an ACP agent offers them: no
        // efforts, no permissions, and modes with none of them normal.
        if provider != agents.first?.provider { return otherAgent }
        let model = choices["model"] ?? "opus"
        let effort = choices["effort"] ?? "high"
        let permission = choices["permission"] ?? "default"
        return SettingsView(
            models: [
                ModelChoice(
                    value: "opus", displayName: "Opus 5.5", description: "", efforts: ["low", "medium", "high"],
                    current: model == "opus", unlisted: false, defaultEffort: "high"),
                ModelChoice(
                    value: "sonnet", displayName: "Sonnet 5.5", description: "", efforts: ["low", "medium", "high"],
                    current: model == "sonnet", unlisted: false, defaultEffort: "medium"),
            ],
            efforts: ["low", "medium", "high"].map {
                EffortChoice(value: $0, current: $0 == effort, default: $0 == "high", unlisted: false)
            },
            permissions: [
                PermissionChoice(
                    value: "default", displayName: "Ask First", custom: false, current: permission == "default",
                    unlisted: false, normal: true, neverAsks: false, settable: true),
                PermissionChoice(
                    value: "acceptEdits", displayName: "Accept Edits", custom: false,
                    current: permission == "acceptEdits", unlisted: false, normal: false, neverAsks: false,
                    settable: true),
                PermissionChoice(
                    value: "bypassPermissions", displayName: "Never Ask", custom: false,
                    current: permission == "bypassPermissions", unlisted: false, normal: false, neverAsks: true,
                    settable: true),
            ],
            modes: [],
            changeable: Changeability(model: .pick, effort: .pick, permission: .pick, mode: .notOffered),
            commands: [])
    }

    private var otherAgent: SettingsView {
        let model = choices["model"] ?? "anthropic/claude-sonnet-5"
        let mode = choices["mode"]
        return SettingsView(
            models: [
                ModelChoice(
                    value: "anthropic/claude-sonnet-5", displayName: "Claude Sonnet 5", description: "", efforts: [],
                    current: model == "anthropic/claude-sonnet-5", unlisted: false, defaultEffort: nil),
                ModelChoice(
                    value: "openai/gpt-6", displayName: "GPT-6", description: "", efforts: [],
                    current: model == "openai/gpt-6", unlisted: false, defaultEffort: nil),
            ],
            efforts: [],
            permissions: [],
            modes: [
                ModeChoice(value: "build", displayName: "Build", current: mode == "build", unlisted: false, normal: false, settable: true),
                ModeChoice(value: "plan", displayName: "Plan", current: mode == "plan", unlisted: false, normal: false, settable: true),
            ],
            changeable: Changeability(model: .pick, effort: .notOffered, permission: .notOffered, mode: .pick),
            commands: [])
    }

    func choose(_ pick: NewAgentPick) {
        switch pick {
        case .model(let value): choices["model"] = value
        case .effort(let value): choices["effort"] = value
        case .permission(let value): choices["permission"] = value
        case .mode(let value): choices["mode"] = value
        }
    }

    let starting = false
    var ready: Bool { machine != nil && !directory.isEmpty }
    func start() {}
}
