import AmuxValues
import AmuxWords
import AppKit
import SwiftUI

/// Whether a host has said what a provider offers there.
public enum OfferState: Equatable, Sendable {
    case asking
    case offered
    case unavailable
}

/// Whether a host has listed its directories.
public enum ListingState: Equatable, Sendable {
    case none
    case asking
    case ready
    case unavailable
}

/// Starting an agent, as the New Agent page reads and changes it. The app adapts its
/// new-agent store to this; a story holds one still.
@MainActor
public protocol NewAgentSurface: AnyObject {
    /// Every host an agent can be started on, this Mac first.
    var hosts: [HostView] { get }
    var machine: [UInt8]? { get }
    func point(at host: [UInt8])
    /// The chosen host's reach as the host list says it now.
    func reached(_ online: Bool)

    var listing: ListingState { get }
    var repositories: [Directory] { get }
    var recent: [Directory] { get }
    /// Where Claude Code and Codex ran on the host lately, outside amux.
    var history: [Directory] { get }
    /// Whether the host keeps the folder among its repositories.
    func isPinned(_ path: String) -> Bool
    /// Pins a folder among the host's repositories, or unpins it.
    func pin(_ directory: Directory, _ on: Bool)
    /// Where an agent is most likely wanted: where agents worked lately,
    /// then the repositories.
    var likely: [Directory] { get }
    var directory: String { get }
    func choose(directory: String)
    /// What the directory search holds; setting it searches.
    var query: String { get set }
    var searching: Bool { get }
    var found: [Directory] { get }
    var typed: String { get set }

    /// The agents the chosen host offers, in its order, named as it names
    /// them; empty until it has said.
    var agents: [HostProvider] { get }
    /// The chosen agent's id; nil while the host lists none.
    var provider: String? { get }
    func choose(provider: String)
    /// What the chosen host has said of the chosen provider; nil before it
    /// was asked.
    var offer: OfferState? { get }
    var settings: SettingsView? { get }
    func choose(_ pick: NewAgentPick)
    var newWorktree: Bool { get set }
    var name: String { get set }
    /// The first message, sent once the agent has started.
    var prompt: String { get set }

    var starting: Bool { get }
    var failure: String? { get }
    var ready: Bool { get }
    func start()
}

/// Starting an agent the way a chat starts: an empty page
/// with a composer, the first message typed into it, and where and with
/// what it runs as chips under the field. Return starts it on the host the
/// first chip names and sends the message.
public struct NewAgentPage: View {
    let surface: any NewAgentSurface
    @FocusState private var focused: Bool
    @State private var browsing = false
    /// The agent picker is open over the composer.
    @State private var pickingAgent = false
    /// The room the chips have, and each level's labels' width, for
    /// `fitting`.
    @State private var room: CGFloat = 0
    @State private var widths: [Int: CGFloat] = [:]

    public init(surface: any NewAgentSurface) {
        self.surface = surface
    }

    public var body: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 40)
            // Names the folder, so the page says where the work happens. The name is set in a
            // heavier weight than the words around it, so a folder called
            // "amux" reads as the folder rather than the product.
            heading
                .font(.ui(.heading, weight: .regular))
                .multilineTextAlignment(.center)
                .padding(.bottom, 24)
            composer
            // Where it runs, under the field, so the message comes first and
            // its setting second.
            strip
                .padding(.top, 8)
                .padding(.horizontal, 8)
            notes
                .padding(.top, 12)
                .padding(.horizontal, 6)
            Spacer(minLength: 40)
            Spacer(minLength: 0)
        }
        .frame(maxWidth: Metrics.reading)
        .padding(.horizontal, Metrics.gutter)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear { focused = true }
        .onChange(of: chosenHost.map(HostWords.online), initial: true) { _, online in
            if let online { surface.reached(online) }
        }
    }

    private var heading: Text {
        let name = (surface.directory as NSString).lastPathComponent
        guard !name.isEmpty else { return Text("What should we build?") }
        return Text("What should we build in \(Text(name).font(.ui(.heading, weight: .semibold)))?")
    }

    private var described: Bool {
        !surface.prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private var startable: Bool { surface.ready && described }

    private var composer: some View {
        ComposerShell(
            placeholder: "Ask anything",
            text: Binding(get: { surface.prompt }, set: { surface.prompt = $0 }), lines: 3...14,
            focused: $focused, submit: { if startable { surface.start() } }
        ) {
            EmptyView()
        } controls: {
            Spacer(minLength: 8)
            // In the field's own row, which has room for it whatever the
            // strip under the field gives up.
            agentMenu(Compaction(level: 0))
        } action: {
            RoundButton(
                face: surface.starting ? .starting : .send, enabled: startable,
                help: "Start on \(hostName) (↩)"
            ) {
                surface.start()
            }
        }
    }

    /// Where the agent runs, under the field: host, folder and worktree at
    /// the left, the permission at the right.
    private var strip: some View {
        Group {
            // As the window narrows the chips give up words in this order:
            // the host's name (its icon stays), the permission's and
            // worktree's words, then the folder's.
            //
            // The menus are costly to measure, so they are built once, at
            // the level that fits; what fits is found by measuring their
            // labels alone, which are drawn hidden behind them. Offering
            // every level of menus to `ViewThatFits` instead took seconds
            // to lay out.
            picks(Compaction(level: fitting))
                .frame(maxWidth: .infinity, alignment: .leading)
                .onGeometryChange(for: CGFloat.self, of: \.size.width) { room = $0 }
                .background(alignment: .leading) { measures }
        }
    }

    /// The first level whose labels fit the room the chips have, leaving
    /// the gap before the round button; the last level when none does.
    private var fitting: Int {
        let gap: CGFloat = 8
        return (0...Compaction.most).first { level in
            widths[level].map { $0 + gap <= room } ?? false
        } ?? Compaction.most
    }

    /// Every level's labels at their own widths, hidden.
    private var measures: some View {
        ZStack(alignment: .leading) {
            ForEach(0...Compaction.most, id: \.self) { level in
                faces(Compaction(level: level))
                    .fixedSize()
                    .onGeometryChange(for: CGFloat.self, of: \.size.width) { widths[level] = $0 }
            }
        }
        .hidden()
        .accessibilityHidden(true)
    }

    /// The chips' labels alone, as wide as the chips themselves.
    private func faces(_ fit: Compaction) -> some View {
        HStack(spacing: 2) {
            hostLabel(fit)
            folderLabel(fit)
            worktreeLabel(fit)
            permissionLabel(fit)
        }
    }

    /// What the chips under the field leave out to fit.
    private struct Compaction {
        static let most = 5
        let level: Int
        var model: Bool { level < 1 }
        var effort: Bool { level < 2 }
        var hostName: Bool { level < 3 }
        var words: Bool { level < 4 }
        var folderName: Bool { level < 5 }
    }

    // MARK: - Where

    private var chosenHost: HostView? {
        surface.hosts.first { $0.hostId == surface.machine }
    }

    private var hostName: String {
        chosenHost.map { $0.local ? "this Mac" : PlaceNames.host($0.name) } ?? "a device"
    }

    private func picks(_ fit: Compaction) -> some View {
        HStack(spacing: 2) {
            hostMenu(fit)
            folderButton(fit)
            worktreeToggle(fit)
            Spacer(minLength: 8)
            permissionMenu(fit)
        }
    }

    private func hostMenu(_ fit: Compaction) -> some View {
        Menu {
            ForEach(surface.hosts, id: \.hostId) { host in
                Toggle(
                    isOn: Binding(get: { host.hostId == surface.machine }, set: { _ in surface.point(at: host.hostId) })
                ) {
                    Text(host.local ? "This Mac" : PlaceNames.host(host.name))
                    if !HostWords.online(host) { Text(HostWords.reach(host)) }
                }
                .disabled(!HostWords.online(host))
            }
        } label: {
            hostLabel(fit)
        }
        .menuStyle(.button)
        .buttonStyle(HoverButtonStyle())
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Runs on \(hostName)")
    }

    private func hostLabel(_ fit: Compaction) -> some View {
        ChipLabel(
            icon: chosenHost.map(HostWords.lucide) ?? "monitor",
            text: fit.hostName ? chosenHost.map { $0.local ? "This Mac" : PlaceNames.host($0.name) } ?? "Choose a device" : nil)
    }

    private func folderButton(_ fit: Compaction) -> some View {
        Button {
            browsing = true
        } label: {
            folderLabel(fit)
        }
        .buttonStyle(HoverButtonStyle())
        .fixedSize()
        .disabled(surface.machine == nil)
        .help(surface.directory.isEmpty ? "Choose a folder" : PlaceNames.path(surface.directory))
        .popover(isPresented: $browsing, arrowEdge: .bottom) {
            DirectoryChooser(surface: surface) { browsing = false }
        }
    }

    private func folderLabel(_ fit: Compaction) -> some View {
        HStack(spacing: 0) {
            if let chosen = chosenDirectory {
                DirectoryMark(directory: chosen, size: 16).padding(.leading, 8)
                ChipLabel(icon: nil, text: fit.folderName ? folder : nil).padding(.leading, -2)
            } else {
                ChipLabel(icon: "folder", text: fit.folderName ? folder : nil)
            }
        }
    }

    /// The chosen folder as the host listed it, for its repository's mark.
    private var chosenDirectory: Directory? {
        guard !surface.directory.isEmpty else { return nil }
        return (surface.repositories + surface.recent + surface.likely + surface.found)
            .first { $0.path == surface.directory && $0.repository != nil }
    }

    private var folder: String {
        let name = (surface.directory as NSString).lastPathComponent
        return name.isEmpty ? "Choose a folder" : name
    }

    /// Whether the agent gets a worktree of its own: on, it is drawn in
    /// blue, as amux doing something for the person.
    private func worktreeToggle(_ fit: Compaction) -> some View {
        let on = surface.newWorktree
        return Button {
            surface.newWorktree.toggle()
        } label: {
            worktreeLabel(fit)
        }
        .buttonStyle(HoverButtonStyle())
        .fixedSize()
        .help(
            on
                ? "The agent gets its own worktree, a separate copy on a new branch"
                : "Give the agent its own worktree: a separate copy on a new branch, so your checkout stays as it is")
        .accessibilityLabel("New worktree")
        .accessibilityValue(on ? "On" : "Off")
    }

    private func worktreeLabel(_ fit: Compaction) -> some View {
        let on = surface.newWorktree
        return ChipLabel(
            icon: "git-branch", text: fit.words ? "Worktree" : nil, tint: on ? Palette.accent : Palette.textSecondary,
            menu: false
        )
        .background(
            RoundedRectangle(cornerRadius: Radius.row, style: .continuous)
                .fill(on ? Palette.accent.opacity(0.12) : .clear))
    }

    // MARK: - What runs there

    /// The chosen agent as the host lists it.
    private var agent: HostProvider? {
        surface.agents.first { $0.provider == surface.provider }
    }

    /// The agent, and the model and effort it starts with.
    private func agentMenu(_ fit: Compaction) -> some View {
        Button { pickingAgent.toggle() } label: { agentLabel(fit) }
            .buttonStyle(HoverButtonStyle())
            .fixedSize()
            .help("Agent, model and effort")
            .popover(isPresented: $pickingAgent, arrowEdge: .top) { AgentPicker(surface: surface) }
    }

    /// The provider's glyph, then the model and the effort as the chat's
    /// composer shows them: "✳ Opus 5.5 High". The glyph always stays: the
    /// provider is what the agent is; the model, else the provider's name.
    /// An agent the app carries no mark for is named in the glyph's place.
    private func agentLabel(_ fit: Compaction) -> some View {
        var model: String?
        var effort: String?
        if let settings = surface.settings, surface.offer == .offered {
            model = fit.model ? settings.models.first(where: \.current)?.displayName : nil
            effort = fit.effort && settings.changeable.effort == .pick
                ? settings.efforts.first(where: \.current)?.value.capitalized : nil
        }
        let name = agent?.name ?? "Agent"
        let glyph = surface.provider.flatMap(ProviderMark.glyph)
        let text = glyph == nil ? [name, model].compactMap { $0 }.joined(separator: " · ") : model ?? name
        return ChipLabel(
            icon: glyph, text: text, detail: model == nil ? nil : effort, tint: .primary)
            .help(name)
    }

    /// The permission the agent starts with, when the host lets it be
    /// picked.
    private var permission: (all: [PermissionChoice], current: PermissionChoice)? {
        guard let settings = surface.settings, surface.offer == .offered, settings.changeable.permission == .pick,
            let current = settings.permissions.first(where: \.current)
        else { return nil }
        return (settings.permissions, current)
    }

    @ViewBuilder private func permissionLabel(_ fit: Compaction) -> some View {
        if let current = permission?.current {
            ChipLabel(
                icon: current.neverAsks ? "shield-alert" : "lock", text: fit.words ? ChatWords.permission(current) : nil,
                tint: current.neverAsks ? Palette.caution : Palette.textSecondary)
        }
    }

    @ViewBuilder private func permissionMenu(_ fit: Compaction) -> some View {
        if let (all, current) = permission {
            Menu {
                ForEach(all.filter(\.settable), id: \.value) { choice in
                    Toggle(isOn: Binding(get: { choice.current }, set: { _ in surface.choose(.permission(choice.value)) })) {
                        Text(ChatWords.permission(choice))
                        if !ChatWords.permissionDetail(choice).isEmpty { Text(ChatWords.permissionDetail(choice)) }
                    }
                }
            } label: {
                permissionLabel(fit)
            }
            .menuStyle(.button)
            .buttonStyle(HoverButtonStyle())
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Permission: \(ChatWords.permission(current))")
        }
    }


    // MARK: - Said under it

    @ViewBuilder private var notes: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let failure = surface.failure {
                Label { Text(failure).textSelection(.enabled) } icon: { Icon("triangle-alert", size: 14) }
                    .foregroundStyle(Palette.failure)
            }
            if surface.listing == .unavailable {
                Text("Couldn’t list folders on \(hostName). Type a path instead.")
            }
            if let agent, agent.signedIn == false {
                // In the agent's own words for what to do, on the host
                // where it keeps its credential.
                Text(ChatWords.withCode("\(agent.name) isn’t signed in on \(hostName). \(ChatWords.signInStepsThere(agent.signInHint))"))
            }
            if let agent, surface.machine != nil, surface.offer == .unavailable {
                Text("Couldn’t get \(agent.name)’s options from \(hostName). It’ll start with the defaults there.")
            }
            if !recent.isEmpty, surface.failure == nil {
                // As many as fit on one line: a row wider than the window
                // would push the composer off its edges.
                ViewThatFits(in: .horizontal) {
                    ForEach((1...recent.count).reversed(), id: \.self) { count in
                        recentRow(Array(recent.prefix(count)))
                    }
                }
            }
        }
        .font(.ui(.meta))
        .foregroundStyle(Palette.textSecondary)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// Folders to start in, one click away, each saying where it is on
    /// hover.
    private func recentRow(_ directories: [Directory]) -> some View {
        HStack(spacing: 6) {
            ForEach(directories, id: \.path) { directory in
                Button {
                    surface.choose(directory: directory.path)
                } label: {
                    HStack(spacing: 6) {
                        DirectoryMark(directory: directory, size: 16)
                        Text(directory.name).lineLimit(1)
                    }
                }
                .buttonStyle(ChipStyle())
                .fixedSize()
                .help("Start in \(PlaceNames.path(directory.path))")
            }
        }
    }

    /// Other folders the host was used in lately, the chosen one apart.
    private var recent: [Directory] {
        Array(surface.likely.filter { $0.path != surface.directory }.prefix(4))
    }
}

extension String {
    /// Only the first letter capitalised; `capitalized` does every word.
    var capitalizedFirst: String { prefix(1).uppercased() + dropFirst() }
}

/// The directories a host has to offer, searched as typed, and a field for
/// one it does not list.
/// The agent a new agent will be, and the model, effort and mode it
/// starts with, in one panel: the providers, each saying when the chosen
/// host has it signed out, then what the chosen one offers there.
public struct AgentPicker: View {
    let surface: any NewAgentSurface

    public init(surface: any NewAgentSurface) {
        self.surface = surface
    }

    public var body: some View {
        let host = surface.hosts.first { $0.hostId == surface.machine }
        let hostName = host.map { $0.local ? "this Mac" : PlaceNames.host($0.name) } ?? "this device"
        ModelPicker(
            providers: surface.agents.map { agent in
                ModelPicker.ProviderItem(
                    provider: agent.provider, name: agent.name,
                    glyph: ProviderMark.glyph(agent.provider),
                    note: agent.signedIn == false ? "Not signed in on \(hostName)" : nil)
            },
            provider: surface.provider,
            settings: surface.offer == .offered ? surface.settings : nil,
            chooseProvider: { surface.choose(provider: $0) },
            chooseModel: { surface.choose(.model($0)) },
            chooseEffort: { surface.choose(.effort($0)) },
            chooseMode: { surface.choose(.mode($0)) },
            chooseOwnMode: { surface.choose(.mode(nil)) })
    }
}

public struct DirectoryChooser: View {
    let surface: any NewAgentSurface
    let done: @MainActor () -> Void

    public init(surface: any NewAgentSurface, done: @escaping @MainActor () -> Void = {}) {
        self.surface = surface
        self.done = done
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if surface.listing != .unavailable {
                SearchField(
                    text: Binding(get: { surface.query }, set: { surface.query = $0 }),
                    prompt: "Search folders")
                    .padding(10)
                Rectangle().fill(Palette.line).frame(height: 1)
            }
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 1) { listing }
                    .padding(6)
            }
            .frame(height: 300)
            Rectangle().fill(Palette.line).frame(height: 1)
            HStack(spacing: 8) {
                TextField("Path, e.g. ~/src/project", text: Binding(get: { surface.typed }, set: { surface.typed = $0 }))
                    .textFieldStyle(.plain)
                    .font(.ui(.body))
                    .padding(.horizontal, 8)
                    .frame(height: 28)
                    .background(RoundedRectangle(cornerRadius: Radius.control, style: .continuous).fill(Palette.well))
                    .onSubmit(useTyped)
                Button("Choose", action: useTyped)
                    .buttonStyle(PillButtonStyle(prominent: !typedPath.isEmpty))
                    .disabled(typedPath.isEmpty)
            }
            .padding(10)
        }
        .frame(width: 400)
        .background(Palette.card)
    }

    private var typedPath: String { surface.typed.trimmingCharacters(in: .whitespacesAndNewlines) }

    private func useTyped() {
        guard !typedPath.isEmpty else { return }
        surface.choose(directory: typedPath)
        done()
    }

    @ViewBuilder private var listing: some View {
        if surface.searching {
            if !surface.found.isEmpty {
                SectionTitle(text: "Found")
                ForEach(surface.found, id: \.path) { row($0) }
            } else if surface.listing != .asking {
                message("No matching folders.")
            }
        } else {
            if !surface.repositories.isEmpty {
                SectionTitle(text: "Repositories")
                ForEach(surface.repositories, id: \.path) { row($0) }
            }
            if !surface.recent.isEmpty {
                SectionTitle(text: "Recent")
                ForEach(surface.recent, id: \.path) { row($0) }
            }
            if !surface.history.isEmpty {
                SectionTitle(text: "From Claude Code and Codex")
                ForEach(surface.history, id: \.path) { row($0) }
            }
            if surface.listing == .ready, surface.repositories.isEmpty, surface.recent.isEmpty,
                surface.history.isEmpty
            {
                message(onThisMac
                    ? "No repositories yet. Add one, or type a path below."
                    : "Nothing has started on this device yet. Type a path below.")
            }
            if onThisMac {
                Button(action: addRepositories) {
                    HStack(spacing: 9) {
                        Icon("plus", size: 14).foregroundStyle(Palette.textSecondary)
                            .frame(width: 16)
                        Text("Add repository…").font(.ui(.body))
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 5)
                    .contentShape(Rectangle())
                }
                .buttonStyle(HoverButtonStyle())
                .padding(.top, 2)
            }
        }
        switch surface.listing {
        case .asking: message("Loading repositories…")
        case .unavailable: message("This device couldn’t list its repositories.")
        case .none, .ready: EmptyView()
        }
    }

    /// Whether the chosen computer is this Mac, whose folders can be browsed.
    private var onThisMac: Bool {
        surface.hosts.first { $0.hostId == surface.machine }?.local ?? false
    }

    private func addRepositories() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = true
        panel.prompt = "Add"
        panel.message = "Choose folders for agents to work in."
        guard panel.runModal() == .OK else { return }
        for url in panel.urls {
            surface.pin(Directory(path: url.path, name: url.lastPathComponent, repository: nil), true)
        }
        if let first = panel.urls.first {
            surface.choose(directory: first.path)
            done()
        }
    }

    private func row(_ directory: Directory) -> some View {
        DirectoryRow(
            directory: directory,
            chosen: surface.directory == directory.path,
            pinned: surface.isPinned(directory.path),
            choose: {
                surface.choose(directory: directory.path)
                done()
            },
            pin: { surface.pin(directory, $0) })
    }

    private func message(_ text: String) -> some View {
        Text(text)
            .font(.ui(.meta))
            .foregroundStyle(Palette.textSecondary)
            .padding(10)
    }
}

/// One folder in the chooser: a click starts there; under the pointer a pin
/// keeps it among the computer's repositories, or lets it go.
private struct DirectoryRow: View {
    let directory: Directory
    let chosen: Bool
    let pinned: Bool
    let choose: () -> Void
    let pin: (Bool) -> Void
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 4) {
            Button(action: choose) {
                HStack(spacing: 9) {
                    DirectoryMark(directory: directory, size: 16)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(directory.name).font(.ui(.body))
                        Text(PlaceNames.path(directory.path))
                            .font(.ui(.meta))
                            .foregroundStyle(Palette.textSecondary)
                            .lineLimit(1)
                            .truncationMode(.head)
                    }
                    Spacer(minLength: 6)
                    if chosen {
                        Icon("check", size: 14).foregroundStyle(Palette.accent)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            BarButton(
                symbol: pinned ? "pin-off" : "pin",
                help: pinned ? "Remove from repositories" : "Keep in repositories", size: 14
            ) { pin(!pinned) }
            .opacity(hovering ? 1 : 0)
        }
        .padding(.leading, 8)
        .padding(.trailing, 2)
        .padding(.vertical, 3)
        .background(
            RoundedRectangle(cornerRadius: Radius.row, style: .continuous)
                .fill(hovering ? Palette.hover : .clear))
        .onHover { hovering = $0 }
    }
}

/// A folder to start in, one click away: a quiet well, lit under the
/// pointer.
private struct ChipStyle: ButtonStyle {
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.ui(.meta))
            .foregroundStyle(hovering ? Palette.text : Palette.textSecondary)
            .padding(.leading, 7)
            .padding(.trailing, 9)
            .frame(height: 24)
            .background(
                RoundedRectangle(cornerRadius: Radius.row, style: .continuous)
                    .fill(configuration.isPressed ? Palette.pressed : Palette.well))
            .overlay(
                RoundedRectangle(cornerRadius: Radius.row, style: .continuous)
                    .fill(hovering && !configuration.isPressed ? Palette.hover : .clear))
            .contentShape(Rectangle())
            .onHover { hovering = $0 }
    }
}

/// A folder's mark: its repository's monogram, the one the agents working
/// in any checkout of it carry; a plain folder for one that names none.
struct DirectoryMark: View {
    let directory: Directory
    var size: CGFloat = 16

    var body: some View {
        if let repository = directory.repository {
            RepositoryTile(repository: repository, size: size)
        } else {
            Icon("folder", size: size * 0.8).foregroundStyle(Palette.textTertiary).frame(width: size, height: size)
        }
    }
}
