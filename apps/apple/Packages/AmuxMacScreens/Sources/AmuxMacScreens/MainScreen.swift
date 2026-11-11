import AmuxValues
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
        act: @escaping @MainActor (AgentAct, AgentKey) -> Void = { _, _ in }
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
                    VStack(spacing: 0) {
                        TopBar(height: bar) {
                            Picker("Show", selection: inspectorTab) {
                                ForEach(InspectorTab.allCases) { tab in Text(tab.title).tag(tab) }
                            }
                            .pickerStyle(.segmented)
                            .labelsHidden()
                            .fixedSize()
                            Spacer()
                            BarButton(symbol: "xmark", help: "Close") { place.inspector = nil }
                        }
                        inspector
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                            .clipped()
                    }
                    .frame(width: layout.inspector - 1)
                    .background(Palette.content)
                }
            }
            .ignoresSafeArea(.container, edges: .top)
        }
        .windowChrome()
        .animation(.snappy(duration: 0.2), value: place.sidebar)
        .animation(.snappy(duration: 0.2), value: place.inspector)
    }

    private var inspecting: Bool {
        place.inspector != nil && place.surface == .agents && place.agent != nil
    }

    private var inspectorTab: Binding<InspectorTab> {
        Binding(get: { place.inspector ?? .changes }, set: { place.inspector = $0 })
    }

    // MARK: - The sidebar

    private func sidebar(bar: CGFloat) -> some View {
        VStack(spacing: 0) {
            TopBar(height: bar, leading: Metrics.windowButtons) {
                BarButton(symbol: "sidebar.left", help: "Hide Sidebar") { place.sidebar = false }
                Spacer()
                if place.surface == .agents {
                    BarButton(symbol: "square.and.pencil", help: "New Agent") { actions.newAgent() }
                        .keyboardShortcut("n")
                }
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
            FleetSidebar(content: content, place: $place, actions: actions)
        case .hosts:
            HostsSidebar(content: content, place: $place)
        }
    }

    // MARK: - The centre

    private func centre(bar: CGFloat, sidebarShown: Bool) -> some View {
        VStack(spacing: 0) {
            TopBar(height: bar, leading: sidebarShown ? 0 : Metrics.windowButtons) {
                if !sidebarShown {
                    BarButton(symbol: "sidebar.left", help: "Show Sidebar") {
                        // A sidebar set aside for the inspector comes back
                        // in its place.
                        if place.sidebar { place.inspector = nil }
                        place.sidebar = true
                    }
                    if place.surface == .agents {
                        BarButton(symbol: "square.and.pencil", help: "New Agent") { actions.newAgent() }
                    }
                }
                title
                Spacer(minLength: 8)
                if content.updateReady {
                    Button("Restart to Update") { actions.restartToUpdate() }
                        .buttonStyle(.borderless)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                if place.surface == .agents, place.agent != nil {
                    BarButton(
                        symbol: "sidebar.right", help: place.inspector == nil ? "Show Inspector" : "Hide Inspector",
                        active: place.inspector != nil
                    ) {
                        place.inspector = place.inspector == nil ? .changes : nil
                    }
                    .keyboardShortcut("i", modifiers: [.command, .option])
                }
            }
            notices
            detail
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .clipped()
        }
    }

    /// The open agent's place and name, as a path: where it runs, then what
    /// it is called.
    @ViewBuilder private var title: some View {
        if place.surface == .agents, let agent = place.agent, let row = content.fleet.row(agent) {
            HStack(spacing: 6) {
                Text(PlacesWords.place(row))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                Text("/").foregroundStyle(.tertiary)
                Text(row.name)
                    .fontWeight(.medium)
                    .lineLimit(1)
            }
            .font(.system(size: 13))
            .help("\(row.hostName) · \(row.workingDirectory)")
        }
    }

    @ViewBuilder private var notices: some View {
        if let problem = content.problem {
            Notice(text: problem, symbol: "exclamationmark.triangle", tint: .red)
        }
        if let note = content.note {
            Notice(text: note, symbol: "checkmark.circle", tint: .secondary)
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

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: symbol).foregroundStyle(tint)
            Text(text).foregroundStyle(.secondary)
            Spacer()
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
