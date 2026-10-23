import AmuxValues
import Foundation
import Observation

/// One account this phone has signed in to.
public struct AccountEntry: Sendable, Equatable, Identifiable, Codable {
    public var account: SignedInAccount
    /// Signing out keeps the account listed with Sign In beside it: the
    /// address is what a person recognises, and forgetting it would make
    /// signing back in look like adding a stranger.
    public var signedIn: Bool
    public var entitlement: Entitlement
    /// How many machines it had paired when last on screen.
    public var hosts: Int?
    /// How many of its agents needed the person when last on screen.
    public var attention: Int?
    /// The profile of this phone's installation that holds this account:
    /// its key, the machines it trusts, its store.
    public var profile: String

    public var id: AccountId { account.id }

    public init(
        account: SignedInAccount, signedIn: Bool = true, entitlement: Entitlement = .none,
        hosts: Int? = nil, attention: Int? = nil, profile: String = ""
    ) {
        self.account = account
        self.signedIn = signedIn
        self.entitlement = entitlement
        self.hosts = hosts
        self.attention = attention
        self.profile = profile
    }

    /// The second line an account row shows.
    public var line: String {
        if !signedIn { return "Signed out" }
        // An account with nothing paired yet is better known by its address.
        guard let hosts, hosts > 0 else { return account.email }
        return hosts == 1 ? "1 host" : "\(hosts) hosts"
    }

    public var name: String {
        account.displayName ?? account.email
    }
}

/// Whether the fleet on screen can reach machines through the relay.
public enum FleetGate: Sendable, Equatable {
    case ready
    case signedOut
    case unsubscribed
}

/// Every account on this phone and which one is on screen.
///
/// The accounts are the profiles of this phone's one installation, as its
/// registry lists them: one per account signed in to, which stays tied to
/// its account after a sign-out. A phone nobody is signed in on shows the
/// profile nobody has signed in on, which the first sign-in takes over — so
/// the machines paired before there was an account are that account's.
/// What is remembered here is only which account is on screen and, per
/// account, what it last listed and what the account service last said.
@MainActor
@Observable
public final class AccountRegistry {
    public private(set) var accounts: [AccountEntry] = []
    public private(set) var selected: AccountId?
    /// The stores of the account on screen.
    public private(set) var stores: StoreBundle?
    /// The profile nobody has signed in on, if there is one.
    public private(set) var unbound: String?
    /// Answers for an account that is not on screen, dropped.
    public private(set) var dropped = 0
    /// Told when what is on screen changes, before `changed`.
    @ObservationIgnored public var switching: (@MainActor (AccountId?) -> Void)?
    @ObservationIgnored public var changed: (@MainActor () -> Void)?
    @ObservationIgnored private let file: URL?
    public private(set) var persistenceFailed = false
    /// What each account last listed and what the account service last
    /// said, by account.
    @ObservationIgnored private var seen: [AccountId: Seen] = [:]

    private struct Seen: Codable, Equatable {
        var hosts: Int?
        var attention: Int?
        var entitlement: Entitlement = .none
    }

    private struct Remembered: Codable {
        var selected: AccountId?
        var seen: [Keyed]
    }

    private struct Keyed: Codable {
        var account: AccountId
        var seen: Seen
    }

    /// Makes the stores for an account going on screen.
    @ObservationIgnored private let makeStores: @MainActor (AccountId) -> StoreBundle

    public init(
        file: URL? = nil,
        makeStores: @escaping @MainActor (AccountId) -> StoreBundle = { StoreBundle(account: $0) }
    ) {
        self.file = file
        self.makeStores = makeStores
        guard let file, let data = try? Data(contentsOf: file),
              let saved = try? AmuxJSON.decoder.decode(Remembered.self, from: data) else {
            persist()
            return
        }
        selected = saved.selected
        seen = Dictionary(saved.seen.map { ($0.account, $0.seen) }) { first, _ in first }
        stores = selected.map(bundle)
    }

    private func persist() {
        guard let file else { return }
        do {
            try FileManager.default.createDirectory(
                at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            let keyed = seen.map { Keyed(account: $0.key, seen: $0.value) }
                .sorted { $0.account.value < $1.account.value }
            try AmuxJSON.encoder.encode(Remembered(selected: selected, seen: keyed))
                .write(to: file, options: .atomic)
            persistenceFailed = false
        } catch {
            persistenceFailed = true
        }
    }

    public var selectedAccount: AccountEntry? {
        accounts.first { $0.id == selected }
    }

    /// The profile on screen: the selected account's, or the one nobody has
    /// signed in on.
    public var profile: String? {
        guard let selected else { return unbound }
        return accounts.first { $0.id == selected }?.profile
    }

    /// What the relay will carry for the account on screen: the link's own
    /// word first, then what the account service last said.
    public var gate: FleetGate {
        guard let entry = selectedAccount, entry.signedIn else { return .signedOut }
        switch stores?.hosts.account?.pro {
        case true?: return .ready
        case false?: return .unsubscribed
        case nil:
            if case .active = entry.entitlement { return .ready }
            return .unsubscribed
        }
    }

    /// Takes the installation's profile list as it is now. An account whose
    /// profile is gone leaves the screen for the first signed-in account,
    /// then any account, then the profile nobody has signed in on.
    public func show(_ profiles: [ProfileView]) {
        let before = profile
        accounts = profiles.filter { !$0.subject.isEmpty }.map { view in
            let id = AccountId(view.subject)
            let seen = seen[id] ?? Seen()
            return AccountEntry(
                account: SignedInAccount(
                    id: id, email: view.account.email,
                    displayName: view.account.name.isEmpty ? nil : view.account.name,
                    picture: URL(string: view.account.picture)),
                signedIn: view.account.binding != .signedOut,
                entitlement: seen.entitlement, hosts: seen.hosts, attention: seen.attention,
                profile: view.id)
        }
        unbound = profiles.first { $0.subject.isEmpty }?.id
        // Nothing was on screen yet, or what was is gone.
        if selected.map({ id in !accounts.contains { $0.id == id } }) ?? (unbound == nil) {
            place(next())
        } else if profile != before {
            switching?(selected)
            changed?()
        } else {
            changed?()
        }
    }

    /// Where the screen goes when the account on it leaves.
    private func next(leaving: AccountId? = nil) -> AccountId? {
        let left = accounts.filter { $0.id != leaving }
        return left.first(where: \.signedIn)?.id ?? left.first?.id
    }

    public func select(_ id: AccountId) {
        guard accounts.contains(where: { $0.id == id }), selected != id else { return }
        place(id)
    }

    /// Takes an account off the screen before its profile is deleted.
    public func leave(_ id: AccountId) {
        guard selected == id else { return }
        place(next(leaving: id))
    }

    private func place(_ id: AccountId?) {
        if selected != id {
            selected = id
            stores = id.map(bundle)
            persist()
        }
        switching?(selected)
        changed?()
    }

    public func entitlement(_ entitlement: Entitlement, for id: AccountId) {
        guard seen[id, default: Seen()].entitlement != entitlement else { return }
        seen[id, default: Seen()].entitlement = entitlement
        if let index = accounts.firstIndex(where: { $0.id == id }) {
            accounts[index].entitlement = entitlement
        }
        persist()
        changed?()
    }

    /// The stores for an account going on screen, which report what it
    /// lists so its row can say so once it is off screen.
    private func bundle(_ id: AccountId) -> StoreBundle {
        let stores = makeStores(id)
        stores.saw = { [weak self] hosts, attention in
            self?.saw(hosts: hosts, attention: attention, for: id)
        }
        return stores
    }

    /// What the account on screen lists, remembered for when it is not.
    public func saw(hosts: Int, attention: Int, for id: AccountId) {
        guard seen[id]?.hosts != hosts || seen[id]?.attention != attention else { return }
        seen[id, default: Seen()].hosts = hosts
        seen[id, default: Seen()].attention = attention
        if let index = accounts.firstIndex(where: { $0.id == id }) {
            accounts[index].hosts = hosts
            accounts[index].attention = attention
        }
        persist()
    }

    /// A late answer is kept only for the account still on screen.
    public func accept<Value>(_ value: Value, for account: AccountId) -> Value? {
        guard account == selected else {
            dropped += 1
            return nil
        }
        return value
    }
}
