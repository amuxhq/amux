import AmuxPush
import CryptoKit
import Foundation

/// One account as notifications see it.
public struct PushAccount: Sendable, Equatable {
    public var account: AccountId
    public var profile: String
    /// Whether it may have notifications: nil until that is known, which
    /// neither registers nor withdraws anything.
    public var wanted: Bool?

    public init(account: AccountId, profile: String, wanted: Bool?) {
        self.account = account
        self.profile = profile
        self.wanted = wanted
    }

    /// Notifications are for accounts signed in with Pro: the relay is what
    /// reaches a host from away, so anybody else would be told of something
    /// they can't act on. An account the app hasn't heard about yet is unknown
    /// rather than unwanted, because a phone that has only just started knows
    /// nothing of an account that isn't on screen, and taking its notifications
    /// away would lose them for every launch.
    public static func wanted(signedIn: Bool, pro: Bool?, entitlement: Entitlement) -> Bool? {
        guard signedIn else { return false }
        if let pro { return pro }
        switch entitlement {
        case .active: return true
        case .lapsed: return false
        case .none: return nil
        }
    }
}

/// Keeps each profile's notifications as they should be: a key in the
/// keychain, said to the runtime, and a device row at the account service, or
/// none of the three.
///
/// A key held in the keychain is what says a profile was registered, across
/// launches: the runtime forgets it at every stop and the service can't be
/// asked. So what is already held is said first, whatever else is still
/// unknown, because a host that reads this phone's entry without it forgets
/// the key and stops pushing to it.
///
/// Everything here is retried the next time the app comes forward. Taking
/// notifications away goes through whatever fails: with the key gone and
/// unsaid, a device row left behind receives nothing a host could seal.
@MainActor
public final class PushRegistrar {
    private let keys: any PushKeys
    private let cloud: any CloudService
    private let environment: PushDevice.Environment
    private let host: @MainActor (String) async -> HostId?
    private let say: @MainActor (String, [UInt8]?) async -> Bool

    private var accounts: [PushAccount] = []
    private var permitted: Bool?
    private var token: Data?
    /// Accounts on their way out, withdrawn while their bearer still works.
    private var leaving: Set<AccountId> = []
    /// What this launch has told the runtime and the service, by profile.
    private var said: [String: [UInt8]] = [:]
    private var registered: [String: Data] = [:]
    private var hosts: [String: HostId] = [:]

    private var settling: Task<Void, Never>?
    private var unsettled = false

    /// - Parameters:
    ///   - host: a profile's host id.
    ///   - say: tells the runtime a profile's public key, or that it wants
    ///     none, and answers whether it was told.
    public init(
        keys: any PushKeys, cloud: any CloudService, environment: PushDevice.Environment,
        host: @escaping @MainActor (String) async -> HostId?,
        say: @escaping @MainActor (String, [UInt8]?) async -> Bool
    ) {
        self.keys = keys
        self.cloud = cloud
        self.environment = environment
        self.host = host
        self.say = say
    }

    /// The accounts on this phone as they are now.
    public func accounts(_ accounts: [PushAccount]) {
        guard accounts != self.accounts else { return }
        self.accounts = accounts
        leaving = leaving.filter { id in
            accounts.contains { $0.account == id && $0.wanted != false }
        }
        settle()
    }

    /// Whether iOS lets this app notify: it can be taken away in Settings at
    /// any time, and there is no telling but to read it. It is read whenever
    /// the app comes forward, so an answer that changes nothing is still the
    /// moment to try again what failed, as a device row on a bad connection.
    public func permitted(_ permitted: Bool) {
        self.permitted = permitted
        settle()
    }

    /// The platform's token for this phone. It can change between launches,
    /// so every launch's token is registered again.
    public func token(_ token: Data) {
        guard token != self.token else { return }
        self.token = token
        settle()
    }

    /// Takes an account's notifications away and returns once that is done.
    /// Called before it signs out or leaves this phone, since asking the
    /// service takes the bearer it is about to lose.
    public func withdraw(_ account: AccountId) async {
        leaving.insert(account)
        settle()
        await settled()
    }

    /// Returns when nothing is left to do.
    public func settled() async {
        while let settling { await settling.value }
    }

    private func settle() {
        unsettled = true
        guard settling == nil else { return }
        settling = Task {
            while unsettled {
                unsettled = false
                for account in accounts { await settle(account) }
            }
            settling = nil
        }
    }

    private func settle(_ account: PushAccount) async {
        guard let host = await hostId(of: account.profile) else { return }
        // A keychain that can't be read now, as before the first unlock, says
        // nothing about what it holds, and a new key would strand every host
        // that holds the old one.
        let held: Curve25519.KeyAgreement.PrivateKey?
        do { held = try keys.key(for: host.description) } catch { return }
        switch wanted(account) {
        case nil:
            if let held { await tell(account.profile, held) }
        case true?:
            guard let key = held ?? (try? keys.makeKey(for: host.description)) else { return }
            await tell(account.profile, key)
            await register(account, host)
        case false?:
            if held != nil || said[account.profile] != nil || registered[account.profile] != nil {
                await withdraw(account, host)
            }
        }
    }

    private func wanted(_ account: PushAccount) -> Bool? {
        if leaving.contains(account.account) || account.wanted == false || permitted == false {
            return false
        }
        return permitted == nil ? nil : account.wanted
    }

    private func hostId(of profile: String) async -> HostId? {
        if let known = hosts[profile] { return known }
        let found = await host(profile)
        hosts[profile] = found
        return found
    }

    private func tell(_ profile: String, _ key: Curve25519.KeyAgreement.PrivateKey) async {
        let bytes = [UInt8](key.publicKey.rawRepresentation)
        guard said[profile] != bytes, await say(profile, bytes) else { return }
        said[profile] = bytes
    }

    private func register(_ account: PushAccount, _ host: HostId) async {
        guard let token, registered[account.profile] != token else { return }
        let device = PushDevice(token: token, environment: environment)
        guard (try? await cloud.registerDevice(account.account, host: host, device: device)) != nil
        else { return }
        registered[account.profile] = token
    }

    private func withdraw(_ account: PushAccount, _ host: HostId) async {
        _ = await say(account.profile, nil)
        said[account.profile] = nil
        registered[account.profile] = nil
        try? keys.removeKey(for: host.description)
        try? await cloud.removeDevice(account.account, host: host)
    }
}
