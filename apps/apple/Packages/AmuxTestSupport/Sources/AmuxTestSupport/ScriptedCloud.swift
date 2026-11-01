import AmuxCore
import Foundation

/// What the cloud will answer, declared by the test rather than discovered at
/// runtime. Every outcome — including the failures and the waiting — is stated
/// here, so a screen that only appears while a request is in flight is a
/// screen a test can actually reach.
public struct ScriptedCloudState: Codable, Sendable, Equatable {
    public var signIn: SignInOutcome
    public var entitlement: Entitlement
    /// The relay credential to hand back, or nothing to refuse.
    public var token: String?
    /// Where that credential says the relay is. App startup dials this address
    /// through the same coordinator used with the production account service.
    public var relayHost: String
    public var relayPort: Int
    public var deletion: DeletionOutcome
    /// What the account service does with a signed purchase handed to it.
    public var purchase: PurchaseRecording
    /// How long every answer takes. Zero is instant.
    public var latency: Duration

    /// What the relay will carry for this account, which is what an active
    /// subscription buys and nothing else does.
    public var tier: Tier {
        switch entitlement {
        case .active: .pro
        case .none, .lapsed: .free
        }
    }

    public init(
        signIn: SignInOutcome = .succeeds(Self.ada),
        entitlement: Entitlement = .active(grant: .purchased(.web), renews: nil),
        token: String? = "scripted-connect-token",
        relayHost: String = "relay.example",
        relayPort: Int = 443,
        deletion: DeletionOutcome = .deleted,
        purchase: PurchaseRecording = .accepted,
        latency: Duration = .zero
    ) {
        self.signIn = signIn
        self.entitlement = entitlement
        self.token = token
        self.relayHost = relayHost
        self.relayPort = relayPort
        self.deletion = deletion
        self.purchase = purchase
        self.latency = latency
    }

    public static let ada = SignedInAccount(
        id: AccountId("ada"), email: "ada@example.com", displayName: "Ada")

    /// Signed out, with nothing bought.
    public static var firstRun: ScriptedCloudState {
        ScriptedCloudState(signIn: .cancelled, entitlement: .none, token: nil)
    }

    /// Signed in, nothing bought.
    public static var unsubscribed: ScriptedCloudState {
        ScriptedCloudState(entitlement: .none)
    }

    /// Signed in, entitled, and nothing was ever bought: the account was given
    /// its access. It is a state the app got wrong for real people before the
    /// cloud could say so — the relay let them in while the phone offered them
    /// a paywall — so every screen that reads an entitlement can be driven
    /// from here.
    public static var granted: ScriptedCloudState {
        ScriptedCloudState(entitlement: .active(grant: .granted, renews: nil))
    }

    public enum SignInOutcome: Codable, Sendable, Equatable {
        case succeeds(SignedInAccount)
        case cancelled
        case refused(String)
        case offline
    }

    /// What becomes of a purchase the phone posts.
    ///
    /// Accepted by default, so every state written before there was such a
    /// thing still reaches the screen it was written for.
    public enum PurchaseRecording: Codable, Sendable, Equatable {
        case accepted
        /// The account service will not take this transaction, in its words.
        case refused(String)
        case offline
    }
}

/// One call a screen made, in the order it made it.
public enum CloudCall: Sendable, Equatable {
    /// A sign-in, with the account it asked amux.sh for.
    case signIn(SignInIntent)
    /// An account kept, and with it the session this phone holds for it.
    case handOver(AccountId)
    case forgetSession(AccountId)
    case entitlement(AccountId)
    case recordPurchase(AccountId)
    case requestDeletion(AccountId, confirmedEmail: String)
}

/// The cloud, scripted. It answers exactly what the state says and records
/// what it was asked, so a test can assert that a screen asked for a token
/// once rather than on every frame.
public final class ScriptedCloudService: CloudService, @unchecked Sendable {
    private let lock = NSLock()
    private var state: ScriptedCloudState
    private var recorded: [CloudCall] = []

    public init(state: ScriptedCloudState = ScriptedCloudState()) {
        self.state = state
    }

    /// The moment a scripted lapse is stamped with, fixed so a
    /// capture of one reads the same every run.
    public static let now = Date(timeIntervalSince1970: 1_764_580_800)

    public var calls: [CloudCall] { lock.withLock { recorded } }

    public var scripted: ScriptedCloudState {
        get { lock.withLock { state } }
        set { lock.withLock { state = newValue } }
    }

    public func reset() {
        lock.withLock { recorded = [] }
    }

    private func record(_ call: CloudCall) -> ScriptedCloudState {
        lock.withLock {
            recorded.append(call)
            return state
        }
    }

    private func wait(_ state: ScriptedCloudState) async {
        guard state.latency > .zero else { return }
        try? await Task.sleep(for: state.latency)
    }

    public func signIn(
        _ intent: SignInIntent, presenting: any WebAuthPresenter
    ) async throws(CloudError) -> SignedInAccount {
        let state = record(.signIn(intent))
        await wait(state)
        switch state.signIn {
        case .succeeds(let account):
            // The app's whole part in signing in: hand over a URL and be told
            // what came back. It never sees a password.
            _ = try? await presenting.present(
                URL(string: "https://amux.sh/sign-in")!, callbackScheme: "amux")
            return account
        case .cancelled: throw CloudError.cancelled
        case .refused(let reason): throw CloudError.refused(reason)
        case .offline: throw CloudError.network("offline")
        }
    }

    /// The account's refresh token for the runtime: the scripted relay's own
    /// login, which that relay's identity endpoint accepts as the CLI's.
    public func handOver(_ id: AccountId) async -> Handover? {
        let state = record(.handOver(id))
        guard let token = state.token,
              let cloud = URL(string: "http://\(state.relayHost):\(state.relayPort)")
        else { return nil }
        return Handover(cloud: cloud, client: "cli", refreshToken: token)
    }

    public func lend(from lender: @escaping @Sendable (AccountId) async -> String?) async {}

    public func forgetSession(_ id: AccountId) async throws {
        _ = record(.forgetSession(id))
    }

    public func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement {
        let state = record(.entitlement(id))
        await wait(state)
        return state.entitlement
    }

    public func recordPurchase(
        _ id: AccountId, signedTransaction: String
    ) async throws(CloudError) {
        let state = lock.withLock {
            recorded.append(.recordPurchase(id))
            return self.state
        }
        await wait(state)
        switch state.purchase {
        case .accepted:
            // What the account service does with a purchase it takes: the
            // account is entitled from then on, as bought in the App Store.
            lock.withLock {
                self.state.entitlement = .active(grant: .purchased(.appStore), renews: nil)
            }
        case .refused(let reason): throw CloudError.refused(reason)
        case .offline: throw CloudError.network("offline")
        }
    }

    public func requestDeletion(
        _ id: AccountId, confirmedEmail: String
    ) async throws(CloudError) -> DeletionOutcome {
        let state = record(.requestDeletion(id, confirmedEmail: confirmedEmail))
        await wait(state)
        return state.deletion
    }
}

/// A sign-in presenter that answers with the callback the cloud would have
/// sent, without opening a browser.
public struct ScriptedWebAuth: WebAuthPresenter {
    public var callback: URL
    public var outcome: Outcome

    public enum Outcome: Sendable, Equatable {
        case returns
        case cancelled
    }

    public init(callback: URL = URL(string: "amux://callback?code=scripted")!,
                outcome: Outcome = .returns) {
        self.callback = callback
        self.outcome = outcome
    }

    public func present(_ url: URL, callbackScheme: String) async throws(CloudError) -> URL {
        switch outcome {
        case .returns: return callback
        case .cancelled: throw CloudError.cancelled
        }
    }
}

/// What the scripted account service will answer, in the door's own words.
///
/// The double's own Swift shape reaches a wire as Swift's synthesised encoding
/// of nested enums — `{"succeeds":{"_0":…}}` — which nothing outside this
/// language would write and nobody could read in a transcript of a failing
/// run. These are the same outcomes, spelled, and every field has the answer
/// most states want so a driver says only what it is changing.
public struct CloudScript: Codable, Sendable, Equatable {
    /// `succeeds`, `cancelled`, `refused` or `offline`.
    public var signIn = "succeeds"
    /// Who signing in comes back as. The identifier is the app's own and is
    /// what a relay credential is later asked for by name.
    public var account = "ada"
    public var email = "ada@example.com"
    public var displayName: String?
    /// Why the cloud refused, where it refused.
    public var reason = "amux.sh could not sign this account in"
    /// `none`, `active` or `lapsed`.
    public var entitlement = "active"
    /// Where the access came from: `appStore`, `web`, or `granted` for an
    /// account that was given it and never bought anything.
    public var source = "web"
    /// The relay credential to hand back. Nothing refuses to issue one, which
    /// is what an account with no subscription is answered with.
    public var token: String? = "scripted-connect-token"
    public var relayHost = "relay.example"
    public var relayPort = 443
    /// `deleted` or `blockedByRenewal`.
    public var deletion = "deleted"
    /// What the account service does with a signed purchase: `accepted`,
    /// `refused` or `network`. Accepted unless a driver says otherwise, so a
    /// journey that is not about paying never has to mention it.
    public var recordPurchase = "accepted"
    /// Why the account service would not take the purchase, where it would
    /// not. Its own field so a driver changes one outcome at a time.
    public var purchaseReason = "amux.sh could not accept that purchase"
    /// Where a blocked deletion says the billing can be stopped.
    public var manageURL = "https://apps.apple.com/account/subscriptions"
    /// How long every answer takes, so a screen that is only on show while a
    /// request is in flight can be reached.
    public var latencyMillis = 0

    public init() {}

    /// Everything unsaid keeps the answer above, so a driver changing one
    /// outcome writes one field.
    public init(from decoder: any Decoder) throws {
        let fields = try decoder.container(keyedBy: CodingKeys.self)
        func said(_ key: CodingKeys, _ fallback: String) throws -> String {
            try fields.decodeIfPresent(String.self, forKey: key) ?? fallback
        }
        signIn = try said(.signIn, signIn)
        account = try said(.account, account)
        email = try said(.email, email)
        displayName = try fields.decodeIfPresent(String.self, forKey: .displayName)
        reason = try said(.reason, reason)
        entitlement = try said(.entitlement, entitlement)
        source = try said(.source, source)
        // Present and null is a cloud that will not issue a credential;
        // absent is a cloud nobody asked about.
        token = fields.contains(.token)
            ? try fields.decodeIfPresent(String.self, forKey: .token) : token
        relayHost = try said(.relayHost, relayHost)
        relayPort = try fields.decodeIfPresent(Int.self, forKey: .relayPort) ?? relayPort
        deletion = try said(.deletion, deletion)
        recordPurchase = try said(.recordPurchase, recordPurchase)
        purchaseReason = try said(.purchaseReason, purchaseReason)
        manageURL = try said(.manageURL, manageURL)
        latencyMillis = try fields.decodeIfPresent(Int.self, forKey: .latencyMillis)
            ?? latencyMillis
    }

    /// The state the double reads, which is the one thing this describes.
    public var state: ScriptedCloudState {
        ScriptedCloudState(
            signIn: outcome, entitlement: entitled, token: token,
            relayHost: relayHost, relayPort: relayPort, deletion: deleting,
            purchase: recording, latency: .milliseconds(latencyMillis))
    }

    private var who: SignedInAccount {
        SignedInAccount(id: AccountId(account), email: email, displayName: displayName)
    }

    private var outcome: ScriptedCloudState.SignInOutcome {
        switch signIn {
        case "cancelled": .cancelled
        case "refused": .refused(reason)
        case "offline": .offline
        default: .succeeds(who)
        }
    }

    /// How this account came by what it has. `granted` is access nobody paid
    /// for, which names no store; anything else was bought in one.
    private var bought: Grant {
        switch source {
        case "granted": .granted
        case "appStore": .purchased(.appStore)
        default: .purchased(.web)
        }
    }

    private var entitled: Entitlement {
        switch entitlement {
        case "none": .none
        // A subscription that ran out says when, because a screen that only
        // said "ended" would be telling somebody less than they knew.
        case "lapsed": .lapsed(grant: bought, endedAt: ScriptedCloudService.now.addingTimeInterval(-86_400))
        default: .active(grant: bought, renews: nil)
        }
    }

    private var recording: ScriptedCloudState.PurchaseRecording {
        switch recordPurchase {
        case "refused": .refused(purchaseReason)
        case "network": .offline
        default: .accepted
        }
    }

    private var deleting: DeletionOutcome {
        guard deletion == "blockedByRenewal", let url = URL(string: manageURL) else {
            return .deleted
        }
        // A deletion is only ever blocked by money still moving, so the
        // refusal names a store even when the scripted access was a gift.
        return .blockedByRenewal(source: bought.purchase ?? .web, manageURL: url)
    }
}
