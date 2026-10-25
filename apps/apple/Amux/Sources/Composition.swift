import AmuxCore
import AmuxDesign
import AmuxFeatures
import AmuxShell
import Foundation
import Observation
import UIKit
import UserNotifications

/// Everything the app is made of, assembled in one place.
///
/// The screens know their stores, the shell knows where things lead, and this
/// knows which stores and which shell — so switching account swaps a set of
/// stores here and nothing above has to be told about accounts at all.
@MainActor
@Observable
final class Composition {
    let accounts = AccountRegistry(file: AppFiles.support.appendingPathComponent("accounts.json"))
    let runtime: RuntimeCoordinator
    let router: Router
    /// The one sign-in this phone has in flight. It outlives the page that
    /// shows it, so a person who leaves the screen while the browser is up
    /// comes back to the attempt rather than to a fresh one.
    let signIn = SignInStore()
    /// What the App Store has to sell and how a purchase went. One per app:
    /// the store sells to an Apple Account, not to an amux one.
    let paywall = PaywallStore()
    /// The account somebody is in the middle of giving up. It outlives the
    /// question it is asked over: a deletion the billing system refuses sends
    /// the person out of the app to cancel a renewal, and coming back finds
    /// the same question with the same address typed.
    let deletion = DeletionStore()
    /// The account somebody is being asked about taking off this phone.
    let removal = RemovalStore()
    /// Keeps what the account on screen buys as amux.sh says it is now.
    private let subscription: EntitlementWatch
    /// What the app is wearing. Nothing means whatever the phone is set to,
    /// which is what most people want and what the app starts as.
    var appearance: Appearance?
    /// Whether this phone sends amux.sh usage events. On until somebody
    /// turns it off, and kept across launches: an off that a relaunch
    /// forgot would send what the person said not to.
    var shareUsage = UserDefaults.standard.object(forKey: Composition.shareUsageKey) as? Bool ?? true {
        didSet {
            UserDefaults.standard.set(shareUsage, forKey: Self.shareUsageKey)
            runtime.shareUsage = shareUsage
        }
    }
    private static let shareUsageKey = "shareUsage"
    /// Notifications as iOS has them, read at launch and whenever the app
    /// comes back to the front, since they are changed in the system's
    /// settings. None in a build that does not offer them.
    private(set) var notifications: NotificationsSetting?
    /// Whether the home's offer of notifications was put away. Kept across
    /// launches: the offer is made once, and Settings has them after.
    private var notificationsOfferDeclined =
        UserDefaults.standard.bool(forKey: Composition.notificationsOfferKey) {
        didSet {
            UserDefaults.standard.set(notificationsOfferDeclined, forKey: Self.notificationsOfferKey)
        }
    }
    private static let notificationsOfferKey = "notificationsOfferDeclined"
    /// Whether this build offers notifications at all. Nothing sends them to
    /// a phone yet, so the App Store build keeps them out of sight rather
    /// than ask for something that never arrives; the debug build offers
    /// them, for a push handed to the simulator.
    #if AMUX_DEBUG_TOOLS
    static let offersNotifications = true
    #else
    static let offersNotifications = false
    #endif

    /// Whether the home offers notifications above its list.
    var offerNotifications: Bool { notifications == .notAsked && !notificationsOfferDeclined }
    /// The one report this phone is in the middle of, from a screenshot or
    /// from Help. In every build: reporting a problem is for everybody.
    let reports = ReportStore()
    /// What photographs the screen and starts the dump a report carries.
    let freezer: any ReportFreezing
    /// A store failure replaces the entire shell; nothing cached or live is
    /// left drawable behind it.
    var storeFailure: String?
    /// The account service. Every screen sees it as `CloudService` and none of
    /// them knows there is HTTP behind it.
    let cloud: any CloudService
    /// The App Store, as the paywall sees it.
    private let store: any StoreFront
    /// Where signing in happens: the system's own browser.
    private let webAuth: any WebAuthPresenter
    /// The one place this app looks at the local network.
    let discovery: LocalDiscovery

    /// What the screens draw: the account on screen's stores, or the signed-out
    /// phone's.
    var stores: StoreBundle { runtime.stores }

    init() {
        let router = Router()
        self.router = router
        // The real services, unless a driven launch says otherwise: signing in
        // must not open a browser at amux.sh, buying must not reach the App
        // Store, and deleting must not delete anybody's account. The doubles
        // are the same shape, so everything above this line is the app either
        // way.
        #if AMUX_DEBUG_TOOLS
        let scripted = ProcessInfo.processInfo.arguments
            .contains("-\(Door.scriptedCloudArgument)")
        cloud = scripted ? DoorHost.shared.cloud : AmuxCloudService()
        store = scripted ? DoorHost.shared.store : AppStoreFront()
        webAuth = scripted ? DoorHost.shared.webAuth : WebSignIn()
        let options = Launch.options
        let only = Launch.discoverable
        #else
        cloud = AmuxCloudService()
        store = AppStoreFront()
        webAuth = WebSignIn()
        let options = RuntimeCoordinator.Options()
        let only: Set<HostId>? = nil
        #endif
        let runtime = RuntimeCoordinator(
            registry: accounts, support: AppFiles.support, deviceName: UIDevice.current.name,
            signedOut: StoreBundle(account: AccountId("signed-out")), options: options,
            networkWatch: SystemNetworkWatch())
        self.runtime = runtime
        // Only the system may look at the network a phone is on, so the
        // browser is the app's and the runtime is handed what it saw.
        discovery = LocalDiscovery(only: only) { [weak runtime] found in
            runtime?.discovered(found)
        }
        runtime.discovery = discovery
        let cloudService = cloud
        subscription = EntitlementWatch(
            registry: accounts,
            read: { id in try? await cloudService.entitlement(id) },
            relink: { [weak runtime] id in await runtime?.refreshEntitlement(for: id) ?? false })
        // Once a sign-in's own access token has expired, the app borrows one
        // from the runtime whose profile holds the account's refresh token.
        let lend: @Sendable (AccountId) async -> String? = { [weak runtime] account in
            await runtime?.bearer(for: account)
        }
        let cloud = cloud
        Task { await cloud.lend(from: lend) }
        let route = { router.top?.name ?? router.tab.rawValue }
        let dump: () -> Task<Result<URL, PartAbsent>, Never>? = { [weak runtime] in
            guard let running = runtime?.profile else { return nil }
            return Task {
                switch await running.dump(reason: "report") {
                case .success(let directory): .success(URL(fileURLWithPath: directory))
                case .failure(let why): .failure(PartAbsent(why.description))
                }
            }
        }
        #if AMUX_DEBUG_TOOLS
        freezer = ReportFreeze(
            route: route, dump: dump, trace: { DoorHost.shared.trace(route: route()) },
            runtimeFailure: { [weak runtime] in runtime?.failure })
        router.arrived = { [weak router] in
            guard let router else { return }
            DoorHost.shared.arrived(at: Self.place(for: router))
        }
        #else
        freezer = ReportFreeze(
            route: route, dump: dump, runtimeFailure: { [weak runtime] in runtime?.failure })
        #endif
        storeFailure = runtime.storeFailure
        runtime.storeFailureChanged = { [weak self] in self?.storeFailure = $0 }
        // A page pushed under the account just left is about that account's
        // machines and agents, however the account on screen changed.
        runtime.accountChanged = { [weak router] in router?.leaveAccount() }
        router.loads(with: self)
        runtime.shareUsage = shareUsage
        // Starting the runtime reads this phone's own store before it dials,
        // so the first frame has rows and needs no network.
        runtime.start()
        discovery.start()
        settleOutstandingPurchases()
        readNotifications()
        Signposts.emit(.compositionBuilt)
    }

    #if AMUX_DEBUG_TOOLS
    /// Where the app is, in the words a report's view-state recording names
    /// places by.
    static func place(for router: Router) -> Place {
        switch router.top {
        case .conversation(let agent): return .conversation(agent)
        case .changes(let agent): return .review(agent)
        case .some(let top): return .screen(top.name)
        case .none:
            return switch router.tab {
            case .agents: .home
            case .hosts: .hosts
            case .you: .settings
            }
        }
    }
    #endif

    /// What the shell asks for that it cannot do itself.
    func handle(_ action: ShellAction) {
        switch action {
        case .selectAccount(let id):
            accounts.select(id)
        case .addAccount:
            signIn.begin(.adding)
            router.open(.signIn(router.tab))
        // Sign In with no row behind it — the home's, or pairing's. With a
        // signed-out account on screen that is the account being signed back
        // into; otherwise it is the first account on this phone.
        case .signIn:
            let entry = accounts.selectedAccount.flatMap { $0.signedIn ? nil : $0 }
            signIn.begin(entry.map { .returning($0.account) } ?? .adding)
            router.open(.signIn(router.tab))
        case .signInAgain(let id):
            let entry = accounts.accounts.first { $0.id == id }
            signIn.begin(entry.map { .returning($0.account) } ?? .adding)
            router.open(.signIn(router.tab))
        case .keepSignIn:
            Task { await signIn.keep(keeping: keep) }
        case .discardSignIn:
            Task { await signIn.discard(with: cloud) }
        case .handOffSignIn:
            Task { await signIn.signIn(with: cloud, presenting: webAuth, keeping: keep) }
        // Subscribing is a page, and it asks the store what it has on the
        // way: the screen is useful before the answer arrives.
        case .subscribe:
            runtime.record(.paywallViewed(from: Self.paywallFrom(router.tab)))
            paywall.entitled(accounts.selectedAccount?.entitlement ?? .none)
            router.open(.paywall(router.tab))
            Task { await paywall.load(from: store) }
        case .buySubscription:
            runtime.record(.purchaseStarted(interval: Self.interval(paywall.chosen)))
            Task {
                guard case .bought(let purchase)? = await paywall.buy(store) else { return }
                await confirm(purchase)
            }
        case .restorePurchases:
            Task {
                guard case .bought(let purchase)? = await paywall.restore(store) else { return }
                await confirm(purchase)
            }
        // A purchase that went through and has not been confirmed, offered to
        // amux.sh again, or the entitlement read again where nothing is held.
        case .retryPurchase:
            Task {
                if let held = paywall.holding {
                    await confirm(held)
                } else if !(await refreshEntitlement()) {
                    paywall.unconfirmed(.unreachable)
                }
            }
        // Leaving an account. It stays listed with Sign In beside it, and its
        // profile stays, signed out: the machines on this network are still
        // reachable.
        case .signOutAccount(let id):
            Task { [cloud, runtime] in
                try? await cloud.forgetSession(id)
                await runtime.signOut(id)
            }
        case .removeAccount(let id):
            removal.ask(id)
        case .cancelRemoval:
            removal.dismiss()
        // Taking an account off this phone: its profile — its key, the
        // machines it paired, what it held — is deleted. The account on
        // amux.sh is untouched.
        case .confirmRemoval(let id):
            removal.dismiss()
            Task { [cloud] in try? await cloud.forgetSession(id) }
            forget(id)
        case .wear(let wanted):
            appearance = wanted
        case .shareUsage(let on):
            shareUsage = on
        case .turnOnNotifications:
            askForNotifications()
        case .notNowNotifications:
            notificationsOfferDeclined = true
        // Once iOS has asked, its own settings are the only place they are
        // changed: an app may ask once and never turn them off.
        case .notificationSettings:
            if notifications == .notAsked {
                askForNotifications()
            } else if let url = URL(string: UIApplication.openNotificationSettingsURLString) {
                UIApplication.shared.open(url)
            }
        case .deleteAccount(let id):
            deletion.ask(id)
        case .cancelDeletion:
            deletion.dismiss()
        case .confirmDeletion:
            Task { await deletion.delete(with: cloud, forgetting: { [weak self] in self?.forget($0) }) }
        case .exportDump:
            Task { await exportDump() }
        case .checkSubscription:
            Task { await subscription.look() }
        }
    }

    private static func paywallFrom(_ tab: Tab) -> PaywallFrom {
        switch tab {
        case .agents: .agents
        case .hosts: .hosts
        case .you: .you
        }
    }

    private static func interval(_ period: Plan.Period) -> BillingInterval {
        switch period {
        case .monthly: .monthly
        case .yearly: .yearly
        }
    }

    /// Reads what iOS says about notifications into `notifications`.
    func readNotifications() {
        guard Self.offersNotifications else { return }
        Task {
            let status = await UNUserNotificationCenter.current().notificationSettings()
                .authorizationStatus
            notifications = switch status {
            case .notDetermined: .notAsked
            case .authorized, .provisional, .ephemeral: .on
            case .denied: .off
            @unknown default: .off
            }
        }
    }

    private func askForNotifications() {
        Task {
            _ = try? await UNUserNotificationCenter.current()
                .requestAuthorization(options: [.alert, .sound, .badge])
            readNotifications()
        }
    }

    /// Takes away the notifications of agents that no longer need you: an
    /// ask answered on another machine leaves nothing on the lock screen to
    /// open onto a chat that has moved on. Only agents the fleet on screen
    /// lists as settled are cleared; one it does not list, on another
    /// account or folded into a family, is left for when it is.
    func clearSettledNotifications() {
        guard Self.offersNotifications else { return }
        let settled = Set(stores.fleet.rows.filter { !$0.needsYou }.map(\.id))
        guard !settled.isEmpty else { return }
        Task {
            let center = UNUserNotificationCenter.current()
            let delivered = await center.deliveredNotifications().map {
                (id: $0.request.identifier, payload: $0.request.content.userInfo)
            }
            let stale = PushPayload.stale(delivered, settled: settled)
            if !stale.isEmpty { center.removeDeliveredNotifications(withIdentifiers: stale) }
        }
    }

    /// The agents the fleet on screen lists as needing you; a change in it
    /// is when notifications may have gone stale.
    var waiting: Set<AgentKey> { Set(stores.fleet.rows.filter(\.needsYou).map(\.id)) }

    /// Opens the chat a notification names, putting the account whose
    /// profile trusts its host on screen first.
    func open(pushed agent: AgentKey) async {
        if let host = agent.hostId { await runtime.bringForward(host) }
        router.open(.conversation(agent))
    }

    private func forget(_ id: AccountId) {
        Task { [runtime] in await runtime.remove(id) }
    }

    /// Keeps an account that signed in: binds its profile with the
    /// sign-in's refresh token, which the profile spends from then on, and
    /// puts it on screen.
    private func keep(_ account: SignedInAccount) async -> CloudError? {
        guard let handover = await cloud.handOver(account.id) else { return .unauthenticated }
        let entitlement = try? await cloud.entitlement(account.id)
        accounts.entitlement(entitlement ?? .none, for: account.id)
        let bound = await runtime.bind(
            account.id, cloud: handover.cloud, client: handover.client,
            refreshToken: handover.refreshToken)
        switch bound {
        case .success: return nil
        case .failure(let why): return .refused(why.description)
        }
    }

    /// Writes this phone's dump and offers it to the share sheet.
    private func exportDump() async {
        guard case .success(let directory) = await stores.dump(reason: "exported from the phone")
        else { return }
        let sheet = UIActivityViewController(activityItems: [directory], applicationActivities: nil)
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let root = scenes.flatMap(\.windows).first(where: \.isKeyWindow)?.rootViewController
        var top = root
        while let presented = top?.presentedViewController { top = presented }
        top?.present(sheet, animated: true)
    }

    /// Reads what the account service says this account may do, after
    /// something changed what that is.
    ///
    /// Forced: a purchase has just changed the answer, and the relay link,
    /// which learns what the account buys only when it next renews its
    /// credential, should not wait minutes for it.
    @discardableResult
    private func refreshEntitlement() async -> Bool {
        guard await subscription.look(force: true) else { return false }
        paywall.entitled(accounts.selectedAccount?.entitlement ?? .none)
        return true
    }

    /// Tells amux.sh about a purchase and reads back what this account may now
    /// do.
    private func confirm(_ purchase: SignedPurchase) async {
        guard let id = accounts.selected else {
            // Nobody to record it against. The purchase is kept and the
            // transaction unfinished, so signing in and opening the app again
            // sends it.
            paywall.unconfirmed(.unreachable)
            return
        }
        guard await paywall.confirm(purchase, with: cloud, as: id, finishing: store) else { return }
        if !(await refreshEntitlement()) { paywall.unconfirmed(.unreachable) }
    }

    /// Sends anything the App Store is still holding, and goes on listening
    /// for purchases that are approved later.
    private func settleOutstandingPurchases() {
        Task {
            guard let id = accounts.selected else { return }
            if await paywall.confirmOutstanding(in: store, with: cloud, as: id) {
                await refreshEntitlement()
            }
        }
        Task {
            for await purchase in store.approvals() {
                await confirm(purchase)
            }
        }
    }
}

extension Composition: RouteLoader {
    // A chat page opens its chat when it first appears. It is closed when
    // its page leaves the stack rather than when it disappears, so a child's
    // chat pushed on top keeps the parent's draft and place.
    func load(_ route: Route) {}

    func left(_ route: Route) {
        if case .conversation(let agent) = route { stores.leave(agent) }
    }
}

/// Where this app keeps things between launches: the installation, with a
/// profile per account, under the support directory, beside what is
/// remembered about the accounts.
enum AppFiles {
    static let support = directory(.applicationSupportDirectory)

    /// What this build calls itself in a report: the thing that wrote it,
    /// then its version.
    static var build: String {
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString")
        return "amux-ios/\(version as? String ?? "0")"
    }

    static var gitSHA: String {
        Bundle.main.object(forInfoDictionaryKey: "AmuxGitSHA") as? String ?? ""
    }

    private static func directory(_ search: FileManager.SearchPathDirectory) -> URL {
        let manager = FileManager.default
        let root = manager.urls(for: search, in: .userDomainMask)[0]
            .appendingPathComponent("amux", isDirectory: true)
        try? manager.createDirectory(at: root, withIntermediateDirectories: true)
        return root
    }
}
