import AmuxCore
import AmuxMacScreens
import AmuxValues
import Foundation
import Observation

/// The shared new-agent store as the Mac's New Agent page reads it, with what
/// leaves the Mac — asking a host for its directories and offers, starting
/// the agent — sent through the stores.
/// Observable for the one thing it holds itself, the first message: the
/// store's own fields are observed through it.
@MainActor
@Observable
final class MacNewAgent: NewAgentSurface {
    @ObservationIgnored let stores: StoreBundle
    private var model: NewAgentStore { stores.newAgent }

    init(stores: StoreBundle) {
        self.stores = stores
    }

    var hosts: [HostView] { [stores.hosts.local].compactMap { $0 } + stores.hosts.hosts }
    var machine: [UInt8]? { model.machine?.bytes }
    func point(at host: [UInt8]) {
        if let id = HostId(bytes: host) { stores.point(at: id) }
    }
    func reached(_ online: Bool) { model.machineReached(online) }

    var listing: ListingState {
        switch model.listing {
        case .none: .none
        case .asking: .asking
        case .ready: .ready
        case .unavailable: .unavailable
        }
    }

    var repositories: [Directory] { model.repositories }
    var recent: [Directory] { model.recent }
    var history: [Directory] { model.history }
    func isPinned(_ path: String) -> Bool { model.isPinned(path) }
    func pin(_ directory: Directory, _ on: Bool) { stores.pin(directory, on) }
    var likely: [Directory] { model.likely }
    var directory: String { model.directory }
    func choose(directory: String) { model.choose(directory: directory) }

    var query: String {
        get { model.query }
        set {
            model.query = newValue
            stores.searchDirectories()
        }
    }

    var searching: Bool { model.searching }
    var found: [Directory] { model.found }

    var typed: String {
        get { model.typed }
        set { model.typed = newValue }
    }

    var agents: [HostProvider] { model.agents }
    var provider: String? { model.provider }
    func choose(provider: String) { model.choose(provider: provider) }

    var offer: OfferState? {
        switch model.offer {
        case .asking?: .asking
        case .offered?: .offered
        case .unavailable?: .unavailable
        case nil: nil
        }
    }

    var settings: SettingsView? { model.settings }
    func choose(_ pick: NewAgentPick) { model.choose(pick) }

    var newWorktree: Bool {
        get { model.newWorktree }
        set { model.newWorktree = newValue }
    }

    var name: String {
        get { model.name }
        set { model.choose(name: newValue) }
    }

    /// The first message, sent to the agent once its chat opens.
    var prompt = ""

    var starting: Bool { model.starting }
    var failure: String? { model.failure }
    var ready: Bool { model.ready }
    func start() { stores.startAgent() }
}
