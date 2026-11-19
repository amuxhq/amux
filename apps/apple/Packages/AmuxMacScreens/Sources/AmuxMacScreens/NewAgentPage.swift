import AmuxValues
import AmuxWords
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

    var provider: Provider { get }
    func choose(provider: Provider)
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
        chosenHost.map { $0.local ? "this Mac" : PlaceNames.host($0.name) } ?? "a computer"
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
            text: fit.hostName ? chosenHost.map { $0.local ? "This Mac" : PlaceNames.host($0.name) } ?? "Choose a computer" : nil)
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

    private func signedIn(_ provider: Provider) -> Bool? {
        chosenHost?.providers.first { $0.provider == provider }?.signedIn
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
    private func agentLabel(_ fit: Compaction) -> some View {
        var model: String?
        var effort: String?
        if let settings = surface.settings, surface.offer == .offered {
            model = fit.model ? settings.models.first(where: \.current)?.displayName : nil
            effort = fit.effort && settings.changeable.effort == .pick
                ? settings.efforts.first(where: \.current)?.value.capitalized : nil
        }
        return ChipLabel(
            icon: surface.provider == .codex ? "provider-codex" : "provider-claude",
            text: model ?? Self.title(surface.provider), detail: model == nil ? nil : effort, tint: .primary)
            .help(Self.title(surface.provider))
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

    static func title(_ provider: Provider) -> String {
        switch provider {
        case .codex: "Codex"
        case .claude, .unspecified: "Claude"
        }
    }

    /// How to sign in where the agent would run, which is where the
    /// provider keeps its credential.
    static func signInSteps(_ provider: Provider) -> String {
        switch provider {
        case .codex: "Run codex login there."
        case .claude, .unspecified: "Run claude and sign in with /login there."
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
            if signedIn(surface.provider) == false {
                Text("\(Self.title(surface.provider)) isn’t signed in on \(hostName). \(Self.signInSteps(surface.provider))")
            }
            if surface.machine != nil, surface.offer == .unavailable {
                Text("Couldn’t get \(Self.title(surface.provider))’s options from \(hostName). It’ll start with the defaults there.")
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
        let hostName = host.map { $0.local ? "this Mac" : PlaceNames.host($0.name) } ?? "this computer"
        ModelPicker(
            providers: [Provider.claude, .codex].map { provider in
                let signedIn = host?.providers.first { $0.provider == provider }?.signedIn
                return ModelPicker.ProviderItem(
                    provider: provider, name: NewAgentPage.title(provider),
                    glyph: provider == .codex ? "provider-codex" : "provider-claude",
                    note: signedIn == false ? "Not signed in on \(hostName)" : nil)
            },
            provider: surface.provider,
            settings: surface.offer == .offered ? surface.settings : nil,
            chooseProvider: { surface.choose(provider: $0) },
            chooseModel: { surface.choose(.model($0)) },
            chooseEffort: { surface.choose(.effort($0)) },
            chooseMode: { surface.choose(.mode($0)) })
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
                ForEach(surface.found, id: \.path) { row($0, recent: false) }
            } else if surface.listing != .asking {
                message("No matching folders.")
            }
        } else {
            if !surface.repositories.isEmpty {
                SectionTitle(text: "Repositories")
                ForEach(surface.repositories, id: \.path) { row($0, recent: false) }
            }
            if !surface.recent.isEmpty {
                SectionTitle(text: "Recent")
                ForEach(surface.recent, id: \.path) { row($0, recent: true) }
            }
            if surface.listing == .ready, surface.repositories.isEmpty, surface.recent.isEmpty {
                message("Add repositories in amux on this computer, or type a path.")
            }
        }
        switch surface.listing {
        case .asking: message("Loading repositories…")
        case .unavailable: message("This computer couldn’t list its repositories.")
        case .none, .ready: EmptyView()
        }
    }

    private func row(_ directory: Directory, recent: Bool) -> some View {
        Button {
            surface.choose(directory: directory.path)
            done()
        } label: {
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
                if surface.directory == directory.path {
                    Icon("check", size: 14).foregroundStyle(Palette.accent)
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .contentShape(Rectangle())
        }
        .buttonStyle(HoverButtonStyle())
    }

    private func message(_ text: String) -> some View {
        Text(text)
            .font(.ui(.meta))
            .foregroundStyle(Palette.textSecondary)
            .padding(10)
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
