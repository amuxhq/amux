import Foundation
import XCTest
@testable import AmuxCore

/// A transport that answers what the test says and keeps what it was asked.
///
/// Nothing here reaches a network. Every branch of the adapter — the redirect
/// it builds, the code it redeems, the refusal it reports, the deletion the
/// billing system blocks — is driven from this side, which is why the suite is
/// offline and takes no account with it.
private final class Answers: CloudTransport, @unchecked Sendable {
    struct Reply {
        var status: Int
        var body: String
    }

    private let lock = NSLock()
    private var replies: [String: Reply] = [:]
    /// Paths the network never gets to. A phone in a lift is not a status
    /// code, and the adapter has a branch for it that no reply can reach.
    private var unreachable: Set<String> = []
    private(set) var asked: [URLRequest] = []

    init(_ replies: [String: Reply]) {
        self.replies = replies
    }

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        lock.withLock { asked.append(request) }
        let path = request.url?.path() ?? ""
        if lock.withLock({ unreachable.contains(path) }) {
            throw URLError(.notConnectedToInternet)
        }
        let reply = lock.withLock { replies[path] } ?? Reply(status: 404, body: "{}")
        let response = HTTPURLResponse(
            url: request.url!, statusCode: reply.status,
            httpVersion: nil, headerFields: nil)!
        return (Data(reply.body.utf8), response)
    }

    func cannotReach(_ path: String) {
        lock.withLock { _ = unreachable.insert(path) }
    }

    func plus(_ path: String, status: Int, body: String) {
        lock.withLock { replies[path] = Reply(status: status, body: body) }
    }

    func request(_ path: String) -> URLRequest? {
        lock.withLock { asked.first { $0.url?.path() == path } }
    }

    func body(_ path: String) -> String {
        request(path)?.httpBody.flatMap { String(data: $0, encoding: .utf8) } ?? ""
    }

    func bearer(_ path: String) -> String? {
        request(path)?.value(forHTTPHeaderField: "Authorization")
    }
}

/// A presenter that answers with the callback amux.sh would have sent, and
/// records the URL it was handed.
private final class Handed: WebAuthPresenter, @unchecked Sendable {
    private let lock = NSLock()
    private var answer: (URL) -> Result<URL, CloudError>
    private(set) var opened: URL?

    init(_ answer: @escaping (URL) -> Result<URL, CloudError>) {
        self.answer = answer
    }

    func present(_ url: URL, callbackScheme: String) async throws(CloudError) -> URL {
        lock.withLock { opened = url }
        switch answer(url) {
        case .success(let callback): return callback
        case .failure(let error): throw error
        }
    }

    /// The callback the account service sends back, echoing the state the app
    /// put in the authorize URL.
    static func returning(code: String) -> Handed {
        Handed { url in
            let state = URLComponents(url: url, resolvingAgainstBaseURL: false)?
                .queryItems?.first { $0.name == "state" }?.value ?? ""
            return .success(URL(string: "amux://callback?code=\(code)&state=\(state)")!)
        }
    }
}

/// The moment every answer in this suite is read at.
private let now = Date(timeIntervalSince1970: 1_700_000_000)

final class AmuxCloudTests: XCTestCase {
    private let endpoint = CloudEndpoint(
        base: URL(string: "https://amux.test")!, clientID: "mobile",
        callback: URL(string: "amux://callback")!,
        scopes: ["openid", "profile", "email", "offline_access", "api"])

    private func service(_ answers: Answers) -> AmuxCloudService {
        AmuxCloudService(endpoint: endpoint, transport: answers, now: { now })
    }

    private var signedIn: Answers {
        Answers([
            "/connect/token": .init(status: 200, body: """
                {"access_token":"at-1","refresh_token":"rt-1","expires_in":3600}
                """),
            "/connect/userinfo": .init(status: 200, body: """
                {"sub":"ada","email":"ada@example.com","name":"Ada"}
                """),
        ])
    }

    func testSignInHandsOffWithPkceAndRedeemsTheCodeItComesBackWith() async throws {
        let answers = signedIn
        let presenter = Handed.returning(code: "code-1")
        let account = try await service(answers).signIn(.adding, presenting: presenter)

        XCTAssertEqual(account.id, AccountId("ada"))
        XCTAssertEqual(account.email, "ada@example.com")
        XCTAssertEqual(account.displayName, "Ada")

        let opened = try XCTUnwrap(presenter.opened)
        let query = try XCTUnwrap(
            URLComponents(url: opened, resolvingAgainstBaseURL: false)?.queryItems)
        func item(_ name: String) -> String? { query.first { $0.name == name }?.value }
        XCTAssertEqual(opened.host(), "amux.test")
        XCTAssertEqual(opened.path(), "/connect/authorize")
        XCTAssertEqual(item("client_id"), "mobile")
        XCTAssertEqual(item("response_type"), "code")
        XCTAssertEqual(item("redirect_uri"), "amux://callback")
        XCTAssertEqual(item("scope"), "openid profile email offline_access api")
        XCTAssertEqual(item("code_challenge_method"), "S256")

        // The verifier is what the app keeps and the challenge is its hash:
        // the redemption must carry the one the authorize URL committed to,
        // or nothing this app opened is what it redeemed.
        let redeemed = answers.body("/connect/token")
        let verifier = try XCTUnwrap(
            redeemed.split(separator: "&").first { $0.hasPrefix("code_verifier=") })
            .dropFirst("code_verifier=".count)
        XCTAssertEqual(item("code_challenge"), AmuxCloudService.challenge(for: String(verifier)))
        XCTAssertTrue(redeemed.contains("grant_type=authorization_code"))
        XCTAssertTrue(redeemed.contains("code=code-1"))
        XCTAssertTrue(redeemed.contains("client_id=mobile"))
        XCTAssertEqual(answers.bearer("/connect/userinfo"), "Bearer at-1")
    }

    /// Both ask amux.sh for its chooser, because without one it carries on as
    /// whoever the browser is signed in as; signing back into one account also
    /// names it, so its form has the address filled in.
    func testTheAuthorizeURLAsksForTheAccountTheSignInIsFor() async throws {
        let cloud = service(signedIn)
        let ada = SignedInAccount(id: AccountId("ada"), email: "ada+work@example.com")
        func items(_ intent: SignInIntent) async throws -> [String: String] {
            let url = await cloud.authorizeURL(verifier: "v", state: "s", intent: intent)
            let query = try XCTUnwrap(
                URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems)
            return Dictionary(uniqueKeysWithValues: query.map { ($0.name, $0.value ?? "") })
        }

        let adding = try await items(.adding)
        XCTAssertEqual(adding["prompt"], "select_account")
        XCTAssertNil(adding["login_hint"])

        let returning = try await items(.returning(ada))
        XCTAssertEqual(returning["login_hint"], "ada+work@example.com")
        XCTAssertEqual(returning["prompt"], "select_account")
        // Encoded, so an address with a plus in it reaches amux.sh as written.
        let url = await cloud.authorizeURL(verifier: "v", state: "s", intent: .returning(ada))
        XCTAssertTrue(url.absoluteString.contains("login_hint=ada%2Bwork%40example.com")
                      || url.absoluteString.contains("login_hint=ada%2Bwork@example.com"))
    }

    func testACallbackThatAnswersADifferentRequestIsNeverRedeemed() async {
        let answers = signedIn
        // Another app claiming the callback cannot know the state this phone
        // just made, so a code arriving with the wrong one is not this
        // sign-in's code.
        let presenter = Handed { _ in
            .success(URL(string: "amux://callback?code=stolen&state=someone-else")!)
        }
        await assert(.refused("that sign-in answered a different request")) {
            try await self.service(answers).signIn(.adding, presenting: presenter)
        }
        XCTAssertNil(answers.request("/connect/token"))
    }

    func testACallbackNamingAnErrorIsReportedInTheCloudsOwnWords() async {
        let presenter = Handed { _ in
            .success(URL(string:
                "amux://callback?error=access_denied&error_description=that%20address%20is%20not%20recognised")!)
        }
        await assert(.refused("that address is not recognised")) {
            try await self.service(self.signedIn).signIn(.adding, presenting: presenter)
        }
    }

    func testClosingTheBrowserIsCancelledRatherThanFailed() async {
        let presenter = Handed { _ in .failure(.cancelled) }
        await assert(.cancelled) {
            try await self.service(self.signedIn).signIn(.adding, presenting: presenter)
        }
    }

    // MARK: - A purchase reaching the cloud

    /// A purchase is taken either way: `200` is the cloud saying it is done,
    /// `202` the cloud saying it has it and will reconcile it. Neither is
    /// something the app has to tell apart, because what the account may then
    /// do comes from the entitlement read.
    func testASignedTransactionIsPostedAsTheAuthenticatedCallerAndTakenOnBothAnswers()
        async throws {
        for status in [200, 202] {
            let answers = signedIn
            answers.plus("/api/purchases", status: status, body: status == 200 ? "{}" : "")
            let cloud = service(answers)
            let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
            try await cloud.recordPurchase(account.id, signedTransaction: "signed.jws.one")

            let posted = answers.request("/api/purchases")
            XCTAssertEqual(posted?.httpMethod, "POST")
            XCTAssertEqual(answers.bearer("/api/purchases"), "Bearer at-1")
            XCTAssertEqual(
                posted?.value(forHTTPHeaderField: "Content-Type"), "application/json")
            // The signed transaction goes over whole, unread by this app.
            XCTAssertEqual(
                answers.body("/api/purchases"), #"{"signed_transaction":"signed.jws.one"}"#)
        }
    }

    func testAPhonesDeviceIsPutUnderItsHostIdWithTheBearerTheRuntimeLends() async throws {
        let host = HostId(UUID(uuidString: "AAAAAAAA-1111-1111-1111-111111111111")!)
        let answers = Answers([
            "/api/devices/aaaaaaaa-1111-1111-1111-111111111111": .init(status: 204, body: ""),
        ])
        let service = service(answers)
        await service.lend { _ in "lent-1" }
        try await service.registerDevice(
            AccountId("ada"), host: host,
            device: PushDevice(token: Data([0xab, 0x01]), environment: .sandbox))
        let request = try XCTUnwrap(answers.asked.last)
        XCTAssertEqual(request.httpMethod, "PUT")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer lent-1")
        let body = try JSONSerialization.jsonObject(with: try XCTUnwrap(request.httpBody))
        XCTAssertEqual(
            body as? [String: String],
            ["transport": "apns", "environment": "sandbox", "token": "ab01"])

        try await service.removeDevice(AccountId("ada"), host: host)
        XCTAssertEqual(answers.asked.last?.httpMethod, "DELETE")
        XCTAssertEqual(
            answers.asked.last?.url?.path(), "/api/devices/aaaaaaaa-1111-1111-1111-111111111111")
    }

    func testADeviceTheServiceRefusesIsAFailure() async {
        let host = HostId(UUID())
        let service = service(Answers([:]))
        await service.lend { _ in "lent-1" }
        do {
            try await service.removeDevice(AccountId("ada"), host: host)
            XCTFail("a 404 is no success")
        } catch {}
    }

    func testAPurchasePostedWithAnUnusableSessionIsUnauthenticated() async throws {
        let answers = signedIn
        answers.plus("/api/purchases", status: 401, body: "")
        let cloud = service(answers)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        await assert(.unauthenticated) {
            try await cloud.recordPurchase(account.id, signedTransaction: "signed.jws.one")
        }
    }

    /// The same refusal the home screen's gate already draws, said in the same
    /// words: an account the cloud does not consider paid for.
    func testAPurchaseRefusedForWantOfASubscriptionIsSaidInTheWordsTheGateUses() async throws {
        let answers = signedIn
        answers.plus("/api/purchases", status: 403, body: #"{"error":"payment_required"}"#)
        let cloud = service(answers)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        await assert(.refused("this account has no subscription")) {
            try await cloud.recordPurchase(account.id, signedTransaction: "signed.jws.one")
        }
    }

    func testATransactionTheCloudWillNotAcceptIsRefusedInItsOwnWords() async throws {
        let answers = signedIn
        answers.plus(
            "/api/purchases", status: 422,
            body: #"{"error":"invalid_transaction","error_description":"that transaction belongs to another account"}"#)
        let cloud = service(answers)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        await assert(.refused("that transaction belongs to another account")) {
            try await cloud.recordPurchase(account.id, signedTransaction: "signed.jws.one")
        }
    }

    /// A post that never arrives is not a refusal. The two read differently on
    /// the paywall because they are different situations, and this is where
    /// that difference is made.
    func testAPurchaseThatNeverLeftThePhoneIsANetworkFailureRatherThanARefusal() async throws {
        let answers = signedIn
        let cloud = service(answers)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        answers.cannotReach("/api/purchases")
        do {
            try await cloud.recordPurchase(account.id, signedTransaction: "signed.jws.one")
            XCTFail("a post that cannot leave the phone must not read as taken")
        } catch {
            guard case .network = error else {
                return XCTFail("expected a network failure, got \(error)")
            }
        }
    }

    func testEntitlementReadsTheGrantsProviderAndItsRenewal() async throws {
        let ends = Date(timeIntervalSince1970: 1_701_004_800)
        for (provider, source) in [("REVENUE_CAT", EntitlementSource.appStore),
                                   ("STRIPE", EntitlementSource.web)] {
            let answers = signedIn
            answers.plus("/api/graphql", status: 200, body: """
                {"data":{"me":{"access":{"pro":true,"until":"2023-11-26T13:20:00Z",
                 "grant":{"__typename":"Purchased","provider":"\(provider)",
                 "willRenew":true,"entitledUntil":"2023-11-26T13:20:00Z"}}}}}
                """)
            let cloud = service(answers)
            let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
            let entitlement = try await cloud.entitlement(account.id)
            XCTAssertEqual(entitlement, Entitlement.active(grant: .purchased(source), renews: ends))
        }
    }

    func testASubscriptionRidingOutItsPeriodIsActiveAndRenewsOnNoDate() async throws {
        let answers = signedIn
        answers.plus("/api/graphql", status: 200, body: """
            {"data":{"me":{"access":{"pro":true,"until":"2023-11-26T13:20:00Z",
             "grant":{"__typename":"Purchased","provider":"STRIPE",
             "willRenew":false,"entitledUntil":"2023-11-26T13:20:00Z"}}}}}
            """)
        let cloud = service(answers)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        let entitlement = try await cloud.entitlement(account.id)
        XCTAssertEqual(entitlement, Entitlement.active(grant: .purchased(.web), renews: nil))
    }

    /// An account that was given its access, which is how every complimentary,
    /// employee and beta account is entitled. Nothing was ever bought, so the
    /// old read — which asked about the billing record — answered null and the
    /// phone offered a paywall to somebody the relay was already letting in.
    func testAccessThatWasGivenRatherThanBoughtIsStillAccess() async throws {
        let answers = signedIn
        answers.plus("/api/graphql", status: 200, body: """
            {"data":{"me":{"access":{"pro":true,"until":null,
             "grant":{"__typename":"Granted"}}}}}
            """)
        let cloud = service(answers)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        let entitlement = try await cloud.entitlement(account.id)
        XCTAssertEqual(entitlement, Entitlement.active(grant: .granted, renews: nil))
        // One question is asked, and it is not about billing.
        let asked = answers.body("/api/graphql")
        XCTAssertTrue(asked.contains("access"), "the phone must ask what the account may do")
        XCTAssertFalse(
            asked.contains("subscription"), "asking about a subscription is asking the wrong thing")
    }

    /// `pro` is the whole gate. An answer that says yes and explains nothing is
    /// still yes: refusing it would be gating on the explanation, which is the
    /// mistake the old read made in the other direction.
    func testAccessWithNoGrantBesideItIsStillAccess() async throws {
        let answers = signedIn
        answers.plus("/api/graphql", status: 200, body: """
            {"data":{"me":{"access":{"pro":true,"until":null,"grant":null}}}}
            """)
        let cloud = service(answers)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        let entitlement = try await cloud.entitlement(account.id)
        XCTAssertEqual(entitlement, Entitlement.active(grant: .granted, renews: nil))
    }

    /// When access ended is the account service's answer, not a sum this phone
    /// does: the billing record here still says the subscription is active and
    /// renewing, and the phone reports what `pro` says regardless.
    func testAnEntitlementIsLapsedWhenTheCloudSaysSoWhateverTheBillingRecordSays()
        async throws
    {
        let answers = signedIn
        answers.plus("/api/graphql", status: 200, body: """
            {"data":{"me":{"access":{"pro":false,"until":"2023-11-13T13:20:00Z",
             "grant":{"__typename":"Purchased","provider":"REVENUE_CAT",
             "willRenew":true,"entitledUntil":"2023-11-13T13:20:00Z"}}}}}
            """)
        let cloud = service(answers)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        let entitlement = try await cloud.entitlement(account.id)
        XCTAssertEqual(
            entitlement,
            Entitlement.lapsed(
                grant: .purchased(.appStore),
                endedAt: Date(timeIntervalSince1970: 1_699_881_600)))
    }

    func testAnAccountThatBoughtNothingIsEntitledToNothing() async throws {
        let answers = signedIn
        answers.plus("/api/graphql", status: 200, body: """
            {"data":{"me":{"access":{"pro":false,"until":null,"grant":null}}}}
            """)
        let cloud = service(answers)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        let entitlement = try await cloud.entitlement(account.id)
        XCTAssertEqual(entitlement, Entitlement.none)
    }

    func testDeletionIsRefusedBeforeItLeavesWhenTheTypedAddressIsNotThisAccounts() async throws {
        let answers = signedIn
        let cloud = service(answers)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        await assert(.refused("that is not this account’s address")) {
            try await cloud.requestDeletion(account.id, confirmedEmail: "bo@example.com")
        }
        XCTAssertNil(answers.request("/api/account"))
    }

    func testDeletionGoesThroughWhenTheAddressMatches() async throws {
        let answers = signedIn
        answers.plus("/api/account", status: 200, body: "")
        let cloud = service(answers)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        let outcome = try await cloud.requestDeletion(
            account.id, confirmedEmail: "  ADA@example.com ")

        XCTAssertEqual(outcome, .deleted)
        XCTAssertEqual(answers.request("/api/account")?.httpMethod, "DELETE")
    }

    func testADeletionBlockedByAnAppStoreSubscriptionSendsYouToTheSystemsOwnPage() async throws {
        let answers = signedIn
        answers.plus(
            "/api/account", status: 409,
            body: #"{"error":"active_subscription","provider":"revenuecat"}"#)
        let cloud = service(answers)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        let outcome = try await cloud.requestDeletion(
            account.id, confirmedEmail: "ada@example.com")

        XCTAssertEqual(outcome, .blockedByRenewal(
            source: .appStore, manageURL: AmuxCloudService.appStoreSubscriptions))
        // Nothing this cloud can cancel, so nothing was asked of it.
        XCTAssertNil(answers.request("/api/billing/stripe/portal"))
    }

    func testADeletionBlockedByAWebSubscriptionSendsYouToTheBillingPortal() async throws {
        let answers = signedIn
        answers.plus(
            "/api/account", status: 409,
            body: #"{"error":"active_subscription","provider":"stripe"}"#)
        answers.plus(
            "/api/billing/stripe/portal", status: 200,
            body: #"{"url":"https://billing.test/session/1"}"#)
        let cloud = service(answers)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        let outcome = try await cloud.requestDeletion(
            account.id, confirmedEmail: "ada@example.com")

        XCTAssertEqual(outcome, .blockedByRenewal(
            source: .web, manageURL: URL(string: "https://billing.test/session/1")!))
    }

    /// The refresh token goes to the runtime once and is gone from here: a
    /// token that may be good for one use can have only one holder.
    func testTheRefreshTokenIsHandedOverOnce() async throws {
        let cloud = service(signedIn)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        let first = await cloud.handOver(account.id)
        let second = await cloud.handOver(account.id)
        XCTAssertEqual(
            first,
            Handover(cloud: URL(string: "https://amux.test")!, client: "mobile", refreshToken: "rt-1"))
        XCTAssertNil(second)
    }

    /// Once the sign-in's own access token has expired, a bearer is borrowed
    /// from the runtime; this app never refreshes one itself.
    func testAnExpiredAccessTokenIsBorrowedNotRefreshed() async throws {
        let answers = Answers([
            "/connect/token": .init(status: 200, body: """
                {"access_token":"at-1","refresh_token":"rt-1","expires_in":0}
                """),
            "/connect/userinfo": .init(status: 200, body: """
                {"sub":"ada","email":"ada@example.com","name":"Ada"}
                """),
            "/api/graphql": .init(status: 200, body: """
                {"data":{"me":{"access":{"pro":false,"until":null,"grant":null}}}}
                """),
        ])
        let cloud = service(answers)
        let account = try await cloud.signIn(.adding, presenting: Handed.returning(code: "code-1"))
        await cloud.lend { $0 == account.id ? "lent-1" : nil }
        _ = try await cloud.entitlement(account.id)

        XCTAssertEqual(answers.asked.filter { $0.url?.path() == "/connect/token" }.count, 1)
        XCTAssertEqual(answers.bearer("/api/graphql"), "Bearer lent-1")
    }

    func testAnAccountThisPhoneHasNotSignedIntoIsUnauthenticated() async {
        await assert(.unauthenticated) {
            try await self.service(self.signedIn).entitlement(AccountId("nobody"))
        }
    }

    private func assert<Value>(
        _ expected: CloudError, _ act: () async throws -> Value,
        file: StaticString = #filePath, line: UInt = #line
    ) async {
        do {
            _ = try await act()
            XCTFail("expected \(expected)", file: file, line: line)
        } catch let error as CloudError {
            XCTAssertEqual(error, expected, file: file, line: line)
        } catch {
            XCTFail("expected \(expected), got \(error)", file: file, line: line)
        }
    }
}
