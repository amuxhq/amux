import AmuxCore
import AmuxDesign
import AmuxFeatures
import AmuxShell
import SwiftUI

/// Whole screens in named states, drawn from invented values with no
/// runtime behind them: the agent list as a catalogue example, and the whole
/// app on the same values for a preview you can tap through.
///
/// Debug-only for the same reason as the rest of the catalogue: none of this
/// content belongs in a build somebody installs.
///
/// Xcode's previews of these are in the `…Previews.swift` files beside this
/// one, a file per area: a preview refreshes by compiling the file that holds
/// it again, so small files refresh sooner, and the canvas lists only the
/// stories of the area open.
extension ComponentCatalog {
    // MARK: - Fleet

    static let fleet: [ComponentExample] = [
        fleet("busy") { StoryFleet.busy(away: true) },
        fleet("notifications-offer", offerNotifications: true) { StoryFleet.busy(nearby: false) },
        fleet("stopped-open") { StoryFleet.busy(stoppedOpen: true) },
        fleet("no-agents") { StoryFleet.noAgents() },
        fleet("first-run") { StoryFleet.firstRun() },
        fleet("first-run-two-found") { StoryFleet.firstRun(found: [StoryFleet.laptop, StoryFleet.studioFound]) },
        fleet("first-run-looking") { StoryFleet.firstRun(found: []) },
        fleet("first-run-refused") { StoryFleet.firstRun(found: [], network: .denied) },
        fleet("first-run-signed-out") { StoryFleet.firstRun(found: [], accounts: 0) },
        fleet("first-run-signed-out-found") { StoryFleet.firstRun(accounts: 0) },
        fleet("large-text", size: .accessibility3) { StoryFleet.busy() },
        fleet("two-accounts") { StoryFleet.busy(accounts: 2) },
        fleet("signed-out") { StoryFleet.busy(accounts: 0) },
        fleet("picture") { StoryFleet.busy(accounts: 2, picture: true) },
        fleet("filter") {
            // The runtime filters the rows; filtered, it lists the two whose
            // step runs the tests.
            var stores = StoryFleet.busy()
            stores.fleet.filter = FleetFilter(words: "test", only: nil)
            let matching: Set = ["auth-refactor", "flaky-e2e"]
            let sections = stores.view.sections.compactMap { section -> FleetSection? in
                var section = section
                section.rows = section.rows.filter { matching.contains($0.card.name) }
                section.families = UInt32(section.rows.count)
                return section.rows.isEmpty ? nil : section
            }
            stores.view = FleetView(sections: sections, needYou: stores.view.needYou, working: stores.view.working)
            stores.fleet.show(stores.view, hosts: stores.hostViews)
            return stores
        },
        fleet("no-matches") {
            // The runtime filters the rows; filtered to nothing, it lists none.
            var stores = StoryFleet.busy()
            stores.fleet.filter = FleetFilter(words: "kubernetes", only: nil)
            stores.view = FleetView(sections: [], needYou: stores.view.needYou, working: stores.view.working)
            stores.fleet.show(stores.view, hosts: stores.hostViews)
            return stores
        },
        ComponentExample(
            id: "fleet.chat-open", family: .fleet, canvas: CGSize(width: 390, height: 844),
            settlesLate: true
        ) { StoryApp(chatOpen: true) },
        ComponentExample(
            id: "chat.exited", family: .chat, canvas: CGSize(width: 390, height: 844),
            settlesLate: true
        ) { StoryApp(chatOpen: true, chatPhase: .exited(cause: .failed("while the daemon was away"))) },
        ComponentExample(
            id: "settings.signed-in", family: .settings, canvas: CGSize(width: 390, height: 844),
            settlesLate: true
        ) { StoryApp(settingsOpen: true).settings },
    ]

    private static func fleet(
        _ id: String, size: DynamicTypeSize = .large, offerNotifications: Bool = false,
        stores: @escaping @MainActor () -> StoryFleet.Stores
    ) -> ComponentExample {
        ComponentExample(
            id: "fleet.\(id)", family: .fleet, canvas: CGSize(width: 390, height: 844),
            // The system bar lays its title out a frame after the list.
            dynamicTypeSize: size, settlesLate: true
        ) {
            let stores = stores()
            // In a navigation stack, as the app shows it, so the title and
            // the buttons beside it are the system bar's.
            NavigationStack {
                AgentsHome(
                    model: stores.fleet, accounts: stores.accounts, hosts: stores.hosts,
                    offerNotifications: offerNotifications
                ) { _ in }
            }
            .onAppear { Design.app.dressNavigationBars() }
        }
    }
}

/// An invented fleet: four machines, agents in every state, a family and a
/// few that stopped.
@MainActor
enum StoryFleet {
    struct Stores {
        let fleet: FleetStore
        let hosts: HostsStore
        let accounts: AccountRegistry
        /// What the stores were shown, for the whole app to show its own.
        var view = FleetView(sections: [], needYou: 0, working: 0)
        var hostViews: [HostView] = []
    }

    static let now = CatalogFixtures.now

    static let desk = host(1, "desk", platform: .macos)
    static let studio = host(2, "studio", platform: .macos)
    static let pi = host(3, "garage-pi", platform: .linux)
    static let laptop = host(4, "laptop", platform: .macos, trusted: false, candidate: true)
    static let studioFound = host(5, "studio", platform: .macos, trusted: false, candidate: true)

    /// - Parameter nearby: whether a machine this phone has not paired is
    ///   on its network, offering itself above the list.
    static func busy(
        stoppedOpen: Bool = false, away: Bool = false, accounts: Int = 1, picture: Bool = false,
        nearby: Bool = true
    ) -> Stores {
        let studioReach: Reach = away ? .away : .online(.direct)
        let needs = [
            row(1, "release-notes", on: desk, .needsYou, minutes: 2,
                .ask(AskSummary(subject: .question(question: "Which changes should the release notes keep?", count: 1), count: 1))),
            row(2, "auth-refactor", on: studio, .needsYou, minutes: 6, reach: studioReach, branch: "refactor-auth",
                .ask(AskSummary(subject: .command(command: "cargo test -p amux-auth"), count: 1))),
        ]
        let live = [
            row(3, "flaky-e2e", on: desk, .working, minutes: 1, kind: .claudePty,
                .step(ActivityLine(activity: activity(.running(key: "t1")), step: "cargo test -p e2e reconnect"))),
            row(4, "migration", on: desk, .working, minutes: 3, children: 2,
                .step(ActivityLine(activity: activity(.subagents(count: 2)), step: nil))),
            row(5, "docs-pass", on: studio, .working, minutes: 4, kind: .codex, reach: studioReach,
                .step(ActivityLine(activity: activity(.thinking), step: nil))),
            row(6, "licensing", on: desk, .idle, minutes: 18,
                .lastSaid("Drafted the licensing page; it's ready for you to read.")),
            row(7, "sensor-logger", on: pi, .idle, minutes: 52, kind: .codex,
                .lastSaid("Running under systemd now.")),
        ]
        let stopped = [
            row(8, "spike-quic", on: studio, .exited, minutes: 180, reach: studioReach, exit: .finished, .exited(.finished)),
            row(9, "bisect-hang", on: desk, .exited, minutes: 300, exit: .failed("Exited with status 1"),
                .exited(.failed("Exited with status 1"))),
        ]
        var sections = [
            FleetSection(kind: .needsYou, families: 2, foldedByDefault: false, rows: needs),
            FleetSection(kind: .live, families: 5, foldedByDefault: false, rows: live),
            FleetSection(kind: .exited, families: 2, foldedByDefault: true, rows: stopped),
        ]
        if away { sections[0].rows[1].secondLine = .hostAway }
        let view = FleetView(sections: sections, needYou: 2, working: 3)
        var studio = studio
        if away { studio.reach = .away }
        var stores = Self.stores(
            hosts: nearby ? [desk, studio, pi, laptop] : [desk, studio, pi], accounts: accounts,
            picture: picture)
        stores.view = view
        stores.fleet.show(view, hosts: stores.hostViews)
        if stoppedOpen { stores.fleet.toggle(.exited) }
        return stores
    }

    static func noAgents() -> Stores {
        let stores = Self.stores(hosts: [desk])
        stores.fleet.show(FleetView(sections: [], needYou: 0, working: 0), hosts: [desk])
        return stores
    }

    /// Nothing paired, with what this phone has found on its network.
    static func firstRun(
        found: [HostView] = [laptop], network: LocalNetworkPermission = .granted, accounts: Int = 1
    ) -> Stores {
        let stores = Self.stores(hosts: found, accounts: accounts)
        stores.hosts.sawLocalNetwork(network)
        stores.fleet.show(FleetView(sections: [], needYou: 0, working: 0), hosts: found)
        return stores
    }

    // MARK: - Building values

    static func stores(hosts: [HostView], accounts count: Int = 1, picture: Bool = false) -> Stores {
        let fleet = FleetStore(now: now)
        let hostsStore = CatalogFixtures.hostsStore(hosts)
        let accounts = AccountRegistry()
        let profiles = [
            ProfileView(
                id: "p1", label: "", subject: "ada",
                account: AccountView(binding: .signedIn, email: "ada@example.com", name: "Ada", picture: picture ? avatar : "", relay: .connected, pro: true)),
            ProfileView(
                id: "p2", label: "", subject: "ada-work",
                account: AccountView(binding: .signedIn, email: "ada@lovelace.dev", name: "Lovelace Labs", picture: "", relay: .connected, pro: true)),
        ]
        accounts.show(Array(profiles.prefix(count)))
        return Stores(fleet: fleet, hosts: hostsStore, accounts: accounts, hostViews: hosts)
    }

    /// A stand-in profile picture: a head and shoulders on a warm gradient,
    /// carried in the URL so it loads with no network.
    static let avatar = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAGAAAABgCAIAAABt+uBvAAAFVklEQVR42u3a91NaWRTA8fMnbrKptti7xhqjKGLvBWuqUbErFhBUEFHE3iu29N3N1uz+CXsu7OwvGW/eA+6D97wz37/gM8zw7jkH/ukI+bs95Gt76Ne20L/awv7EWsP/aI3Afm959BumjfxVG/lFG/WlOfoXUszPTTE/NcVinxvjPjXGf2qI/9iQgH2oT3xPSnpXl/yWlHJVi6Ve1qRe1KRd1KSfV6efVT/GXFUZ2GlV5kllFnZckX1Eyjksx3IPyp/sl2F5e2V5u6VPsZ2S/O2SAmxLo8I2NYUbxVjRerF6Xa1eUxevFmGalSKNs7DEWVi6jKnKHKqyJVX5UkH5YkGFPb8SW8ivsj3FquexvBprXq0Fe1I3h+XWz+bWz+Q2zOQ0mnMaTTlNpuym6exm4DoUnelsLXAdio4xC4G4zvU6hqwW4DoUHUNmC3Adis5UZitwHYrOJAJxHYrOZEYbcB2KzkRGO3Adig4B4joUnfHHHcB1KDp6BOI6FB19eidwHYrOGAHiOtfrjKY/A65D0RlNew5ch6IzgkBch6IzkvYCuA5FZzjVDcR1rtMZSn0JXIeiQ4C4DkVnMOUVBFznw+zs+xlsDntnxiye3potV2brlckaQJ2BlNcQQB2k+a4OdmmaxwKiM5DsBpJex0MjXOfSZLuYJkms05/cBTLScbcgpQ4BklLn4+yMjzrnnox2aXT6kt6AjH47/+ucIZAkOjoEkqMO5jLaJdDRJXWDTHVcxkWXYZG1Tm9iDwThP7pAnVPDImudHgSSr86pYQljqtOT2AvB8K3stc4JAWKo053QC7LWOTE4TqYc7HS6E3TA+hXKWud4apmdzpt4AsT2jc5a53hymZ1OV3wfsJ5gsNbB2Ol0xfcD6/kOa50jBGKm8zrODcR0+sVa52jSyU7nVdwAsJ4NstbxADHS+Q+I6eSUtQ7GTudl7CCwniuz1jmcWGGnQ4BYT91Z6xwgEDOdF7FDwHonwVoHY6fzPGYYJNjYyFeHAEmwz2KngzHVeRY9AtJs+2Sq04lA0uxCWejsT6yy1umMHgXJNsVy1OmIGgUp9+iy0+mIGgMprwz8pbM/viqNTnuUHqS/wZDLbwd12iL1EJALFbnotEWOQwDvd4LtH/1bnVYPUGCvmwL+rUzRaXk0AfK9/ZJAhwBxHYqONmISuA5FRxsxBVyHotPsAeI61+k0hRuA61B0CFAAdSgvCXdre6R1bFe/HhCdxnAjSK/z3XfWtzq7+g1sx52UOg1hRpBSR8grlK6zPbbpSRqdhrBpkEZH4BtdoM7W6JYn1jr1YSYI+EvCa51N0vbmyDY7nfpQE8hdZ4O0w0inLtQMjHRETb981MHWR3Yxv+vUhphBMTrrwyT/6tSGzICSdNaG91aH9/yoU+MGUpQOaWjfXzrVD2dBeTor7vyiU/1wDhSp4+7Ad50qBFKqjhMbPPBRp+qBBeT1vSNKxzl46KNOJQFSrs4y6cgXnYoHVlC2DuYYOPJap+K+FYLzFepHHcfAsdc65ffnQfE6S1j/sXc6ZfdsECTzHaY6mHc6BOgm6Cz2n2Be6JTeW4BATU4l1sG80Cm9uwA3RMfef2rvOxWrU3LXDt7tJOSog4nV0SDQzdFZILlE6WjuLALrfVZQ6WCidIrvLIEXu1D56th0LlE6agS6UTo23RkmXEf9owPE7tHlrmPTnQvXKUIgsVcGcteZ150L1ym8vQxibzDkrmNFIME6bqAbpmPtxS4E6qhuO0HU/Y4ydCxuICE6qlsrIOq6SRk6mECdAgIk5vZLGTqW3kuBOvm3VkHUZZwydCw9lwJ18n9Y/RebN+p3f+R8FAAAAABJRU5ErkJggg=="

    static func host(
        _ index: UInt8, _ name: String, platform: Platform, trusted: Bool = true, candidate: Bool = false
    ) -> HostView {
        HostView(
            hostId: Array(repeating: index, count: 16), name: name, local: false, trusted: trusted,
            candidate: candidate, reach: .online(.direct), addrs: [], current: true,
            providers: [ProviderSignIn(provider: .claude, signedIn: true), ProviderSignIn(provider: .codex, signedIn: true)],
            lastDialError: nil, platform: platform, signedIn: nil, version: nil)
    }

    static func key(_ index: UInt8, on host: HostView) -> AgentKey {
        AgentKey(host: host.hostId, agent: Array(repeating: index, count: 16))
    }

    private static func activity(_ kind: ActivityKind) -> Activity {
        Activity(kind: kind, sinceMs: ms(minutesAgo: 1), elapsedMs: 60_000, turnElapsedMs: 60_000)
    }

    private static func ms(minutesAgo: Double) -> Int64 {
        Int64(now.addingTimeInterval(-60 * minutesAgo).timeIntervalSince1970 * 1_000)
    }

    private static func row(
        _ index: UInt8, _ name: String, on host: HostView, _ attention: Attention, minutes: Double,
        kind: Kind = .claudeSdk, reach: Reach = .online(.direct), children: UInt32 = 0,
        branch: String? = nil, exit: ExitCause? = nil, _ second: SecondLine
    ) -> FleetRow {
        FleetRow(
            card: FleetCard(
                agent: key(index, on: host), name: name, kind: kind, attention: attention,
                cwd: "/Users/ada/src/\(name)", phaseSinceMs: ms(minutesAgo: minutes),
                host: host.name, hostReach: reach, children: children, familyAttention: attention,
                members: children + 1, descendants: children, backgroundJobs: 0, stopHeld: false,
                branch: branch, exitCause: exit),
            depth: 0, expanded: false, secondLine: second, loud: nil)
    }
}

// MARK: - The whole app

/// The whole app on the invented fleet, with every agent's chat answering
/// from the catalogue's conversation. In the canvas's live mode it taps
/// through like the app: open a chat, go back, switch tabs.
@MainActor
struct StoryApp: View {
    @State private var router: Router
    @State private var app: (accounts: AccountRegistry, stores: StoreBundle)
    private let notifications: NotificationsSetting?

    /// - Parameters:
    ///   - chatOpen: opens the first agent's chat, as a person who had just
    ///     tapped it would see it.
    ///   - settingsOpen: raises the settings sheet over the list.
    ///   - reviewOpen: opens the first agent's chat and its review over it.
    ///   - newAgentOpen: raises New Agent over the list.
    ///   - notifications: what Settings says of notifications; none leaves
    ///     the row out, as a build that offers none does.
    init(
        chatOpen: Bool = false,
        settingsOpen: Bool = false,
        reviewOpen: Bool = false,
        newAgentOpen: Bool = false,
        notifications: NotificationsSetting? = nil,
        chatPhase: PhaseView = .idle,
        _ fleet: @escaping @MainActor () -> StoryFleet.Stores = { StoryFleet.busy() }
    ) {
        let invented = fleet()
        let router = Router()
        if chatOpen || reviewOpen, let first = invented.view.sections.first?.rows.first {
            router.open(.conversation(first.card.agent))
            if reviewOpen { router.open(.changes(first.card.agent)) }
        }
        if settingsOpen { router.select(.you) }
        if newAgentOpen { router.composing = true }
        _router = State(initialValue: router)
        let bundle = StoreBundle(
            account: AccountId("ada"), clock: { StoryFleet.now },
            opener: { _ in
                let chat = ScriptedChat(
                    rows: CatalogFixtures.conversation, frame: ScriptedChat.frame(phase: chatPhase),
                    strip: ScriptedChat.strip(model: "opus 4.6", git: CatalogFixtures.git))
                chat.working = CatalogFixtures.review
                return chat
            })
        bundle.fleet.show(invented.view, hosts: invented.hostViews)
        bundle.hosts.show(invented.hostViews)
        if let roster = invented.hosts.roster { bundle.hosts.show(roster) }
        _app = State(initialValue: (invented.accounts, bundle))
        self.notifications = notifications
    }

    var body: some View {
        shell.environment(\.design, Design.app)
    }

    /// The settings sheet alone, as it is drawn over the app: a capture
    /// leaves out what the app presents.
    var settings: some View {
        shell.settings.environment(\.design, Design.app)
    }

    private var shell: Shell {
        Shell(
            router: router, accounts: app.accounts, stores: app.stores, signIn: SignInStore(),
            paywall: PaywallStore(), deletion: DeletionStore(), notifications: notifications
        ) { _ in }
    }
}

extension ScriptedChat: OpenChat {
    public var id: UInt64 { 0 }
    public func close() {}
}

/// One catalogue example filling the preview's device, the way the app
/// draws a screen: under the device's own safe areas, not on the fixed
/// canvas a snapshot uses.
@MainActor
struct StoryScreen: View {
    let id: String

    init(_ id: String) { self.id = id }

    var body: some View {
        if let example = ComponentCatalog.example(id: id) {
            ZStack {
                Ground()
                example.build()
                    .padding([.chat, .fleet, .hosts, .pairing, .settings, .report].contains(example.family) ? 0 : 18)
            }
            .environment(\.design, Design.app)
            .environment(\.dynamicTypeSize, example.dynamicTypeSize)
        } else {
            Text("No story named \(id)")
        }
    }
}

/// Every example of one family, one under the other.
@MainActor
struct StoryGallery: View {
    @Environment(\.colorScheme) private var scheme
    let family: ComponentExample.Family

    init(_ family: ComponentExample.Family) { self.family = family }

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 24) {
                ForEach(ComponentCatalog.examples.filter { $0.family == family }) { example in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(example.id).font(.caption.monospaced()).foregroundStyle(.secondary)
                        ComponentExampleView(
                            example: example, appearance: scheme, reducesTransparency: false)
                    }
                }
            }
            .padding(.vertical, 20)
        }
    }
}

// MARK: - Foundation

/// The visual foundation on one page: every type role at its size, and the
/// spacing scale every gap is taken from.
@MainActor
struct FoundationSheet: View {
    @Environment(\.design) private var design

    private let roles: [(Design.Role, String, String)] = [
        (.screenTitle, "Screen title", "A title inside a page"),
        (.identifier, "Name", "release-notes"),
        (.body, "Body", "The pairing errors all come from one table."),
        (.detail, "Preview", "Which changes should the release notes keep?"),
        (.sectionTitle, "Section", "Exited"),
        (.caption, "Meta", "desk · 2m"),
        (.mono, "Code", "cargo test -p amux-auth"),
        (.monoSmall, "Code, small", "crates/wire/src/codes.rs"),
    ]

    private let steps: [(String, CGFloat)] = [
        ("xxs", Space.xxs), ("xs", Space.xs), ("s", Space.s), ("m", Space.m),
        ("l · margin", Space.l), ("xl", Space.xl), ("xxl", Space.xxl),
    ]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Space.xl) {
                Text("Type")
                    .designFont(.sectionTitle, design).foregroundStyle(design.inkMuted.color)
                ForEach(roles.indices, id: \.self) { index in
                    let (role, name, sample) = roles[index]
                    let spec = design.spec(role)
                    VStack(alignment: .leading, spacing: Space.xs) {
                        Text(sample).designFont(role, design).foregroundStyle(design.ink.color)
                        Text("\(name) · \(spec.family) \(Int(spec.size)) pt")
                            .font(.caption2.monospaced()).foregroundStyle(design.inkFaint.color)
                    }
                }
                Rectangle().fill(design.hairline.color).frame(height: 0.5)
                Text("Space")
                    .designFont(.sectionTitle, design).foregroundStyle(design.inkMuted.color)
                ForEach(steps.indices, id: \.self) { index in
                    let (name, value) = steps[index]
                    HStack(spacing: Space.m) {
                        Rectangle().fill(design.accent.color).frame(width: value, height: 12)
                        Text("\(name) · \(Int(value)) pt")
                            .font(.caption2.monospaced()).foregroundStyle(design.inkMuted.color)
                    }
                }
            }
            .padding(.horizontal, Space.margin)
            .padding(.vertical, Space.xl)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .background(Ground())
        .environment(\.design, Design.app)
    }
}

/// The lines a screen's content is meant to sit on: the margins, and where
/// a list row's text starts after its mark.
@MainActor
struct AlignmentGuides: View {
    var body: some View {
        GeometryReader { proxy in
            let lines: [CGFloat] = [Space.margin, Space.margin + Space.m + Space.m, proxy.size.width - Space.margin]
            ForEach(lines, id: \.self) { x in
                Rectangle().fill(Color.red.opacity(0.5)).frame(width: 0.5).position(x: x, y: proxy.size.height / 2)
                    .frame(height: proxy.size.height)
            }
        }
        .ignoresSafeArea()
        .allowsHitTesting(false)
    }
}

/// Every state's mark, once per way a working agent's mark could move.
@MainActor
struct WorkingMarks: View {
    @Environment(\.design) private var design

    private static let states: [String] = ["needs-you", "working", "idle", "away", "stopped"]

    var body: some View {
        VStack(alignment: .leading, spacing: Space.xl) {
            ForEach(WorkingMark.allCases, id: \.self) { style in
                VStack(alignment: .leading, spacing: Space.s) {
                    Text(style.rawValue.capitalized)
                        .designFont(.identifier, design).foregroundStyle(design.ink.color)
                    HStack(spacing: Space.xl) {
                        ForEach(Self.states, id: \.self) { state in
                            VStack(spacing: Space.s) {
                                StateMarkSample(state).frame(width: 14, height: 14)
                                Text(state).designFont(.caption, design).foregroundStyle(design.inkFaint.color)
                            }
                        }
                    }
                    .environment(\.workingMark, style)
                }
            }
        }
        .padding(Space.margin)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Ground())
        .environment(\.design, Design.app)
    }
}
