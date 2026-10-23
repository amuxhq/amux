import Foundation

/// Everything this app asks of the cloud.
///
/// One protocol, so a screen never sees HTTP and a test never needs a server:
/// the production adapter and the scripted double are the same shape, and a
/// screen cannot tell which one it is holding.
public protocol CloudService: Sendable {
    /// Signs somebody in on the web, asking amux.sh for the account the
    /// intent names. What comes back is whoever actually signed in, which is
    /// not always who was asked for.
    func signIn(
        _ intent: SignInIntent, presenting: any WebAuthPresenter
    ) async throws(CloudError) -> SignedInAccount
    /// Hands over the refresh token a sign-in obtained, once, for the
    /// runtime to bind the account's profile with. From then on the profile
    /// alone spends it: a refresh token may be good for one use, so two
    /// holders would sign each other out.
    func handOver(_ id: AccountId) async -> Handover?
    /// Lets go of what this phone holds for an account. The account on
    /// amux.sh is untouched.
    func forgetSession(_ id: AccountId) async throws
    /// Where a fresh bearer comes from once the one a sign-in obtained has
    /// expired: the runtime whose profile holds the account's refresh token.
    func lend(from lender: @escaping @Sendable (AccountId) async -> String?) async
    func account(_ id: AccountId) async throws(CloudError) -> AccountFacts
    func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement
    /// Hands over a purchase the App Store signed, so the subscription it paid
    /// for becomes this account's.
    ///
    /// The app carries the signed transaction and nothing else: what the cloud
    /// then does with it — which billing system it is reconciled against, how
    /// a renewal is watched — is the cloud's business, and an app that knew
    /// would be a second place for that to change.
    func recordPurchase(_ id: AccountId, signedTransaction: String) async throws(CloudError)
    func requestDeletion(_ id: AccountId, confirmedEmail: String) async throws(CloudError) -> DeletionOutcome
    func uploadReport(_ id: AccountId, bundle: ReportBundle) async throws(CloudError) -> ReportReceipt
}

/// What a sign-in leaves for the runtime: the service that issued the
/// refresh token, the client it was issued to, and the token itself.
public struct Handover: Sendable, Equatable {
    public var cloud: URL
    public var client: String
    public var refreshToken: String

    public init(cloud: URL, client: String, refreshToken: String) {
        self.cloud = cloud
        self.client = client
        self.refreshToken = refreshToken
    }
}

/// Which account a sign-in is for.
///
/// It decides what amux.sh shows before anybody types: a person adding an
/// account is offered every account that browser has used and another one,
/// and a person signing back into an account this phone lists is sent straight
/// to that account.
public enum SignInIntent: Sendable, Equatable {
    /// Another account for this phone, or the first one.
    case adding
    /// An account this phone already lists and is signed out of.
    case returning(SignedInAccount)
}

/// Sign-in happens on the web, in a browser the app does not own and cannot
/// read. This is the app's whole part in it: hand over a URL and be told what
/// came back.
public protocol WebAuthPresenter: Sendable {
    func present(_ url: URL, callbackScheme: String) async throws(CloudError) -> URL
}

public enum CloudError: Error, Sendable, Equatable {
    case cancelled
    case unauthenticated
    case network(String)
    case refused(String)
    case timeout
}

public struct AccountId: Hashable, Sendable, Codable, CustomStringConvertible {
    public let value: String
    public init(_ value: String) { self.value = value }
    public init(from decoder: any Decoder) throws {
        value = try decoder.singleValueContainer().decode(String.self)
    }
    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(value)
    }
    public var description: String { value }
}

public struct SignedInAccount: Sendable, Equatable, Codable {
    public var id: AccountId
    public var email: String
    public var displayName: String?
    /// The account's profile picture, where it has one.
    public var picture: URL?

    public init(id: AccountId, email: String, displayName: String? = nil, picture: URL? = nil) {
        self.id = id
        self.email = email
        self.displayName = displayName
        self.picture = picture
    }
}

public struct AccountFacts: Sendable, Equatable, Codable {
    public var id: AccountId
    public var email: String
    public var displayName: String?
    public var entitlement: Entitlement

    public init(id: AccountId, email: String, displayName: String? = nil, entitlement: Entitlement) {
        self.id = id
        self.email = email
        self.displayName = displayName
        self.entitlement = entitlement
    }
}

/// What this account is allowed to do, and where that came from. A lapsed
/// entitlement says when it ended rather than pretending it never existed.
///
/// The cloud answers one question — may this account act — and this is that
/// answer. It is never inferred from whether a billing record exists: an
/// account can be entitled without anybody ever having paid, and reading the
/// absence of a purchase as the absence of access is the mistake this shape
/// exists to prevent.
public enum Entitlement: Sendable, Equatable, Codable {
    case none
    case active(grant: Grant, renews: Date?)
    case lapsed(grant: Grant, endedAt: Date)
}

/// Why an account has what it has.
///
/// Two cases, not an optional purchase, because an entitlement that was given
/// rather than sold is a state every screen has to say something about — and a
/// screen that treated it as a missing purchase would offer to manage a
/// subscription that does not exist.
public enum Grant: Sendable, Equatable, Codable {
    /// Bought, in the place it was bought.
    case purchased(EntitlementSource)
    /// Given: complimentary, an employee, a beta tester. There is no store
    /// behind it, nothing is being billed, and there is nothing to cancel.
    case granted

    /// Where it was bought, for the screens that only have something to say
    /// about a purchase. Nothing when it was not one.
    public var purchase: EntitlementSource? {
        switch self {
        case .purchased(let source): source
        case .granted: nil
        }
    }
}

public enum EntitlementSource: String, Sendable, Equatable, Codable {
    case appStore
    case web
}

/// A relay credential issued by the configured cloud, and the relay it is good at. The bridge asks for one
/// when it needs it and the app answers; nothing caches it beyond its expiry.
///

/// Deletion can be refused while money is still moving, and the refusal has to
/// say where to go and stop it.
public enum DeletionOutcome: Sendable, Equatable, Codable {
    case deleted
    case blockedByRenewal(source: EntitlementSource, manageURL: URL)
}

/// A report as assembled for upload. Every part declares itself present or
/// states why it is missing, so a report with a hole in it is still readable
/// as a report rather than as a bug in the reporter.
public struct ReportBundle: Sendable, Equatable, Codable {
    /// Every part, `report.json` first. What somebody wrote about the report
    /// is inside that file rather than beside it: the header is what declares
    /// the note, the rectangles and which other parts are here at all, and a
    /// note carried separately would be a second place for it to live.
    public var parts: [ReportPart]

    public init(parts: [ReportPart]) {
        self.parts = parts
    }

    /// One part by the name it is filed under.
    public func part(_ name: String) -> ReportPart? {
        parts.first { $0.name == name }
    }
}

public struct ReportPart: Sendable, Equatable, Codable {
    public var name: String
    public var data: Data?
    public var absenceReason: String?

    public init(name: String, data: Data? = nil, absenceReason: String? = nil) {
        self.name = name
        self.data = data
        self.absenceReason = absenceReason
    }

    public var present: Bool { data != nil }
}

public struct ReportReceipt: Sendable, Equatable, Codable {
    public var id: String
    public var receivedAt: Date

    public init(id: String, receivedAt: Date) {
        self.id = id
        self.receivedAt = receivedAt
    }
}
