import AmuxCore
import AmuxMacScreens
import AmuxValues
import Foundation

/// The shared new-agent store as the Mac's sheet reads it, with what
/// leaves the Mac — asking a host for its directories and offers, starting
/// the agent — sent through the stores.
@MainActor
final class MacNewAgent: NewAgentSurface {
    let stores: StoreBundle
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

    var provider: Provider { model.provider.wire }

    func choose(provider: Provider) {
        if let chosen = NewAgentStore.Provider.allCases.first(where: { $0.wire == provider }) {
            model.choose(provider: chosen)
        }
    }

    var offer: OfferState? {
        switch model.offers[model.provider] {
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

    var starting: Bool { model.starting }
    var failure: String? { model.failure }
    var ready: Bool { model.ready }
    func start() { stores.startAgent() }
}
