import Foundation

/// Keeps what the account service says the account on screen buys current.
///
/// A subscription changes away from the phone: bought on amux.sh, cancelled
/// in the App Store's settings, lapsed at the end of a period. The phone
/// learns of it only by asking, so it asks whenever somebody comes back to
/// the app and whenever You comes on screen. A look within `spacing` of the
/// last one asks nothing: You and the home read the same answer, and a
/// person flicking between tabs is not a reason to call amux.sh each time.
@MainActor
public final class EntitlementWatch {
    /// The least time between two reads that nobody forced.
    public static let spacing: TimeInterval = 30

    private let registry: AccountRegistry
    private let read: @MainActor (AccountId) async -> Entitlement?
    private let relink: @MainActor (AccountId) async -> Bool
    private let now: @MainActor () -> Date
    private var lastRead: (account: AccountId, at: Date)?

    /// - Parameters:
    ///   - read: what the account service says an account buys, or nothing
    ///     where it did not answer.
    ///   - relink: has the account's relay link ask what it buys again, so it
    ///     does not carry the old answer until its credential next renews.
    public init(
        registry: AccountRegistry,
        read: @escaping @MainActor (AccountId) async -> Entitlement?,
        relink: @escaping @MainActor (AccountId) async -> Bool,
        now: @escaping @MainActor () -> Date = { Date() }
    ) {
        self.registry = registry
        self.read = read
        self.relink = relink
        self.now = now
    }

    /// Reads what the account on screen buys and keeps it, unless it was read
    /// less than `spacing` ago. `force` reads regardless and always has the
    /// relay link ask too, which is what a purchase wants: it has just
    /// changed the answer, whatever was last read.
    ///
    /// Returns whether the account service answered.
    @discardableResult
    public func look(force: Bool = false) async -> Bool {
        guard let id = registry.selected else { return false }
        let at = now()
        if !force, let last = lastRead, last.account == id,
           at.timeIntervalSince(last.at) < Self.spacing {
            return true
        }
        let before = registry.selectedAccount?.entitlement
        guard let answer = await read(id), let kept = registry.accept(answer, for: id) else {
            return false
        }
        lastRead = (id, at)
        registry.entitlement(kept, for: id)
        if force || kept != before || relayDisagrees(with: kept) {
            if await relink(id) { await registry.stores?.refreshAccount() }
        }
        return true
    }

    /// Whether the relay link's own word on what the account buys is not
    /// what the account service just said.
    private func relayDisagrees(with entitlement: Entitlement) -> Bool {
        guard let pro = registry.stores?.hosts.account?.pro else { return false }
        if case .active = entitlement { return !pro }
        return pro
    }
}
