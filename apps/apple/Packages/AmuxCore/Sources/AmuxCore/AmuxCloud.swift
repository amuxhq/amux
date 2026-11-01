import CryptoKit
import Foundation

/// Where the cloud is, and who this app says it is when it knocks.
///
/// The client identifier and the callback are not this app's to choose: the
/// account service already has a registered client for the phone, with that
/// one redirect and no secret, and an app that invented either would be
/// refused at the authorize endpoint rather than at a screen anybody can read.
public struct CloudEndpoint: Sendable, Equatable {
    public var base: URL
    public var clientID: String
    public var callback: URL
    public var scopes: [String]

    public init(base: URL, clientID: String, callback: URL, scopes: [String]) {
        self.base = base
        self.clientID = clientID
        self.callback = callback
        self.scopes = scopes
    }

    /// The real account service.
    ///
    /// `offline_access` is asked for because an access token lasts an hour and
    /// a phone is opened for ten seconds at a time: without a refresh token
    /// this app would send somebody back to a browser every hour, and the
    /// design's whole claim about sign-in is that it happens once.
    public static let production = CloudEndpoint(
        base: URL(string: "https://amux.sh")!,
        clientID: "mobile",
        callback: URL(string: "amux://callback")!,
        scopes: ["openid", "profile", "email", "offline_access", "api"])

    /// The host this app tells somebody it is sending them to. It is read off
    /// the URL rather than written twice, so the screen cannot name one place
    /// and the hand-off open another.
    public var host: String { base.host() ?? base.absoluteString }

    /// Where somebody is sent to reach a person. Public because the screen
    /// that offers it is not this adapter's, and derived from the same base as
    /// everything else so a build pointed at another service cannot offer to
    /// contact the production one's support.
    public var support: URL { base.appending(path: "support") }

    var authorize: URL { base.appending(path: "connect/authorize") }
    var token: URL { base.appending(path: "connect/token") }
    var userinfo: URL { base.appending(path: "connect/userinfo") }
    var graphQL: URL { base.appending(path: "api/graphql") }
    var purchases: URL { base.appending(path: "api/purchases") }
    var account: URL { base.appending(path: "api/account") }
    var stripePortal: URL { base.appending(path: "api/billing/stripe/portal") }
}

/// How a request actually leaves the phone.
///
/// One seam, so the adapter's own suite can drive every branch — a refused
/// sign-in, an expired token, a deletion the billing system blocks — without a
/// server and without a network. The adapter above it is the same code either
/// way; what changes is only what answers.
public protocol CloudTransport: Sendable {
    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse)
}

extension URLSession: CloudTransport {
    public func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let (data, response) = try await data(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw URLError(.badServerResponse)
        }
        return (data, http)
    }
}

/// The amux cloud as it really is.
///
/// Sign-in is an authorization code with PKCE, and the code is the only thing
/// that ever comes back to the app: the password is typed on amux.sh, in a
/// browser this app cannot read, and the verifier that redeems the code never
/// leaves the phone. That is why the sign-in screen has no field of its own —
/// an app with a password box would be asking to be trusted with the one
/// secret it is designed never to hold.
public actor AmuxCloudService: CloudService {
    private let endpoint: CloudEndpoint
    private let transport: any CloudTransport
    /// Where a fresh bearer comes from once a sign-in's own has expired.
    private var lender: (@Sendable (AccountId) async -> String?)?
    private let now: @Sendable () -> Date
    /// What this phone holds for each account it has signed in. The access
    /// token is short-lived and the refresh token is what survives; both stay
    /// here rather than on any screen.
    private var sessions: [AccountId: Session] = [:]

    private struct Session {
        var access: String
        var refresh: String?
        var expiresAt: Date
    }

    public init(
        endpoint: CloudEndpoint = .production,
        transport: any CloudTransport = URLSession.shared,
        now: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.endpoint = endpoint
        self.transport = transport
        self.now = now
    }

    public func handOver(_ account: AccountId) -> Handover? {
        guard let refresh = sessions[account]?.refresh else { return nil }
        sessions[account]?.refresh = nil
        return Handover(cloud: endpoint.base, client: endpoint.clientID, refreshToken: refresh)
    }

    public func forgetSession(_ account: AccountId) {
        sessions.removeValue(forKey: account)
    }

    public func lend(from lender: @escaping @Sendable (AccountId) async -> String?) {
        self.lender = lender
    }

    // MARK: - Signing in

    public func signIn(
        _ intent: SignInIntent, presenting: any WebAuthPresenter
    ) async throws(CloudError) -> SignedInAccount {
        let verifier = Self.randomToken()
        let state = Self.randomToken()
        let returned = try await presenting.present(
            authorizeURL(verifier: verifier, state: state, intent: intent),
            callbackScheme: endpoint.callback.scheme ?? "amux")
        let code = try Self.code(from: returned, expecting: state)
        let issued = try await exchange([
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": endpoint.callback.absoluteString,
            "client_id": endpoint.clientID,
            "code_verifier": verifier,
        ])
        let who = try await who(with: issued.access_token)
        let id = AccountId(who.sub)
        // In memory and nowhere else. Whoever signed in is not always the
        // account that was asked for, and the token is written down only when
        // somebody keeps the account it belongs to.
        sessions[id] = Session(
            access: issued.access_token, refresh: issued.refresh_token,
            expiresAt: now().addingTimeInterval(TimeInterval(issued.expires_in ?? 3600)))
        return SignedInAccount(
            id: id, email: who.email ?? "", displayName: who.name,
            picture: who.picture.flatMap(URL.init(string:)))
    }

    /// The URL the browser opens.
    ///
    /// The challenge is the SHA-256 of a secret this phone just made and keeps;
    /// the state is a second secret that is only ever compared with what comes
    /// back. Together they are what stops another app on this phone claiming
    /// the callback and redeeming somebody else's code.
    ///
    /// The intent picks what amux.sh shows. Both ask for its account chooser,
    /// which lists the accounts this browser has used and offers another:
    /// without it amux.sh carries on as whoever the browser is signed in as,
    /// whichever account was meant. Signing back into one account also names
    /// it by its address, so the form amux.sh shows for it has the address
    /// filled in. A server that knows neither parameter ignores both.
    func authorizeURL(verifier: String, state: String, intent: SignInIntent) -> URL {
        var components = URLComponents(url: endpoint.authorize, resolvingAgainstBaseURL: false)!
        components.queryItems = [
            URLQueryItem(name: "client_id", value: endpoint.clientID),
            URLQueryItem(name: "response_type", value: "code"),
            URLQueryItem(name: "redirect_uri", value: endpoint.callback.absoluteString),
            URLQueryItem(name: "scope", value: endpoint.scopes.joined(separator: " ")),
            URLQueryItem(name: "code_challenge", value: Self.challenge(for: verifier)),
            URLQueryItem(name: "code_challenge_method", value: "S256"),
            URLQueryItem(name: "state", value: state),
        ]
        components.queryItems?.append(URLQueryItem(name: "prompt", value: "select_account"))
        if case .returning(let account) = intent {
            components.queryItems?.append(URLQueryItem(name: "login_hint", value: account.email))
        }
        // A plus is legal in an address and left alone by URLComponents, but a
        // server reading the query as a form turns it into a space, which
        // would ask for somebody else's address.
        components.percentEncodedQuery = components.percentEncodedQuery?
            .replacingOccurrences(of: "+", with: "%2B")
        return components.url!
    }

    /// Reads the callback. A callback that names an error, carries no code, or
    /// answers a request this phone did not make is refused here rather than
    /// redeemed.
    static func code(from callback: URL, expecting state: String) throws(CloudError) -> String {
        let items = URLComponents(url: callback, resolvingAgainstBaseURL: false)?.queryItems ?? []
        func value(_ name: String) -> String? {
            items.first { $0.name == name }?.value
        }
        if let error = value("error") {
            throw CloudError.refused(value("error_description") ?? error)
        }
        guard value("state") == state else {
            throw CloudError.refused("that sign-in answered a different request")
        }
        guard let code = value("code"), !code.isEmpty else {
            throw CloudError.refused("amux.sh sent no sign-in back")
        }
        return code
    }

    // MARK: - What the cloud knows

    /// What this account is allowed to do, and where that came from.
    ///
    /// One read, and `pro` is the whole gate. Not the tier claim in the access
    /// token, which is a copy of this answer that goes stale between token
    /// issues, and above all not the presence of a billing record: an account
    /// can be entitled without ever having bought anything, and an app that
    /// looked for a purchase would tell such a person to subscribe while the
    /// relay was already letting them in. The grant beside it is only ever the
    /// explanation of the access, never the test for it.
    public func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement {
        let query = "{ me { access { pro until grant { __typename "
            + "... on Purchased { provider willRenew entitledUntil } } } } }"
        var request = URLRequest(url: endpoint.graphQL)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try? JSONEncoder().encode(["query": query])
        let answer: GraphQLAnswer<Me> = try await ask(request, as: GraphQLAnswer<Me>.self, for: id)
        guard let access = answer.data?.me?.access else { return .none }
        return access.entitlement
    }

    /// Hands a signed App Store transaction to the account service.
    ///
    /// Nothing is read back but the fact that it was taken: what this account
    /// may now do is the entitlement read's answer, which is the same read a
    /// subscription bought on the web arrives through. `202` is as good as
    /// `200` — the cloud has the transaction and will reconcile it — and the
    /// caller finds out through that read either way.
    public func recordPurchase(
        _ id: AccountId, signedTransaction: String
    ) async throws(CloudError) {
        var request = URLRequest(url: endpoint.purchases)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try? JSONEncoder().encode(
            Purchase(signed_transaction: signedTransaction))
        let (data, response) = try await send(request, for: id)
        guard (200..<300).contains(response.statusCode) else {
            throw Self.refusal(data, response)
        }
    }

    // MARK: - Leaving

    /// Deletes the account, once the person has typed its address.
    ///
    /// The address is checked here, against what the cloud says this account
    /// is, because the account service takes no address: deletion is
    /// authenticated by the token alone. Typing it is the person proving they
    /// know which account they are about to lose, and a check made anywhere
    /// but against the cloud's own answer would be checking against whatever
    /// this phone happened to remember.
    public func requestDeletion(
        _ id: AccountId, confirmedEmail: String
    ) async throws(CloudError) -> DeletionOutcome {
        let who = try await who(with: try await bearer(for: id))
        let typed = confirmedEmail.trimmingCharacters(in: .whitespacesAndNewlines)
        guard typed.compare(who.email ?? "", options: .caseInsensitive) == .orderedSame else {
            throw CloudError.refused("that is not this account’s address")
        }
        var request = URLRequest(url: endpoint.account)
        request.httpMethod = "DELETE"
        let (data, response) = try await send(request, for: id)
        switch response.statusCode {
        case 200..<300:
            sessions.removeValue(forKey: id)
            return .deleted
        case 401: throw CloudError.unauthenticated
        // Money is still moving. The account service names the provider that
        // is billing, and the person is sent to the one place that can stop
        // it — which for a subscription bought in the App Store is not a page
        // this cloud owns at all.
        case 409:
            let blocked = try Self.decode(Blocked.self, from: data)
            switch blocked.provider {
            case "revenuecat":
                return .blockedByRenewal(source: .appStore, manageURL: Self.appStoreSubscriptions)
            default:
                return .blockedByRenewal(source: .web, manageURL: try await billingPortal(id))
            }
        default: throw Self.refusal(data, response)
        }
    }

    /// Where a web subscription is cancelled: a one-time link into the billing
    /// provider's own portal. Asked for only when a deletion is actually
    /// blocked, because the link is minted per press and expires.
    private func billingPortal(_ id: AccountId) async throws(CloudError) -> URL {
        var request = URLRequest(url: endpoint.stripePortal)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = Data("{}".utf8)
        let session = try? await ask(request, as: SessionURL.self, for: id)
        guard let url = session.flatMap({ URL(string: $0.url) }) else {
            // The portal is a convenience on top of the refusal, not the
            // refusal itself: somebody whose deletion is blocked is sent to
            // their account page, which is always there.
            return endpoint.base.appending(path: "account")
        }
        return url
    }

    // MARK: - Tokens

    /// The access token to knock with, refreshed when it is about to expire.
    ///
    /// A minute of slack, because a token that is valid when the request is
    /// built can be expired by the time it arrives, and the failure that
    /// causes is one nobody can act on.
    /// A bearer for an account: the one its sign-in obtained while it is
    /// fresh, and after that one the runtime lends. This app never spends a
    /// refresh token itself; the account's profile holds it.
    private func bearer(for id: AccountId) async throws(CloudError) -> String {
        if let session = sessions[id], !session.access.isEmpty,
           session.expiresAt > now().addingTimeInterval(60) {
            return session.access
        }
        guard let lender, let lent = await lender(id) else { throw .unauthenticated }
        return lent
    }

    private func exchange(_ form: [String: String]) async throws(CloudError) -> Issued {
        var request = URLRequest(url: endpoint.token)
        request.httpMethod = "POST"
        request.setValue(
            "application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        request.httpBody = Data(Self.form(form).utf8)
        let (data, response) = try await send(request)
        guard (200..<300).contains(response.statusCode) else {
            throw Self.refusal(data, response)
        }
        return try Self.decode(Issued.self, from: data)
    }

    private func who(with bearer: String) async throws(CloudError) -> Who {
        var request = URLRequest(url: endpoint.userinfo)
        request.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        let (data, response) = try await send(request)
        guard (200..<300).contains(response.statusCode) else {
            throw Self.refusal(data, response)
        }
        return try Self.decode(Who.self, from: data)
    }

    // MARK: - Talking

    private func ask<Answer: Decodable>(
        _ request: URLRequest, as: Answer.Type, for id: AccountId
    ) async throws(CloudError) -> Answer {
        let (data, response) = try await send(request, for: id)
        guard (200..<300).contains(response.statusCode) else {
            throw Self.refusal(data, response)
        }
        return try Self.decode(Answer.self, from: data)
    }

    private func send(
        _ request: URLRequest, for id: AccountId
    ) async throws(CloudError) -> (Data, HTTPURLResponse) {
        var authorized = request
        authorized.setValue(
            "Bearer \(try await bearer(for: id))", forHTTPHeaderField: "Authorization")
        return try await send(authorized)
    }

    private func send(
        _ request: URLRequest
    ) async throws(CloudError) -> (Data, HTTPURLResponse) {
        do {
            return try await transport.send(request)
        } catch let error as URLError where error.code == .timedOut {
            throw CloudError.timeout
        } catch let error as URLError where error.code == .cancelled {
            throw CloudError.cancelled
        } catch {
            // Everything else a network does is one thing to the person
            // holding the phone: it did not get there.
            throw CloudError.network((error as? URLError)?.localizedDescription
                ?? error.localizedDescription)
        }
    }

    /// What a refusal means, in the app's own words.
    ///
    /// The account service says `payment_required` when nothing is bought,
    /// which is not a failure to report but the second gate the home screen
    /// already draws — so it is said in the words that screen uses.
    static func refusal(_ data: Data, _ response: HTTPURLResponse) -> CloudError {
        let said = (try? JSONDecoder().decode(Refused.self, from: data))
        switch response.statusCode {
        case 401: return .unauthenticated
        case 403 where said?.error == "payment_required":
            return .refused("this account has no subscription")
        default: break
        }
        let reason = said?.error_description ?? said?.error
            ?? String(data: data, encoding: .utf8).flatMap { $0.isEmpty ? nil : $0 }
            ?? "amux.sh refused the request"
        return .refused(reason)
    }

    static func decode<Value: Decodable>(
        _ type: Value.Type, from data: Data
    ) throws(CloudError) -> Value {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        do {
            return try decoder.decode(type, from: data)
        } catch {
            throw CloudError.refused("amux.sh answered in a shape this app does not know")
        }
    }

    static func form(_ fields: [String: String]) -> String {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        return fields.keys.sorted().map { key in
            let value = fields[key]!.addingPercentEncoding(withAllowedCharacters: allowed) ?? ""
            return "\(key)=\(value)"
        }.joined(separator: "&")
    }

    /// A secret this phone makes for one sign-in, in the alphabet a URL can
    /// carry without escaping.
    static func randomToken() -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        for index in bytes.indices { bytes[index] = UInt8.random(in: .min ... .max) }
        return Data(bytes).base64URLEncoded
    }

    static func challenge(for verifier: String) -> String {
        Data(SHA256.hash(data: Data(verifier.utf8))).base64URLEncoded
    }

    /// Where an App Store subscription is managed. It is the system's own page
    /// and not a page this cloud can offer.
    public static let appStoreSubscriptions = URL(string: "https://apps.apple.com/account/subscriptions")!
}

// MARK: - What the cloud answers

/// Field names are the wire's, not this app's: they are what the account
/// service writes, and renaming them here would put a translation in the one
/// place a mismatch is hardest to see.
private struct Issued: Decodable {
    let access_token: String
    let refresh_token: String?
    let expires_in: Int?
}

private struct Who: Decodable {
    let sub: String
    let email: String?
    let name: String?
    let picture: String?
}

/// What a purchase is posted as. One field: the App Store's signed
/// transaction, whole.
private struct Purchase: Encodable {
    let signed_transaction: String
}

private struct Refused: Decodable {
    let error: String?
    let error_description: String?
}

private struct Blocked: Decodable {
    let error: String?
    let provider: String?
}

private struct SessionURL: Decodable {
    let url: String
}

private struct GraphQLAnswer<Payload: Decodable>: Decodable {
    let data: Payload?
}

private struct Me: Decodable {
    let me: Account?

    struct Account: Decodable {
        let access: Access?
    }

    /// What the account service says this account may do.
    ///
    /// `pro` is a plain yes or no and nothing here second-guesses it. The app
    /// used to decide for itself by comparing a subscription's end date with
    /// the clock, which is how an account the relay would let in ended up
    /// being shown a paywall; when access ends is the account service's
    /// answer, not a sum this phone does.
    struct Access: Decodable {
        let pro: Bool
        /// When access ends, or ended. Null when nothing ever ends.
        let until: Date?
        /// Why the account has it. Null when nothing was ever bought or given.
        let grant: Given?

        var entitlement: Entitlement {
            let given = grant?.grant
            // Access with no explanation beside it is still access. Demanding
            // one before believing `pro` would be gating on the explanation,
            // which is the whole mistake this read replaced.
            if pro { return .active(grant: given ?? .granted, renews: renews) }
            // Nothing to say about a lapse nobody can date, so it reads as
            // having nothing rather than as having ended on no day.
            guard let given, let until else { return .none }
            return .lapsed(grant: given, endedAt: until)
        }

        /// When this is charged again, where anything is. A grant renews on no
        /// date because nothing is being charged, and a subscription that has
        /// been cancelled but not yet run out renews on none either — the
        /// person paid for the month they are in.
        private var renews: Date? {
            guard let given = grant, given.willRenew == true else { return nil }
            return given.entitledUntil ?? until
        }
    }

    /// One member of the grant union.
    ///
    /// Only the purchase's fields are asked for: what a grant says about
    /// itself is a reason — complimentary, employee, beta — and this app has
    /// nothing different to show for each of them. The type name is what says
    /// which member arrived, so a member added later reads as a grant rather
    /// than as a purchase with everything missing.
    struct Given: Decodable {
        let typeName: String
        let provider: String?
        let willRenew: Bool?
        let entitledUntil: Date?

        enum CodingKeys: String, CodingKey {
            case typeName = "__typename"
            case provider, willRenew, entitledUntil
        }

        var grant: Grant {
            guard typeName == "Purchased", let provider else { return .granted }
            return .purchased(provider == "REVENUE_CAT" ? .appStore : .web)
        }
    }
}

extension Data {
    /// Base64 as a URL carries it: the two substituted characters put in and
    /// the padding taken off.
    var base64URLEncoded: String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
