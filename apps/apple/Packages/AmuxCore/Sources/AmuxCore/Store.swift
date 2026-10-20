import Foundation
import Observation

/// One thing that can be bought, as the store describes it.
///
/// The price is a string and not a number, because it is the store's own
/// rendering in the person's own currency and locale. An app that formatted a
/// number itself would eventually print a price the App Store does not charge.
public struct Plan: Sendable, Equatable, Identifiable, Codable {
    public enum Period: String, Sendable, Equatable, Codable {
        case monthly
        case yearly

        public var title: String {
            switch self {
            case .monthly: "Monthly"
            case .yearly: "Yearly"
            }
        }

        /// How the button says what is being bought: *Subscribe · £7.99 a
        /// month*.
        public var spelled: String {
            switch self {
            case .monthly: "a month"
            case .yearly: "a year"
            }
        }
    }

    public var id: String
    public var period: Period
    public var price: String
    /// What the longer plan is worth saying about itself, where there is
    /// something. Nothing rather than "0% off" when the two are priced the
    /// same: a saving that is not one is an argument, not a fact.
    public var saving: String?

    public init(id: String, period: Period, price: String, saving: String? = nil) {
        self.id = id
        self.period = period
        self.price = price
        self.saving = saving
    }

    /// The products this app sells. The identifiers are the account service's,
    /// which is what the receipt is matched against when the store tells it
    /// somebody paid.
    public static let monthlyID = "amux_pro_monthly"
    public static let yearlyID = "amux_pro_yearly"
    public static let identifiers = [monthlyID, yearlyID]
}

/// One purchase the App Store has signed, as it crosses out of StoreKit.
///
/// The signature is the whole point: it is what amux.sh checks before it will
/// believe this Apple Account paid, and it is the App Store's to make, not
/// this app's. Nothing here is a claim the phone could have invented.
public struct SignedPurchase: Sendable, Equatable, Codable, Identifiable {
    /// The App Store's own identifier for this transaction, as a string. It
    /// is what names the transaction again when it is time to finish it.
    public var id: String
    public var productID: String
    /// The signed transaction itself — a JSON Web Signature the App Store
    /// wrote, carried whole and never taken apart here.
    public var signed: String

    public init(id: String, productID: String, signed: String) {
        self.id = id
        self.productID = productID
        self.signed = signed
    }
}

/// What buying or restoring came to.
public enum PurchaseOutcome: Sendable, Equatable {
    /// Paid for, and the store says so now. The signed transaction comes with
    /// it, because the purchase is not somebody's until amux.sh has it.
    case bought(SignedPurchase)
    /// The store has taken it and cannot finish it yet — a child's purchase
    /// waiting on a parent, or a bank asking for a second factor. Nothing is
    /// owed and nothing has been bought; it may land minutes or days later.
    case pending
    case cancelled
    /// Nothing on this Apple Account to restore. Not a failure: it is the
    /// answer to the question the button asks.
    case nothingToRestore
}

public enum StoreError: Error, Sendable, Equatable {
    /// The store could not be reached or has nothing to sell.
    case unavailable(String)
    case failed(String)
}

/// Everything this app asks of the App Store.
///
/// One protocol, so the paywall never sees StoreKit: the production front and
/// the scripted one are the same shape, and every state a person can reach —
/// cancelled, pending, refused — is reachable in a test without a sandbox
/// account.
public protocol StoreFront: Sendable {
    func plans() async throws(StoreError) -> [Plan]
    func buy(_ plan: Plan) async throws(StoreError) -> PurchaseOutcome
    func restore() async throws(StoreError) -> PurchaseOutcome
    /// Tells the store this purchase is dealt with. Called only once amux.sh
    /// has taken the signed transaction: a transaction finished before that
    /// is one the App Store will never offer again, and the account it was
    /// bought for would never learn about it.
    func finish(_ purchase: SignedPurchase) async
    /// Everything the store is still holding — a purchase from a launch that
    /// ended before amux.sh answered, or one bought on another device for the
    /// same Apple Account.
    func unfinished() async -> [SignedPurchase]
    /// Purchases that arrive after the fact: a child's purchase a parent
    /// approved, a bank's second factor answered, a renewal. Nothing presses
    /// a button for these, so the app has to be listening.
    func approvals() -> AsyncStream<SignedPurchase>
}

/// Subscribing: what is on offer, what is chosen, and how a purchase went.
@MainActor
@Observable
public final class PaywallStore {
    public enum Phase: Sendable, Equatable {
        case ready
        /// The store's own sheet is up. Nothing on this screen may move while
        /// it is: what happens next is the store's to say.
        case buying
        /// Taken but not finished. The screen says so and stops offering to
        /// buy, because buying again would be a second charge for the same
        /// month.
        case awaitingApproval
        /// The store took the money and the signed transaction has gone to
        /// amux.sh, which has not answered yet. The subscription is not this
        /// account's until it does.
        case confirming
        /// Bought, and amux.sh will not confirm it. The purchase is kept and
        /// the transaction is not finished, so it can be sent again — from
        /// the button, or by the next launch on its own.
        case unconfirmed(Unconfirmed)
        case failed(String)
        /// Entitled: bought here or on the web, or given. What the screen says
        /// differs, so the phase carries which it was rather than assuming a
        /// purchase.
        case entitled(Grant)
    }

    /// Why a purchase that went through has not become access. The three read
    /// differently because they are different situations: a phone that could
    /// not get through, amux.sh saying no, and amux.sh saying yes to the
    /// purchase while the account it was bought for still has nothing.
    public enum Unconfirmed: Sendable, Equatable {
        /// The post never got there.
        case unreachable
        /// amux.sh would not take the transaction, in its own words.
        case refused(String)
        /// amux.sh took the purchase and the entitlement read back afterwards
        /// still says no access. The App Store's receipt reaches the account
        /// service through its own webhook, so the two can be minutes apart;
        /// the screen says access is on its way and goes on offering to ask
        /// again.
        case switchingOn

        /// The one word the screen and the driver name this state by.
        public var named: String {
            switch self {
            case .unreachable: "unreachable"
            case .refused: "refused"
            case .switchingOn: "switching on"
            }
        }
    }

    public private(set) var plans: [Plan] = []
    /// Which plan is selected. Yearly by default, which is what the drawing
    /// shows: it is the cheaper of the two per month, and preselecting the
    /// dearer one would be the app arguing for its own revenue.
    public private(set) var chosen: Plan.Period = .yearly
    public private(set) var phase: Phase = .ready
    /// What this account is already entitled to. A subscription bought
    /// anywhere else — on the web, through the CLI — is honoured here: the
    /// screen says where it came from instead of offering to sell a second.
    public private(set) var entitlement: Entitlement

    /// The purchase this phone is holding on behalf of amux.sh: bought, not
    /// yet confirmed, and not yet finished with the App Store. It is what
    /// Retry sends again.
    public private(set) var holding: SignedPurchase?

    public init(
        entitlement: Entitlement = .none, plans: [Plan] = [], phase: Phase = .ready,
        holding: SignedPurchase? = nil
    ) {
        self.entitlement = entitlement
        self.plans = plans
        self.phase = phase
        self.holding = holding
    }

    public var plan: Plan? {
        plans.first { $0.period == chosen } ?? plans.first
    }

    /// Whether there is anything to sell this person.
    ///
    /// An active entitlement is an active entitlement wherever it was bought.
    /// The App Store's own rules aside, selling a second subscription to
    /// somebody who already pays for one on the web would be taking money for
    /// nothing.
    public var entitled: Bool {
        if case .active = entitlement { return true }
        return false
    }

    /// Why this account has what it has, for the one line that says so.
    /// Nothing when there is nothing to describe.
    public var grant: Grant? {
        switch entitlement {
        case .active(let grant, _), .lapsed(let grant, _): grant
        case .none: nil
        }
    }

    /// Whether the screen is waiting on somebody else and must not be
    /// pressed: the store's own sheet, or amux.sh being told about it.
    public var working: Bool { phase == .buying || phase == .confirming }

    public func choose(_ period: Plan.Period) {
        guard !working else { return }
        chosen = period
    }

    /// Asks the store what it has. A store with nothing to sell is said
    /// plainly rather than drawn as an empty list of plans.
    public func load(from store: any StoreFront) async {
        do {
            plans = try await store.plans()
            if plans.isEmpty { phase = .failed("the App Store has nothing to sell right now") }
        } catch {
            phase = Self.phase(after: error)
        }
    }

    /// Buys the chosen plan.
    ///
    /// Cancelling puts the screen back exactly where it was, with the same
    /// plan chosen: closing the store's sheet is a person deciding not to buy
    /// now, and an error over it would be the app arguing with that.
    @discardableResult
    public func buy(_ store: any StoreFront) async -> PurchaseOutcome? {
        guard !working, !entitled, let plan else { return nil }
        phase = .buying
        do {
            let outcome = try await store.buy(plan)
            settle(outcome)
            return outcome
        } catch {
            phase = Self.phase(after: error)
            return nil
        }
    }

    /// Puts back a subscription this Apple Account already has.
    ///
    /// It exists for a person on a new phone, and it is the App Store's own
    /// requirement. Finding nothing is an answer and is said as one.
    @discardableResult
    public func restore(_ store: any StoreFront) async -> PurchaseOutcome? {
        guard !working else { return nil }
        phase = .buying
        do {
            let outcome = try await store.restore()
            settle(outcome)
            return outcome
        } catch {
            phase = Self.phase(after: error)
            return nil
        }
    }

    private func settle(_ outcome: PurchaseOutcome) {
        switch outcome {
        // Bought is not yet subscribed. What this account may do is amux.sh's
        // to say, and it has not been told yet; claiming the entitlement here
        // would put somebody on a home screen that cannot reach a host.
        case .bought(let purchase):
            holding = purchase
            phase = .confirming
        case .pending: phase = .awaitingApproval
        case .cancelled: phase = .ready
        case .nothingToRestore:
            phase = .failed("there is nothing on this Apple Account to restore")
        }
    }

    /// Tells amux.sh about a purchase, and only then finishes it with the
    /// App Store.
    ///
    /// That order is the whole of it. A transaction finished before the
    /// account service has the signed copy is one the App Store will never
    /// offer this app again, and the subscription somebody paid for would
    /// exist nowhere but on their bank statement.
    @discardableResult
    public func confirm(
        _ purchase: SignedPurchase, with cloud: any CloudService, as account: AccountId,
        finishing store: any StoreFront
    ) async -> Bool {
        holding = purchase
        phase = .confirming
        do {
            try await cloud.recordPurchase(account, signedTransaction: purchase.signed)
        } catch {
            phase = .unconfirmed(Self.unconfirmed(after: error))
            return false
        }
        await store.finish(purchase)
        holding = nil
        return true
    }

    /// Everything the App Store is still holding, sent again.
    ///
    /// This is what makes an unconfirmed purchase temporary without anybody
    /// pressing anything: a launch asks, and so does a purchase the store
    /// approves later. Answers whether any of them was taken, because that is
    /// what makes it worth reading the entitlement again.
    @discardableResult
    public func confirmOutstanding(
        in store: any StoreFront, with cloud: any CloudService, as account: AccountId
    ) async -> Bool {
        var taken = false
        for purchase in await store.unfinished() where !entitled {
            if await confirm(purchase, with: cloud, as: account, finishing: store) { taken = true }
        }
        return taken
    }

    /// Says a purchase is still not confirmed, where what stopped it happened
    /// outside this store — nobody signed in to record it against, or an
    /// entitlement that could not be read back afterwards.
    public func unconfirmed(_ why: Unconfirmed) {
        phase = .unconfirmed(why)
    }

    static func unconfirmed(after error: CloudError) -> Unconfirmed {
        switch error {
        case .refused(let reason): .refused(reason)
        // Everything else is a phone that did not get through, including a
        // session that has to be renewed first — which the next launch does
        // before it offers this purchase again.
        case .network, .timeout, .cancelled, .unauthenticated: .unreachable
        }
    }

    /// What the cloud says this account is entitled to, which outranks
    /// anything the store said: the account service is where a subscription
    /// actually lives, and the store only knows about the ones bought on it.
    public func entitled(_ entitlement: Entitlement) {
        self.entitlement = entitlement
        if case .active(let grant, _) = entitlement {
            if phase != .buying { phase = .entitled(grant) }
        } else if phase == .confirming {
            // A purchase amux.sh took, and an account that still has nothing.
            // Staying on "Confirming with amux.sh…" would leave somebody who
            // has paid looking at a screen with nothing on it they can press,
            // so the screen says access is still switching on and keeps the
            // button that asks again.
            phase = .unconfirmed(.switchingOn)
        }
    }

    static func phase(after error: StoreError) -> Phase {
        switch error {
        case .unavailable(let what): .failed(what)
        case .failed(let what): .failed(what)
        }
    }
}

extension EntitlementSource {
    /// Where a subscription was bought, as a person would say it. The App
    /// Store's own name, because that is where they would go to cancel it.
    public var named: String {
        switch self {
        case .appStore: "App Store"
        case .web: "amux.sh"
        }
    }

    /// The same place as a sentence names it. Apart from `named` because a row
    /// reporting where a subscription came from wants the bare name, and a
    /// sentence about going there to stop it wants the article.
    public var place: String {
        switch self {
        case .appStore: "the App Store"
        case .web: "amux.sh"
        }
    }
}

extension Grant {
    /// The two or three words a row has room for. A purchase names the place
    /// it was bought, because that is where somebody would go to change it; a
    /// grant names no place, because there is not one.
    public var named: String {
        switch self {
        case .purchased(let source): source.named
        case .granted: "Included"
        }
    }
}

extension Entitlement {
    /// Whether the relay carries this account's hosts, in two words. Where
    /// it was bought, and that hosts on this network work without it, are
    /// said on the page this opens, not on a row too narrow to hold them.
    public var summary: String {
        switch self {
        case .active: "Subscribed"
        case .none, .lapsed: "Not subscribed"
        }
    }
}
