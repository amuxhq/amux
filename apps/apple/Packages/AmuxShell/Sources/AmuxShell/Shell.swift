import AmuxCore
import AmuxDesign
import AmuxFeatures
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers
#if canImport(UIKit)
import UIKit
#endif

/// Puts text on the system clipboard.
///
/// The clipboard is the system's and not this app's, so it is reached from
/// here rather than from a screen: a screen that touched UIKit would be a
/// screen that could not be photographed or replayed away from a device.
@MainActor
private func copy(_ text: String) {
    #if canImport(UIKit)
    UIPasteboard.general.string = text
    #endif
}

/// What the shell cannot do for itself and hands back to whoever assembled the
/// app: anything that reaches the cloud, the store or another account.
public enum ShellAction: Equatable, Sendable {
    case selectAccount(AccountId)
    case addAccount
    /// Somebody asked to sign in. Where that leads is a page; what it does
    /// when it gets there is the cloud's, which is why both leave the shell.
    case signIn
    /// Sign back into one account this phone already lists. Kept apart from
    /// adding one, because amux.sh is asked for that account by name and
    /// whoever comes back is checked against it.
    case signInAgain(AccountId)
    /// A sign-in came back as another account than the one asked for, and
    /// the person wants it on the phone anyway.
    case keepSignIn
    /// The same, and the person does not.
    case discardSignIn
    /// The press on the sign-in screen itself: hand off to the account
    /// service. The shell does not do this because it reaches nothing.
    case handOffSignIn
    case subscribe
    /// Buy the plan the paywall has selected.
    case buySubscription
    /// Put back a subscription this Apple Account already has.
    case restorePurchases
    /// Send a purchase amux.sh has not confirmed to it again.
    case retryPurchase
    /// Leave an account. It stays listed with Sign In beside it.
    case signOutAccount(AccountId)
    /// Somebody asked to take an account off this phone. What that does is
    /// said first, over the page it was asked from.
    case removeAccount(AccountId)
    /// The answer to that question.
    case confirmRemoval
    case cancelRemoval
    /// Somebody asked to give up an account for good. What that costs is a
    /// question the app asks before anything leaves this phone.
    case deleteAccount(AccountId)
    /// The answer to that question, with the address typed. It reaches the
    /// account service, so the shell does not do it.
    case confirmDeletion
    /// Changed their mind about it.
    case cancelDeletion
    /// Light, dark, or whatever the phone is set to.
    case wear(Appearance?)
    /// Turn sending amux.sh usage events on or off.
    case shareUsage(Bool)
    /// Write this phone's diagnostic dump and offer it to share.
    case exportDump
    /// Ask the account service what the account on screen buys now: the app
    /// came back to the front, or You is on screen. A subscription bought or
    /// lapsed elsewhere reaches the phone only by asking.
    case checkSubscription
}

/// Leaves the app for a page somewhere else: a billing portal, the App Store's
/// own subscriptions page, a way to reach a person.
///
/// Reached from here rather than from a screen for the same reason the
/// clipboard is: a screen that opened a URL would be a screen that could not
/// be photographed or replayed away from a device.
@MainActor
private func leave(for url: URL) {
    #if canImport(UIKit)
    UIApplication.shared.open(url)
    #endif
}

/// The app: three tabs, a stack under each, and a title menu on the Agents
/// tab that names the account whose fleet is on show.
///
/// Everything here is navigation and composition. The screens themselves are
/// functions of their stores and never reach for a route, which is why one can
/// be captured or replayed on its own.
public struct Shell: View {
    @Environment(\.design) private var design
    private let router: Router
    private let accounts: AccountRegistry
    private let stores: StoreBundle
    /// The one sign-in this phone has in flight. It is not an account's store
    /// because there is no account until it finishes.
    private let signIn: SignInStore
    /// What is on offer and how a purchase went. One per app, not per account:
    /// the App Store sells to an Apple Account, not to an amux one.
    private let paywall: PaywallStore
    /// The account this phone is in the middle of giving up, if any. Its own
    /// store because the question outlives the page it is asked over: leaving
    /// to cancel a renewal and coming back finds the same question, with the
    /// address still typed.
    private let deletion: DeletionStore
    /// The account this phone is asking about taking off it, if any.
    private let removal: RemovalStore
    /// What the app is wearing, or nothing for whatever the phone is set to.
    private let appearance: Appearance?
    /// Whether this phone sends amux.sh usage events.
    private let shareUsage: Bool
    /// Freezes the screen and opens a report on it, from Help. The app owns
    /// the capture; the shell only knows where the row that asks for it is.
    private let report: @MainActor () -> Void
    private let actions: @MainActor (ShellAction) -> Void
    @Environment(\.scenePhase) private var scenePhase

    public init(
        router: Router,
        accounts: AccountRegistry,
        stores: StoreBundle,
        signIn: SignInStore,
        paywall: PaywallStore,
        deletion: DeletionStore,
        removal: RemovalStore = RemovalStore(),
        appearance: Appearance? = nil,
        shareUsage: Bool = true,
        report: @escaping @MainActor () -> Void = {},
        actions: @escaping @MainActor (ShellAction) -> Void
    ) {
        Design.app.dressNavigationBars()
        self.appearance = appearance
        self.shareUsage = shareUsage
        self.deletion = deletion
        self.removal = removal
        self.router = router
        self.accounts = accounts
        self.stores = stores
        self.signIn = signIn
        self.paywall = paywall
        self.report = report
        self.actions = actions
    }

    /// Whether a bottom sheet or card is up over the tab on screen.
    private var coveredByASheet: Bool {
        switch router.tab {
        case .hosts: stores.hosts.readingDevices
        // The settings sheet's own cards cover the sheet, not a tab.
        case .agents, .you: false
        }
    }

    public var body: some View {
        @Bindable var router = router
        // The system's tab bar, for the system's behaviour: it builds a tab
        // when it is first reached and keeps it, and it hides under a pushed
        // page so a conversation ends with its composer.
        TabView(selection: Binding(get: { router.beneath }, set: { router.select($0) })) {
            SwiftUI.Tab(Tab.agents.title, systemImage: Tab.agents.symbol, value: Tab.agents) {
                NavigationStack(path: $router.agentsPath) {
                    AgentsTab(
                        router: self.router, accounts: accounts, stores: stores,
                        actions: actions)
                        .barUnderSheet(coveredByASheet)
                        .navigationDestination(for: Route.self) { page($0).toolbar(.hidden, for: .tabBar) }
                }
            }
            SwiftUI.Tab(Tab.hosts.title, systemImage: Tab.hosts.symbol, value: Tab.hosts) {
                NavigationStack(path: $router.hostsPath) {
                    HostsTabRoot(router: self.router, stores: stores)
                        .barUnderSheet(coveredByASheet)
                        .navigationDestination(for: Route.self) { page($0).toolbar(.hidden, for: .tabBar) }
                }
            }
        }
        // Settings are a sheet raised from the account disc rather than a
        // tab: somewhere visited now and then does not earn a place beside
        // the two that are used all day. The sheet keeps a stack of its own,
        // so signing in or subscribing from it pushes inside it.
        .sheet(isPresented: Binding(
            get: { router.tab == .you },
            set: { shown in if !shown, router.tab == .you { router.select(router.beneath) } }
        )) {
            settings
        }
        // Starting an agent is raised over whatever tab asked for it, the way
        // a new message is: the list stays beneath, and Cancel puts it away.
        .sheet(isPresented: $router.composing) {
            NewAgentPage(router: self.router, stores: stores)
        }
        .tint(design.accentColor)
        .reported("shell", value: router.tab.rawValue)
        .simultaneousGesture(backGesture)
        // What the account buys is read again as the app comes to the front
        // and as You comes on screen; the composition spaces the reads out.
        .onChange(of: scenePhase, initial: true) { _, phase in
            if phase == .active { actions(.checkSubscription) }
        }
        .onChange(of: router.tab) { _, tab in
            if tab == .you && scenePhase == .active { actions(.checkSubscription) }
        }
    }

    /// The settings sheet's content: its page in a stack of its own. Public
    /// so a picture of the sheet can be taken without presenting it.
    public var settings: some View {
        @Bindable var router = router
        return NavigationStack(path: $router.youPath) {
            YouTabRoot(
                router: self.router, accounts: accounts, stores: stores,
                deletion: deletion, removal: removal, appearance: appearance,
                shareUsage: shareUsage, report: report, actions: actions)
                .navigationDestination(for: Route.self) { page($0) }
        }
        .tint(design.accentColor)
    }

    /// Restores the platform's edge-to-pop interaction while the custom page
    /// chrome keeps the system navigation bar out of the approved layout.
    private var backGesture: some Gesture {
        DragGesture(minimumDistance: 12, coordinateSpace: .global)
            .onEnded { value in
                guard !router.path.isEmpty,
                      value.startLocation.x <= 24,
                      value.translation.width >= 80,
                      abs(value.translation.width) > abs(value.translation.height)
                else { return }
                router.pop()
            }
    }

    /// One page per route, every route with a built screen: the switch has
    /// no default, so a new route does not compile until it has one.
    @ViewBuilder
    private func page(_ route: Route) -> some View {
        switch route {
        case .conversation(let agent):
            ChatPage(agent: agent, router: router, stores: stores)
        case .changes(let agent):
            ChangesPage(agent: agent, router: router, stores: stores)
        case .pairByCode(let host):
            PairByCodePage(host: host, router: router, stores: stores, actions: actions)
        case .pairConfirmation(let invitation):
            PairConfirmationPage(
                invitation: invitation, router: router, stores: stores, actions: actions)
        case .signIn(let from):
            SignInPage(from: from, router: router, model: signIn, actions: actions)
        case .paywall(let from):
            PaywallPage(from: from, router: router, model: paywall, actions: actions)
        case .host(let host):
            HostPage(model: stores.hosts, host: host) { action in
                switch action {
                case .back: router.pop()
                // The page is about a machine that is leaving the list, so
                // it goes with it.
                case .revoke(let host):
                    stores.revoke(host)
                    router.pop()
                case .pairAgain(let host):
                    router.open(.pairByCode(host))
                }
            }
            .toolbar(.hidden, for: .navigationBar)
        }
    }
}

private extension View {
    /// A bottom sheet or card over a tab's root takes the tab bar's place:
    /// the bar drawn over it would cover its last row. Uncovered, the root
    /// leaves the bar to the system rather than asking for it, since a root
    /// that asks for the bar outranks a pushed page that hides it.
    func barUnderSheet(_ covered: Bool) -> some View {
        toolbar(covered ? .hidden : .automatic, for: .tabBar)
    }
}

/// An agent's changes for the comparison its chat has chosen, pushed over
/// the chat from the changes chip or the overview. It reviews the diff the
/// host froze as the page opened, and keeps its comments on the chat so
/// leaving and coming back loses none.
private struct ChangesPage: View {
    let agent: AgentKey
    let router: Router
    let stores: StoreBundle
    @State private var chat: ChatModel?
    @State private var review: ReviewModel?
    /// What the review was frozen against, fixed as it opened.
    @State private var against: String?
    /// Why there is no review: nothing changed, or the host was not asked.
    @State private var missing: String?

    var body: some View {
        Group {
            if let chat, let review {
                ReviewPage(model: review, against: against) { action in
                    switch action {
                    case .back: router.pop()
                    case .attach:
                        chat.attach(review)
                        router.pop()
                    }
                }
            } else {
                ZStack(alignment: .topLeading) {
                    Ground()
                    if let missing {
                        Explain(missing)
                            .identified("review.empty")
                            .padding(.horizontal, 20)
                            .padding(.top, 10)
                    }
                }
                .navigationBarBackButtonHidden(true)
                .toolbar {
                    ToolbarItem(placement: .topBarLeading) {
                        BarBack(identifier: "review.back") { router.pop() }
                    }
                }
            }
        }
        .task {
            guard chat == nil, let model = try? stores.chat(agent) else { return }
            chat = model
            against = ChatWords.comparison(model.comparison, base: model.base)
            switch await model.openReview() {
            case .success(let opened?): review = opened
            case .success(nil):
                missing = model.comparison == .onBranch
                    ? String(localized: "This agent has no changes on its branch.")
                    : String(localized: "This agent has no uncommitted changes.")
            case .failure(let failure): missing = failure.description
            }
        }
    }
}

/// An agent's chat. The page asks the account's stores for it, which keep
/// it open until the page leaves the stack; photos and files are picked
/// here, and everything that reaches the agent's lifecycle or the clipboard
/// goes through the stores and the router.
private struct ChatPage: View {
    let agent: AgentKey
    let router: Router
    let stores: StoreBundle
    @State private var model: ChatModel?
    @State private var failure: String?
    @State private var picking: AttachChoice?
    @State private var photo: PhotosPickerItem?
    @State private var speech = SpeechDictation()
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.openURL) private var openURL
    /// Whether the fleet has listed this agent since the page opened, so its
    /// leaving the fleet reads as deleted rather than as not yet listed.
    @State private var listed = false

    var body: some View {
        Group {
            if let model {
                ChatScreen(
                    model: model, subject: subject, family: stores.profile?.family(agent),
                    actions: act)
            } else {
                ZStack(alignment: .topLeading) {
                    Ground()
                    VStack(alignment: .leading, spacing: 14) {
                        BackLink("Agents", identifier: "chat.back") { router.pop() }
                        Explain(failure ?? String(localized: "Opening this chat"))
                            .identified("chat.failure", value: failure ?? "")
                    }
                    .padding(.horizontal, 20)
                    .padding(.top, 10)
                }
                .toolbar(.hidden, for: .navigationBar)
            }
        }
        .photosPicker(
            isPresented: Binding(
                get: { picking == .photo }, set: { if !$0 { picking = nil } }),
            selection: $photo, matching: .images)
        .fileImporter(
            isPresented: Binding(get: { picking == .file }, set: { if !$0 { picking = nil } }),
            allowedContentTypes: [.item]
        ) { result in
            guard case .success(let url) = result else { return }
            attach(file: url)
        }
        .onChange(of: photo) { _, item in
            guard let item else { return }
            photo = nil
            Task { await attach(photo: item) }
        }
        .onAppear(perform: open)
        .onChange(of: stores.fleet.row(agent) != nil) { _, present in
            if present { listed = true } else if listed { router.pop() }
        }
        .onChange(of: model?.frame?.ended != nil) { _, ended in
            if ended { router.pop() }
        }
        // Dictation stops when the chat ends it (a send, an edit, the
        // composer going away), when the page leaves, and in the background.
        .onChange(of: model?.dictation.active == true) { _, active in
            if !active { speech.stop() }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { speech.stop() }
        }
        .onDisappear { speech.stop() }
    }

    private func open() {
        guard model == nil else { return }
        listed = stores.fleet.row(agent) != nil
        do {
            model = try stores.chat(agent)
        } catch {
            failure = error.description
        }
    }

    private var subject: ChatSubject {
        let row = stores.fleet.row(agent)
        let host = stores.fleet.host(agent.hostId)
        let name = row?.name ?? model?.frame?.name ?? agent.description
        // The card counts what a stop cuts on hosts this phone cannot see; a
        // folded family's member has one though home lists no row for it.
        let card = row?.card ?? stores.profile?.card(agent)
        return ChatSubject(
            name: name.isEmpty ? agent.description : name,
            host: host?.name ?? row?.hostName ?? "",
            directory: row?.workingDirectory ?? "",
            reach: host?.reach ?? row?.hostReach ?? .offline(.plain),
            descendants: Int(card?.descendants ?? 0),
            backgroundJobs: Int(card?.backgroundJobs ?? 0),
            stopHeld: card?.stopHeld ?? false)
    }

    private func act(_ action: ChatAction) {
        switch action {
        case .back: router.pop()
        case .attach(let choice): picking = choice
        case .rename(let name): Task { _ = await stores.rename(name, of: agent) }
        case .stopAgent: Task { _ = await stores.stop(agent) }
        case .cancelStop: Task { _ = await stores.cancelStop(agent) }
        case .delete:
            Task {
                if case .success = await stores.delete(agent) { router.pop() }
            }
        case .copyAddress: copy(subject.address)
        case .open(let other): router.open(.conversation(other))
        case .review: router.open(.changes(agent))
        case .dictate:
            if let model { speech.toggle(model) }
        case .dictationSettings:
            if let settings = URL(string: UIApplication.openSettingsURLString) { openURL(settings) }
        }
    }

    /// A photo as JPEG, the one image type every agent kind reads.
    private func attach(photo item: PhotosPickerItem) async {
        guard let data = try? await item.loadTransferable(type: Data.self) else { return }
        #if canImport(UIKit)
        let jpeg = UIImage(data: data)?.jpegData(compressionQuality: 0.85) ?? data
        #else
        let jpeg = data
        #endif
        model?.attach(jpeg, name: "photo.jpg", mime: "image/jpeg", image: true)
    }

    private func attach(file url: URL) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        // A file over the limit is refused by its size, before it is read.
        if let size = try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize,
           model?.admits(name: url.lastPathComponent, size: size) == false {
            return
        }
        guard let data = try? Data(contentsOf: url) else { return }
        let type = UTType(filenameExtension: url.pathExtension)
        model?.attach(
            data, name: url.lastPathComponent,
            mime: type?.preferredMIMEType ?? "application/octet-stream",
            image: type?.conforms(to: .image) ?? false)
    }
}

private struct AgentsTab: View {
    let router: Router
    let accounts: AccountRegistry
    let stores: StoreBundle
    let actions: @MainActor (ShellAction) -> Void

    var body: some View {
        AgentsHome(model: stores.fleet, accounts: accounts, hosts: stores.hosts) { action in
            switch action {
            case .open(let agent):
                stores.fleet.opened(agent)
                router.open(.conversation(agent))
            case .newAgent: router.composing = true
            case .openExceptions: router.select(.hosts)
            case .switchAccount(let id): actions(.selectAccount(id))
            case .addAccount: actions(.addAccount)
            case .signIn: actions(.signIn)
            case .signInAgain(let id): actions(.signInAgain(id))
            case .subscribe: actions(.subscribe)
            case .openSettings: router.select(.you)
            // Pairing lives under Hosts wherever it is started from: the page
            // it opens is that tab's, and going back from it belongs there
            // rather than on a list of agents that has none.
            case .pair(let host):
                router.select(.hosts)
                router.open(.pairByCode(host))
            // The one place the list is allowed to regroup. Data arriving
            // never reorders what a thumb is already travelling towards.
            case .refresh: stores.fleet.refreshOrder(now: stores.now())
            case .toggleFamily(let head): stores.toggleFamily(head)
            case .filter(let filter): stores.filterFleet(filter)
            }
        }
    }
}

/// Signing in.
///
/// The page navigates and the composition reaches the cloud: leaving is a pop
/// either way, and the hand-off is handed out of the shell because a browser,
/// a token and an account are none of the shell's business.
private struct SignInPage: View {
    let from: Tab
    let router: Router
    let model: SignInStore
    let actions: @MainActor (ShellAction) -> Void

    var body: some View {
        SignIn(model: model, back: from.title) { action in
            switch action {
            case .cancel, .done: router.pop()
            case .start: actions(.handOffSignIn)
            case .keep: actions(.keepSignIn)
            case .discard: actions(.discardSignIn)
            }
        }
        // The screen draws its own header, so the bar would be a second one.
        .toolbar(.hidden, for: .navigationBar)
    }
}

/// Subscribing.
///
/// Choosing a plan is the screen's own state and is settled here; buying and
/// restoring leave the shell, because both are the App Store's and the shell
/// reaches nothing.
private struct PaywallPage: View {
    let from: Tab
    let router: Router
    let model: PaywallStore
    let actions: @MainActor (ShellAction) -> Void

    var body: some View {
        Paywall(model: model, back: from.title) { action in
            switch action {
            case .cancel, .done: router.pop()
            case .choose(let period): model.choose(period)
            case .buy: actions(.buySubscription)
            case .restore: actions(.restorePurchases)
            case .retry: actions(.retryPurchase)
            }
        }
        // The screen draws its own header, so the bar would be a second one.
        .toolbar(.hidden, for: .navigationBar)
    }
}

private struct HostsTabRoot: View {
    let router: Router
    let stores: StoreBundle

    var body: some View {
        HostsTab(model: stores.hosts) { action in
            switch action {
            case .open(let host): router.open(.host(host))
            case .pair(let host): router.open(.pairByCode(host))
            case .newAgent: router.composing = true
            // Revoking goes nowhere: the list it happens on is the list it
            // changes, and the machine leaves it when the runtime says so.
            case .revoke(let host): stores.revoke(host)
            // A local network this phone refused is granted back in the
            // system's settings and nowhere else: iOS asks once, and this app
            // has no second ask to offer.
            case .openSystemSettings:
                if let url = URL(string: UIApplication.openSettingsURLString) {
                    UIApplication.shared.open(url)
                }
            }
        }
        // The screen draws its own header, so the bar would be a second one.
        .toolbar(.hidden, for: .navigationBar)
    }
}

/// Starting an agent on one of this phone's machines.
///
/// The page opens the attempt rather than the screen doing it, and asks the
/// machine it opened on what it has to offer straight away: the answer takes a
/// round trip and the screen is useful before it arrives, so nothing waits on
/// it.
private struct NewAgentPage: View {
    @Environment(\.design) private var design
    let router: Router
    let stores: StoreBundle

    var body: some View {
        NavigationStack {
            NewAgent(model: stores.newAgent, hosts: stores.hosts) { action in
                switch action {
                // A different machine means a different set of directories,
                // so pointing at one asks it what it has.
                case .point(let host): stores.point(at: host)
                case .search: stores.searchDirectories()
                // Starting is the one thing on this screen that leaves the
                // phone. Leaving is what the confirmation below does, when the
                // machine says the agent exists — a screen that left on the
                // press would be claiming something it has not been told.
                case .start: stores.startAgent()
                case .cancel: router.composing = false
                }
            }
        }
        .tint(design.accentColor)
        .onAppear { stores.startNewAgent(on: stores.hosts.online.first?.id) }
        // The sheet goes and the conversation opens over the list: going
        // back from an agent that was just started belongs at the list it
        // joined, not at the form that made it.
        .onChange(of: stores.newAgent.created) { _, started in
            guard let started else { return }
            stores.fleet.opened(started)
            router.started(started)
        }
    }
}

/// Typing a machine's six-digit code, and then deciding about the machine that
/// answered it.
///
/// The page opens the attempt rather than the screen doing it, because opening
/// one is what clears the last one: a refusal left on screen from the code
/// somebody typed a minute ago would be read as this code failing.
///
/// A code that authenticates leads to the same confirmation a link leads to,
/// on this page rather than on another one. Pairing is two acts on purpose —
/// knowing the code proves possession of the offer and nothing about which
/// machine made it — so the digits are never the end of it: what the machine
/// said its name and its key are has to be read and agreed to before any trust
/// is written. Keeping both halves here means the person who typed the code
/// stays where they typed it, and going back from either is going back to
/// Hosts.
private struct PairByCodePage: View {
    let host: HostId
    let router: Router
    let stores: StoreBundle
    /// Buying the relay tunnel, for the one refusal a code cannot be typed out
    /// of: the machine answered and this account may not open one to it.
    let actions: @MainActor (ShellAction) -> Void

    var body: some View {
        page
            .toolbar(.hidden, for: .navigationBar)
            .onAppear { stores.pairing.open(machine: stores.hosts.known(host)) }
    }

    @ViewBuilder
    private var page: some View {
        switch stores.pairing.phase {
        // A refusal belongs to the digits: it is the code that did not work,
        // and the next thing to do is type another one.
        case .entering, .checking, .refused, .needsSubscription: digits
        case .confirming, .trusted: decision
        }
    }

    private var digits: some View {
        PairByCode(model: stores.pairing) { action in
            switch action {
            case .digits(let typed): stores.pair(digits: typed)
            // A code cannot reach any of these — nothing on the keypad
            // authenticates, and an account is not what a typed code needs.
            case .confirm, .abandon, .signIn: break
            case .subscribe: actions(.subscribe)
            case .cancel: router.pop()
            }
        }
    }

    private var decision: some View {
        PairConfirmation(model: stores.pairing) { action in
            switch action {
            case .confirm(let peer): stores.confirmPairing(peer)
            // Turning the machine away is a message to it, not just a way off
            // the screen: told, it can release the attempt now rather than
            // holding it open until it expires. What is left behind is an
            // empty keypad, because the next thing somebody does after
            // refusing a machine is try the code for the right one.
            case .abandon(let peer): stores.abandonPairing(peer)
            case .cancel: router.pop()
            case .digits, .signIn, .subscribe: break
            }
        }
    }
}

/// Where a pairing link lands.
///
/// Arriving here authenticates the invitation and nothing else. The machine is
/// asked to prove it issued the link and to say who it is; the trust is written
/// only when the person presses, and leaving instead tells the machine so.
///
/// The authentication is asked for whenever this page has something to ask
/// with, which is what carries a link across a cold start and a sign-in: a
/// launch that opened on this page with no account yet asks again the moment
/// an account arrives, rather than stranding the person on a screen that can
/// never say who it is confirming.
private struct PairConfirmationPage: View {
    let invitation: PairingInvitation
    let router: Router
    let stores: StoreBundle
    let actions: @MainActor (ShellAction) -> Void
    @State private var asked = LinkAsked()

    var body: some View {
        page
            .toolbar(.hidden, for: .navigationBar)
            .onAppear { ask() }
            .onChange(of: stores.account) { _, _ in ask() }
            .onChange(of: stores.fleet.relay) { _, _ in ask() }
            // A link that opened the app arrives before its runtime has
            // started; it is asked again once the runtime first answers.
            .onChange(of: stores.applied > 0) { _, _ in ask() }
    }

    /// A machine only the relay has seen, reached by a phone with no relay, is
    /// one missing piece rather than a failed invitation. The invitation is
    /// kept — this page is still the route, and signing in makes it ask — so
    /// nothing is spent by saying so.
    @ViewBuilder
    private var page: some View {
        if unreachable {
            PairNeedsAnAccount { action in
                switch action {
                case .signIn: actions(.signIn)
                case .cancel: router.pop()
                case .confirm, .abandon, .digits, .subscribe: break
                }
            }
        } else {
            confirmation
        }
    }

    /// Whether this invitation has nothing this phone can act on: no address
    /// to dial and no account to reach a relay with.
    private var unreachable: Bool {
        invitation.needsAnAccount && stores.hosts.account?.binding != .signedIn
    }

    private var confirmation: some View {
        PairConfirmation(model: stores.pairing) { action in
            switch action {
            case .confirm(let peer): stores.confirmPairing(peer)
            // Turning the machine away is a message to it, not just a way off
            // the screen: told, it can release the attempt now instead of
            // holding it open until it expires.
            case .abandon(let peer):
                stores.abandonPairing(peer)
                router.pop()
            case .cancel: router.pop()
            case .digits, .signIn: break
            case .subscribe: actions(.subscribe)
            }
        }
    }

    /// Puts the invitation to the machine, once there is anything to put it
    /// with.
    ///
    /// A link is authenticated over the relay it names, so a phone with no
    /// account and a phone whose connection has not finished opening are the
    /// same thing here: there is nobody to ask. Asking anyway comes back as a
    /// refusal within the second, and "that invitation did not work" is a
    /// verdict about the machine — saying it about a connection that was still
    /// being made would send somebody back to a machine that is fine to ask it
    /// for another code.
    private func ask() {
        // An invitation carrying addresses is dialled on the network this
        // phone is already on: there is no relay in the way of it, so waiting
        // for one would strand a machine standing on the same desk behind an
        // account nobody needs.
        guard !invitation.needsAnAccount || stores.fleet.relay == .connected,
              !unreachable,
              asked.shouldAsk(stores.account)
        else { return }
        stores.pairing.open()
        if stores.pair(link: invitation.link, relayOnly: invitation.needsAnAccount) {
            asked.asked(stores.account)
        }
    }
}

/// Which account a pairing invitation has been put to.
///
/// A link can arrive before this phone has an account at all: a cold start
/// hands the URL over before the first frame, and the person may sign in
/// afterwards. That does not spend the invitation — it was never asked about —
/// so it is put to whichever account is on show, once each. Signing in asks;
/// an ordinary redraw does not; and switching to a second account asks again,
/// because trust is per account and the first account's answer is not the
/// second's.
struct LinkAsked {
    private var account: AccountId?

    func shouldAsk(_ current: AccountId) -> Bool { account != current }

    mutating func asked(_ current: AccountId) { account = current }
}

/// Settings, in the sheet raised from the account disc.
///
/// The screen decides nothing: switching account, signing in and out, buying
/// and deleting all leave here, because each of them either changes what the
/// whole app is pointed at or reaches something outside it.
private struct YouTabRoot: View {
    let router: Router
    let accounts: AccountRegistry
    let stores: StoreBundle
    let deletion: DeletionStore
    let removal: RemovalStore
    let appearance: Appearance?
    let shareUsage: Bool
    /// Freezes the screen behind this page and opens the report on it.
    let report: @MainActor () -> Void
    let actions: @MainActor (ShellAction) -> Void

    var body: some View {
        DeleteAccountOverlay(
            entry: accounts.selectedAccount, model: deletion,
            actions: { asked in
                switch asked {
                case .cancel: actions(.cancelDeletion)
                case .confirm: actions(.confirmDeletion)
                // The only place a renewal can be stopped is where it was
                // bought, which for an App Store subscription is a page of the
                // system's rather than one this app or amux.sh owns.
                case .manageBilling(let url): leave(for: url)
                }
            }
        ) {
            RemoveAccountOverlay(
                accounts: accounts, model: removal,
                actions: { asked in
                    switch asked {
                    case .cancel: actions(.cancelRemoval)
                    case .confirm: actions(.confirmRemoval)
                    }
                }
            ) {
                you
            }
        }
        .navigationTitle(Tab.you.title)
        .toolbarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button(role: .close) { router.select(router.beneath) }
                    .accessibilityIdentifier("you.close")
            }
        }
    }

    private var you: some View {
        YouScreen(
            accounts: accounts, appearance: appearance, shareUsage: shareUsage,
            // This phone's own key, read off the machine store the way the
            // devices page reads it: absent until a connection has said what
            // this device's identity is, rather than guessed at.
            identity: stores.hosts.roster.map { Fingerprint.short($0.identity.fingerprint) }
        ) { action in
            switch action {
            case .select(let id): actions(.selectAccount(id))
            case .add: actions(.addAccount)
            case .signIn(let id): actions(.signInAgain(id))
            case .signOut(let id): actions(.signOutAccount(id))
            case .remove(let id): actions(.removeAccount(id))
            case .subscription: actions(.subscribe)
            case .appearance(let wanted): actions(.wear(wanted))
            case .shareUsage(let on): actions(.shareUsage(on))
            case .delete(let id): actions(.deleteAccount(id))
            // This phone's key and the machines that trust it are one page,
            // and it is the machines tab: an identity is only interesting
            // beside what it is trusted by.
            case .identity: router.select(.hosts)
            // Reaching a person happens on the web, where the people are.
            // There is no form in here to fill in: a message written into this
            // app would have to be carried by the same account service the
            // person may be writing about because they cannot reach it.
            case .support: leave(for: CloudEndpoint.production.support)
            // Writing a report freezes the frame that was on show before any
            // of the report's own UI appears — otherwise the picture would be
            // of the report rather than of what was wrong.
            case .report: report()
            case .exportDump: actions(.exportDump)
            }
        }
    }
}
