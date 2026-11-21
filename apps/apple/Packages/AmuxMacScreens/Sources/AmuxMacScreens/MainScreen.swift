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
    /// An agent the sidebar does not list, as a row: a member of a family,
    /// whose head stands for it there. Nil for one the host no longer has.
    public var member: @MainActor (AgentKey) -> FleetRow?
    /// The head of the family an agent the sidebar does not list is in:
    /// the row that stands for it.
    public var head: @MainActor (AgentKey) -> AgentKey?
    /// The open agent's branch, changes, relatives and actions, for the
    /// bars around its chat.
    public var opened: OpenAgentBar?

    public init(
        profiles: [ProfileView], profile: String? = nil, fleet: FleetContent = FleetContent(),
        hosts: [HostView] = [], thisMac: HostView? = nil, toFinish: UInt32 = 0, problem: String? = nil,
        note: String? = nil, signingIn: Bool = false, updateReady: Bool = false,
        member: @escaping @MainActor (AgentKey) -> FleetRow? = { _ in nil },
        head: @escaping @MainActor (AgentKey) -> AgentKey? = { _ in nil }, opened: OpenAgentBar? = nil
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
        self.member = member
        self.head = head
        self.opened = opened
    }

    var current: ProfileView? { profiles.first { $0.id == profile } }

    /// An agent's row, whether the sidebar lists it or a family's head
    /// stands for it there.
    @MainActor public func row(_ agent: AgentKey) -> FleetRow? {
        fleet.row(agent) ?? member(agent)
    }

    /// The sidebar's row for an agent: its own, else its family head's.
    @MainActor func listed(_ agent: AgentKey) -> AgentKey? {
        fleet.row(agent) != nil ? agent : head(agent)
    }
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
    /// Pins, settles, renames or deletes an agent.
    public var act: @MainActor (AgentAct, AgentKey) -> Void
    /// What deleting an agent does to its workspace, for its confirmation.
    public var deleteWarning: DeleteWarningAsk
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
        act: @escaping @MainActor (AgentAct, AgentKey) -> Void = { _, _ in },
        deleteWarning: @escaping DeleteWarningAsk = { _ in .failure(DeleteUnasked()) },
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
        self.act = act
        self.deleteWarning = deleteWarning
        self.dismissProblem = dismissProblem
        self.openTerminal = openTerminal
    }
}

/// The main window: a frame holding the window's buttons, the title row
/// and the rail, with the app's content on it as one rounded window: the
/// open surface's sidebar, its centre and, for an agent, the tool column,
/// hairlines between them, with the tools' tabs on the title row above
/// the tool column.
///
/// The frame carries only what is the window's: the system's buttons,
/// which stay where the system puts them, the sidebar toggle, back and
/// forward, and the surfaces. What belongs to the open agent sits in its
/// window's own bar.
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
                width: window.size.width - Metrics.rail - Metrics.gap,
                sidebar: place.sidebar ? sidebarWidth + 1 : 0,
                inspector: inspecting ? inspectorWidth + 1 : 0)
            VStack(spacing: 0) {
                titleRow(layout, width: window.size.width)
                    .frame(width: window.size.width, height: bar, alignment: .leading)
                    .clipped()
                HStack(spacing: 0) {
                    Rail(content: content, surface: $place.surface, actions: actions)
                        .frame(width: Metrics.rail)
                    // One rounded window on the frame: the list, the chat and
                    // the tools, hairlines between them.
                    workspace(layout)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .onFrame()
                }
                .padding(.trailing, Metrics.gap)
                .padding(.bottom, Metrics.gap)
            }
            .background {
                // The frame is the window's to drag, wherever nothing on it
                // takes the pointer.
                Palette.frame.gesture(WindowDragGesture())
            }
            .ignoresSafeArea(.container, edges: .top)
        }
        .textTiers()
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

    private var inspecting: Bool { content.showsChanges(place) }

    private func exists(_ agent: AgentKey) -> Bool { content.row(agent) != nil }

    /// The open agent, when it has a folder and terminal of its own: a
    /// provider subagent has neither.
    private var ownAgent: AgentKey? {
        guard place.surface == .agents, let agent = place.agent, content.row(agent)?.native != true else { return nil }
        return agent
    }

    // MARK: - The title row

    /// The frame's top, its parts over the columns under them: over the
    /// rail and sidebar, the sidebar toggle and back and forward; over the
    /// chat, the open agent's toolbar; over the tool column, past a short
    /// divider, the tools' tabs. Every button is centred on the window
    /// buttons' centre line, and the window drags from the rest.
    private func titleRow(_ layout: ColumnLayout, width: CGFloat) -> some View {
        let left = max(Metrics.rail + layout.sidebar, Self.leftCluster)
        return HStack(spacing: 0) {
            HStack(spacing: 6) {
                BarButton(
                    symbol: layout.showsSidebar ? "panel-left-close" : "panel-left-open",
                    help: layout.showsSidebar ? "Hide sidebar (⌃⌘S)" : "Show sidebar (⌃⌘S)"
                ) {
                    if layout.showsSidebar {
                        place.sidebar = false
                    } else {
                        // A sidebar set aside for the tools comes back in
                        // their place.
                        if place.sidebar { place.changes = false }
                        place.sidebar = true
                    }
                }
                history
            }
            .padding(.leading, Metrics.windowButtons)
            .frame(width: left, alignment: .leading)
            chatToolbar()
                .padding(.leading, 10)
                .padding(.trailing, 6)
                .frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
            if content.updateReady {
                Button("Restart to update") { actions.restartToUpdate() }
                    .buttonStyle(AccentButtonStyle())
                    .padding(.trailing, 10)
            }
            if inspecting {
                Rectangle().fill(Palette.line).frame(width: 1, height: 16)
                ToolStrip(
                    tabs: [ToolTabItem(symbol: "file-diff", title: "Review", counts: nil)],
                    selected: 0, close: { place.changes = false }, tools: toolsMenu
                ) {
                    toolToggle
                }
                .frame(width: layout.inspector - 1)
            } else if ownAgent != nil {
                toolToggle
            }
        }
        .padding(.trailing, Metrics.gap)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .gesture(WindowDragGesture())
    }

    /// The room the window's buttons, the sidebar toggle and history take.
    private static var leftCluster: CGFloat { Metrics.windowButtons + 3 * 28 + 2 * 6 + 8 }

    /// The tools that can open in the tool column, for its +.
    private var toolsMenu: ToolsMenu {
        var terminal: (@MainActor () -> Void)?
        if let agent = ownAgent, content.row(agent)?.card.attachable == true {
            let open = actions.openTerminal
            terminal = { open(agent) }
        }
        let binding = $place
        return ToolsMenu(reviewOpen: inspecting, openReview: { binding.wrappedValue.changes = true }, openTerminal: terminal)
    }

    /// Shows or hides the tool window; while it is hidden, the open agent's
    /// change counts ride on it, so what the window would show is in view.
    @ViewBuilder private var toolToggle: some View {
        if ownAgent != nil {
            Button {
                place.changes.toggle()
            } label: {
                HStack(spacing: 6) {
                    if !inspecting, let changes = content.opened?.changes, changes.files > 0 {
                        ChangeCounts(changes: changes).font(.ui(.meta)).offset(y: 1)
                    }
                    Icon("panel-right", size: 16, optical: false)
                        .foregroundStyle(inspecting ? AnyShapeStyle(.primary) : AnyShapeStyle(Palette.textSecondary))
                }
                .padding(.horizontal, 6)
                .frame(minWidth: 28)
                .frame(height: 26)
                .contentShape(Rectangle())
            }
            .buttonStyle(HoverButtonStyle(active: inspecting))
            .help(inspecting ? "Hide tools (⌥⌘I)" : "Show tools (⌥⌘I)")
        }
    }

    /// Back and forward through the agents opened.
    private var history: some View {
        HStack(spacing: 6) {
            BarButton(symbol: "arrow-left", help: "Back (⌘[)", disabled: !place.canGoBack) {
                place.goBack(where: exists)
            }
            BarButton(symbol: "arrow-right", help: "Forward (⌘])", disabled: !place.canGoForward) {
                place.goForward(where: exists)
            }
        }
    }

    // MARK: - The workspace window

    private func workspace(_ layout: ColumnLayout) -> some View {
        HStack(spacing: 0) {
            if layout.showsSidebar {
                surfaceSidebar
                    .frame(width: layout.sidebar - 1)
                    .frame(maxHeight: .infinity)
                    .background(Palette.sidebar)
                    .clipped()
                ColumnEdge(width: $sidebarWidth, range: Metrics.sidebarRange)
            }
            centre
                .frame(minWidth: ColumnLayout.centre, maxWidth: .infinity)
            if inspecting {
                ColumnEdge(width: $inspectorWidth, range: Metrics.inspectorRange, trailing: true)
                inspector
                    .frame(width: layout.inspector - 1)
                    .frame(maxHeight: .infinity)
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

    private var centre: some View {
        VStack(spacing: 0) {
            notices
            detail
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .clipped()
        }
    }

    /// The open agent as a path: its repository's tile and name, the agent
    /// that started it when it has a parent, and its own name, a menu of
    /// what can be done to it and its relatives; then its branch, and its
    /// host when that is not this Mac. Else what the centre shows. What it
    /// can leave out goes as the room runs out — the repository's name,
    /// then the host, then the branch — each only once the whole name
    /// would otherwise be cut, and the agent's name truncates last.
    @ViewBuilder private func chatToolbar() -> some View {
        if place.surface == .agents, let agent = place.agent, let row = content.row(agent) {
            HStack(spacing: 6) {
                ViewThatFits(in: .horizontal) {
                    agentPath(row, repository: true, host: true, branch: true)
                    agentPath(row, repository: false, host: true, branch: true)
                    agentPath(row, repository: false, host: false, branch: true)
                    agentPath(row, repository: false, host: false, branch: false)
                    agentPath(row, repository: false, host: false, branch: false, truncating: true)
                }
                Spacer(minLength: 8)
                if ownAgent != nil, let opened = content.opened, opened.actions.opens {
                    OpenMenu(actions: opened.actions)
                }
            }
            .font(.ui(.prose))
            .help("\(PlaceNames.host(row.hostName)) · \(PlaceNames.path(row.workingDirectory))")
        } else if place.surface == .agents, place.agent == nil {
            Text("New agent").font(.ui(.prose, weight: .medium))
        } else if place.surface == .hosts, let id = place.host,
            let host = ([content.thisMac].compactMap { $0 } + content.hosts).first(where: { $0.hostId == id })
        {
            Text(PlaceNames.host(host.name)).font(.ui(.prose, weight: .medium))
        }
    }

    /// One variant of the open agent's path, for the toolbar to pick the
    /// fullest that fits; the last truncates the agent's name.
    private func agentPath(
        _ row: FleetRow, repository: Bool, host: Bool, branch: Bool, truncating: Bool = false
    ) -> some View {
        HStack(spacing: 6) {
            RepositoryTile(repository: row.card.repository, size: 16, native: row.native)
            if repository {
                Text(row.card.repository?.name ?? PlacesWords.place(row))
                    .foregroundStyle(Palette.textSecondary)
                    .lineLimit(1)
                    .fixedSize()
                separator
            }
            if let parent = content.opened?.family?.parent {
                Button(parent.name.isEmpty ? "agent" : parent.name) { place.agent = parent.agent }
                    .buttonStyle(.plain)
                    .foregroundStyle(Palette.textSecondary)
                    .lineLimit(1)
                    .fixedSize(horizontal: !truncating, vertical: false)
                    .layoutPriority(-1)
                    .help("Open the agent that started it")
                separator
            }
            Menu {
                AgentMenu(row: row, actions: actions)
                LineageItems(family: content.opened?.family) { place.agent = $0 }
            } label: {
                HStack(spacing: 4) {
                    Text(row.name).fontWeight(.medium).lineLimit(1)
                    Icon("chevron-down", size: 12).foregroundStyle(Palette.textTertiary)
                }
                .padding(.horizontal, 6)
                .frame(height: 28)
                .contentShape(Rectangle())
            }
            .menuStyle(.button)
            .buttonStyle(HoverButtonStyle())
            .menuIndicator(.hidden)
            .fixedSize(horizontal: !truncating, vertical: false)
            .layoutPriority(1)
            .padding(.leading, -6)
            if branch, let name = content.opened?.branch ?? row.branch, !name.isEmpty {
                HStack(spacing: 4) {
                    Icon("git-branch", size: 12)
                    Text(name).font(.mono(.small)).lineLimit(1).fixedSize()
                }
                .foregroundStyle(Palette.textTertiary)
                .padding(.leading, 4)
                // The smaller monospaced line, centred, sits above the
                // title's baseline; this puts them on one.
                .offset(y: 1)
                .help("Branch \(name)")
            }
            if host, row.card.agent.host != content.thisMac?.hostId {
                Text(PlaceNames.host(row.hostName))
                    .font(.ui(.caption, weight: .medium))
                    .foregroundStyle(Palette.textSecondary)
                    .padding(.horizontal, 6)
                    .frame(height: 18)
                    .background(RoundedRectangle(cornerRadius: 5, style: .continuous).fill(Palette.well))
                    .fixedSize()
                    .padding(.leading, 4)
            }
        }
    }

    private var separator: some View {
        Text("/").foregroundStyle(Palette.textQuaternary)
    }

    @ViewBuilder private var notices: some View {
        if let problem = content.problem {
            Notice(text: problem, symbol: "triangle-alert", tint: Palette.failure, dismiss: actions.dismissProblem)
        }
        if let note = content.note {
            Notice(text: note, symbol: "circle-check", tint: Palette.textSecondary)
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
            Text(text).foregroundStyle(Palette.textSecondary).textSelection(.enabled)
            Spacer()
            if let dismiss {
                BarButton(symbol: "x", help: "Dismiss", size: 12) { dismiss() }
            }
        }
        .font(.ui(.body))
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
                    .foregroundStyle(Palette.textSecondary)
                Button("New agent", action: newAgent)
                    .controlSize(.large)
            case .hosts:
                Text("Pick a device to see how it’s connected.")
                    .foregroundStyle(Palette.textSecondary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// How the windows share a frame too narrow for all three columns at their
/// widths: the tool window gives way to its narrowest first, then the
/// sidebar to its own, then the sidebar steps aside, so the chat never
/// drops below a readable width. The window's least width fits the rail,
/// the chat and the narrowest tool window (`MainWindowSize.least`).
/// `width` is the frame's room past the rail and the right-hand gap; the
/// sidebar's and the tool column's widths include their hairlines.
struct ColumnLayout {
    static let centre: CGFloat = 380
    static let narrowestSidebar = Metrics.sidebarRange.lowerBound + 1
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

/// The smallest the main window goes.
public enum MainWindowSize {
    /// Wide enough for the rail, a readable chat and the narrowest tool
    /// window beside it; tall enough for setting up's screens.
    public static let least = CGSize(
        width: Metrics.rail + ColumnLayout.centre + ColumnLayout.narrowestInspector + Metrics.gap, height: 560)
}

/// What can be done to an agent, for its title's menu and its row's.
struct AgentMenu: View {
    let row: FleetRow
    let actions: MainActions

    var body: some View {
        let agent = row.card.agent
        // Only an agent with a terminal interface of its own has one to
        // attach to.
        if !row.native, row.card.attachable {
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
            Button("Don’t settle") { actions.act(.cancelClose, agent) }
        } else {
            Button("Settle") { actions.act(.close, agent) }
        }
    }
}

extension MainContent {
    /// Whether the window shows the changes pane at `place`: the open
    /// agent's own folder, which a provider subagent does not have.
    @MainActor public func showsChanges(_ place: Place) -> Bool {
        place.showsChanges(native: place.agent.flatMap { row($0) }?.native == true)
    }
}
