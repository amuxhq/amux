import AmuxValues
import AmuxWords
import SwiftUI

/// The fleet as the sidebar lists it.
public struct FleetContent: Sendable {
    /// The runtime's sections, in its order, settled agents among them.
    public var sections: [FleetSection]
    /// Whether the runtime has said what it lists: until then an empty list
    /// is not yet read.
    public var loaded: Bool
    /// The clock ages are read against.
    public var now: Date

    public init(sections: [FleetSection] = [], loaded: Bool = true, now: Date = Date()) {
        self.sections = sections
        self.loaded = loaded
        self.now = now
    }

    /// Every section but the settled agents'.
    var live: [FleetSection] { sections.filter { $0.kind != .closed } }
    var settled: FleetSection? { sections.first { $0.kind == .closed } }
    var rows: [FleetRow] { sections.flatMap(\.rows) }

    func row(_ agent: AgentKey) -> FleetRow? { rows.first { $0.card.agent == agent } }
}

/// What the main window shows around the open surface.
public struct MainContent {
    public var profiles: [ProfileView]
    /// The profile on screen.
    public var profile: String?
    public var fleet: FleetContent
    /// The machines the profile trusts, this Mac apart.
    public var hosts: [HostView]
    /// This Mac, as the profile knows it.
    public var thisMac: HostView?
    /// How many setting-up steps the window offers to finish.
    public var toFinish: UInt32
    public var problem: String?
    public var note: String?
    public var signingIn: Bool
    public var updateReady: Bool

    public init(
        profiles: [ProfileView], profile: String? = nil, fleet: FleetContent = FleetContent(),
        hosts: [HostView] = [], thisMac: HostView? = nil, toFinish: UInt32 = 0, problem: String? = nil,
        note: String? = nil, signingIn: Bool = false, updateReady: Bool = false
    ) {
        self.profiles = profiles
        self.profile = profile ?? profiles.first?.id
        self.fleet = fleet
        self.hosts = hosts
        self.thisMac = thisMac
        self.toFinish = toFinish
        self.problem = problem
        self.note = note
        self.signingIn = signingIn
        self.updateReady = updateReady
    }

    var current: ProfileView? { profiles.first { $0.id == profile } }
}

/// What the main window can ask for.
public struct MainActions {
    public var openProfile: @MainActor (String) -> Void
    public var signIn: @MainActor () -> Void
    public var signOut: @MainActor () -> Void
    public var finishSettingUp: @MainActor () -> Void
    public var dismissFinishing: @MainActor () -> Void
    public var restartToUpdate: @MainActor () -> Void
    public var newAgent: @MainActor () -> Void
    /// Narrows the fleet to the rows that say these words.
    public var search: @MainActor (String) -> Void
    public var toggleFamily: @MainActor (AgentKey) -> Void
    /// Pins, settles, renames or deletes an agent.
    public var act: @MainActor (AgentAct, AgentKey) -> Void
    /// Takes the line saying something went wrong away.
    public var dismissProblem: @MainActor () -> Void
    /// The agent's own interface in Terminal.
    public var openTerminal: @MainActor (AgentKey) -> Void

    public init(
        openProfile: @escaping @MainActor (String) -> Void = { _ in },
        signIn: @escaping @MainActor () -> Void = {},
        signOut: @escaping @MainActor () -> Void = {},
        finishSettingUp: @escaping @MainActor () -> Void = {},
        dismissFinishing: @escaping @MainActor () -> Void = {},
        restartToUpdate: @escaping @MainActor () -> Void = {},
        newAgent: @escaping @MainActor () -> Void = {},
        search: @escaping @MainActor (String) -> Void = { _ in },
        toggleFamily: @escaping @MainActor (AgentKey) -> Void = { _ in },
        act: @escaping @MainActor (AgentAct, AgentKey) -> Void = { _, _ in },
        dismissProblem: @escaping @MainActor () -> Void = {},
        openTerminal: @escaping @MainActor (AgentKey) -> Void = { _ in }
    ) {
        self.openProfile = openProfile
        self.signIn = signIn
        self.signOut = signOut
        self.finishSettingUp = finishSettingUp
        self.dismissFinishing = dismissFinishing
        self.restartToUpdate = restartToUpdate
        self.newAgent = newAgent
        self.search = search
        self.toggleFamily = toggleFamily
        self.act = act
        self.dismissProblem = dismissProblem
        self.openTerminal = openTerminal
    }
}

/// The main window: the rail, the open surface's sidebar, its centre and,
/// for an agent, the pane on the right.
///
/// The sidebar is flush and full height, and the window's buttons sit on
/// it, as in the developer tools this window sits beside. The centre and
/// the inspector are the app's to fill: an agent's chat and its changes.
public struct MainScreen<Detail: View, Inspector: View>: View {
    let content: MainContent
    @Binding var place: Place
    let actions: MainActions
    let detail: Detail
    let inspector: Inspector

    @State private var sidebarWidth = Metrics.sidebar
    @State private var inspectorWidth = Metrics.inspector
    /// What the agents are searched for. Held here, not in the sidebar,
    /// because the runtime keeps filtering the list while the sidebar is
    /// hidden or another surface is showing.
    @State private var search = ""

    public init(
        content: MainContent, place: Binding<Place>, actions: MainActions = MainActions(),
        @ViewBuilder detail: () -> Detail, @ViewBuilder inspector: () -> Inspector
    ) {
        self.content = content
        _place = place
        self.actions = actions
        self.detail = detail()
        self.inspector = inspector()
    }

    public var body: some View {
        GeometryReader { window in
            let bar = max(window.safeAreaInsets.top, 38)
            let layout = ColumnLayout(
                width: window.size.width, sidebar: place.sidebar ? Metrics.rail + sidebarWidth + 1 : 0,
                inspector: inspecting ? inspectorWidth + 1 : 0)
            HStack(spacing: 0) {
                if layout.showsSidebar {
                    sidebar(bar: bar)
                        .frame(width: layout.sidebar - 1)
                        .background(Palette.sidebar)
                    ColumnEdge(width: $sidebarWidth, range: Metrics.sidebarRange)
                }
                centre(bar: bar, sidebarShown: layout.showsSidebar)
                    .frame(minWidth: ColumnLayout.centre, maxWidth: .infinity)
                    .background(Palette.content)
                if inspecting {
                    ColumnEdge(width: $inspectorWidth, range: Metrics.inspectorRange, trailing: true)
                    inspector
                        .frame(width: layout.inspector - 1)
                        .frame(maxHeight: .infinity)
                        .clipped()
                        .background(Palette.content)
                }
            }
            .environment(\.barHeight, bar)
            .ignoresSafeArea(.container, edges: .top)
        }
        .windowChrome()
        // A search narrows the agents; another surface starts it afresh, so
        // nothing there is listed through a filter no field shows.
        // Another profile's fleet starts unfiltered, and so does the field.
        .onChange(of: content.profile) { search = "" }
        .onChange(of: place.surface) { _, surface in
            if surface != .agents, !search.isEmpty {
                search = ""
                actions.search("")
            }
        }
        .animation(.snappy(duration: 0.2), value: place.sidebar)
        .animation(.snappy(duration: 0.2), value: place.changes)
    }

    private var inspecting: Bool {
        place.changes && place.surface == .agents && place.agent != nil
    }

    private func exists(_ agent: AgentKey) -> Bool { content.fleet.row(agent) != nil }

    // MARK: - The sidebar

    private func sidebar(bar: CGFloat) -> some View {
        VStack(spacing: 0) {
            TopBar(height: bar, leading: Metrics.windowButtons) {
                BarButton(symbol: "panel-left", help: "Hide Sidebar (⌃⌘S)") { place.sidebar = false }
                Spacer()
                history
            }
            HStack(spacing: 0) {
                Rail(content: content, surface: $place.surface, actions: actions)
                    .frame(width: Metrics.rail)
                surfaceSidebar
                    .frame(maxWidth: .infinity)
                    .clipped()
            }
        }
    }

    @ViewBuilder private var surfaceSidebar: some View {
        switch place.surface {
        case .agents:
            FleetSidebar(content: content, place: $place, search: $search, actions: actions)
        case .hosts:
            HostsSidebar(content: content, place: $place)
        }
    }

    // MARK: - The centre

    private func centre(bar: CGFloat, sidebarShown: Bool) -> some View {
        VStack(spacing: 0) {
            TopBar(height: bar, leading: sidebarShown ? 0 : Metrics.windowButtons) {
                if !sidebarShown {
                    BarButton(symbol: "panel-left", help: "Show Sidebar (⌃⌘S)") {
                        // A sidebar set aside for the changes comes back in
                        // their place.
                        if place.sidebar { place.changes = false }
                        place.sidebar = true
                    }
                    history
                }
                title
                Spacer(minLength: 8)
                if content.updateReady {
                    Button("Restart to Update") { actions.restartToUpdate() }
                        .buttonStyle(.borderless)
                        .font(.ui(.meta))
                        .foregroundStyle(.secondary)
                }
                // A provider subagent has no terminal and no folder of its
                // own, so neither its interface nor its changes open.
                if place.surface == .agents, let agent = place.agent, content.fleet.row(agent)?.native != true {
                    BarButton(symbol: "square-terminal", help: "Open in Terminal") { actions.openTerminal(agent) }
                    if !place.changes {
                        BarButton(symbol: "panel-right", help: "Show Changes (⌥⌘I)") { place.changes = true }
                    }
                }
            }
            notices
            detail
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .clipped()
        }
    }

    /// Back and forward through the agents opened.
    private var history: some View {
        HStack(spacing: 0) {
            BarButton(symbol: "arrow-left", help: "Back (⌘[)", size: 13, disabled: !place.canGoBack) {
                place.goBack(where: exists)
            }
            BarButton(symbol: "arrow-right", help: "Forward (⌘])", size: 13, disabled: !place.canGoForward) {
                place.goForward(where: exists)
            }
        }
    }

    /// The open agent's place and name, as a path, the name a menu of what
    /// can be done to it.
    @ViewBuilder private var title: some View {
        if place.surface == .agents, let agent = place.agent, let row = content.fleet.row(agent) {
            HStack(spacing: 6) {
                RepositoryTile(repository: row.card.repository, size: 18, native: row.native)
                Text(row.card.repository?.name ?? PlacesWords.place(row))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                Text("/").foregroundStyle(.tertiary)
                Menu {
                    AgentMenu(row: row, actions: actions)
                } label: {
                    HStack(spacing: 4) {
                        Text(row.name).fontWeight(.medium).lineLimit(1)
                        Icon("chevron-down", size: 11).foregroundStyle(.tertiary)
                    }
                    .padding(.horizontal, 6)
                    .frame(height: 24)
                    .contentShape(Rectangle())
                }
                .menuStyle(.button)
                .buttonStyle(HoverButtonStyle())
                .menuIndicator(.hidden)
                .fixedSize()
            }
            .font(.ui(.body))
            .help("\(PlaceNames.host(row.hostName)) · \(PlaceNames.path(row.workingDirectory))")
        } else if place.surface == .agents, place.agent == nil {
            Text("New Agent").font(.ui(.body, weight: .medium))
        }
    }

    @ViewBuilder private var notices: some View {
        if let problem = content.problem {
            Notice(text: problem, symbol: "triangle-alert", tint: Palette.failure, dismiss: actions.dismissProblem)
        }
        if let note = content.note {
            Notice(text: note, symbol: "circle-check", tint: .secondary)
        }
    }
}

extension MainScreen where Inspector == EmptyView {
    public init(
        content: MainContent, place: Binding<Place>, actions: MainActions = MainActions(),
        @ViewBuilder detail: () -> Detail
    ) {
        self.init(content: content, place: place, actions: actions, detail: detail, inspector: { EmptyView() })
    }
}

/// One line across the top of the centre, about something that happened.
private struct Notice: View {
    let text: String
    let symbol: String
    let tint: Color
    var dismiss: (@MainActor () -> Void)?

    var body: some View {
        HStack(spacing: 8) {
            Icon(symbol, size: 14).foregroundStyle(tint)
            Text(text).foregroundStyle(.secondary).textSelection(.enabled)
            Spacer()
            if let dismiss {
                BarButton(symbol: "x", help: "Dismiss", size: 12) { dismiss() }
            }
        }
        .font(.callout)
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background(Palette.hover)
    }
}

/// How a row's place is written in one short line.
enum PlacesWords {
    /// The folder's own name, which is how a person knows a repository;
    /// the host's when there is no folder.
    static func place(_ row: FleetRow) -> String {
        let folder = (row.workingDirectory as NSString).lastPathComponent
        return folder.isEmpty ? row.hostName : folder
    }
}

/// The centre with nothing open in it.
public struct EmptyCentre: View {
    let surface: Surface
    let newAgent: @MainActor () -> Void

    public init(surface: Surface, newAgent: @escaping @MainActor () -> Void = {}) {
        self.surface = surface
        self.newAgent = newAgent
    }

    public var body: some View {
        VStack(spacing: 12) {
            switch surface {
            case .agents:
                Text("Pick an agent, or start one.")
                    .foregroundStyle(.secondary)
                Button("New Agent", action: newAgent)
                    .controlSize(.large)
            case .hosts:
                Text("Pick a host to see how this Mac reaches it.")
                    .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// How the columns share a window too narrow for all three at their
/// widths: the inspector gives way to its narrowest first, then the
/// sidebar to its own, then the sidebar steps aside, so the chat never
/// drops below a readable width. Widths include the rail and each column's
/// edge.
struct ColumnLayout {
    static let centre: CGFloat = 380
    static let narrowestSidebar = Metrics.rail + Metrics.sidebarRange.lowerBound + 1
    static let narrowestInspector = Metrics.inspectorRange.lowerBound + 1

    let showsSidebar: Bool
    let sidebar: CGFloat
    let inspector: CGFloat

    init(width: CGFloat, sidebar: CGFloat, inspector: CGFloat) {
        let least = inspector > 0 ? Self.narrowestInspector : 0
        showsSidebar = sidebar > 0 && Self.narrowestSidebar + Self.centre + least <= width
        self.sidebar = showsSidebar ? max(Self.narrowestSidebar, min(sidebar, width - Self.centre - least)) : 0
        self.inspector = inspector > 0 ? max(least, min(inspector, width - self.sidebar - Self.centre)) : 0
    }
}

/// What can be done to an agent, for its title's menu and its row's.
struct AgentMenu: View {
    let row: FleetRow
    let actions: MainActions

    var body: some View {
        let agent = row.card.agent
        if !row.native {
            Button("Open in Terminal") { actions.openTerminal(agent) }
            Divider()
        }
        if row.pinned {
            Button("Unpin") { actions.act(.unfile, agent) }
        } else if row.pinnable {
            Button("Pin") { actions.act(.pin, agent) }
        }
        if row.closed {
            Button("Unsettle") { actions.act(.unfile, agent) }
        } else if row.card.closeHeld {
            Button("Don’t Settle") { actions.act(.cancelClose, agent) }
        } else {
            Button("Settle") { actions.act(.close, agent) }
        }
    }
}
