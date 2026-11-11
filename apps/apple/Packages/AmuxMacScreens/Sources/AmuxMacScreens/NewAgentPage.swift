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

/// Starting an agent, as the sheet reads and changes it. The app adapts its
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

/// Starting an agent the way the references start a chat: an empty page
/// with a composer, the first message typed into it, and where and with
/// what it runs as chips under the field. Return starts it on the host the
/// first chip names and sends the message.
public struct NewAgentPage: View {
    let surface: any NewAgentSurface
    @FocusState private var focused: Bool
    @State private var browsing = false

    public init(surface: any NewAgentSurface) {
        self.surface = surface
    }

    public var body: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 40)
            Text(heading)
                .font(.system(size: 24, weight: .semibold))
                .multilineTextAlignment(.center)
                .padding(.bottom, 22)
            composer
            notes
                .padding(.top, 12)
                .padding(.horizontal, 6)
            Spacer(minLength: 40)
            Spacer(minLength: 0)
        }
        .frame(maxWidth: Metrics.reading)
        .padding(.horizontal, 32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear { focused = true }
        .onChange(of: chosenHost.map(HostWords.online), initial: true) { _, online in
            if let online { surface.reached(online) }
        }
    }

    private var heading: String {
        let folder = (surface.directory as NSString).lastPathComponent
        return folder.isEmpty ? "What should an agent work on?" : "What should an agent do in \(folder)?"
    }

    private var composer: some View {
        VStack(alignment: .leading, spacing: 0) {
            TextField(
                "Describe the work. Return starts it on \(hostName).",
                text: Binding(get: { surface.prompt }, set: { surface.prompt = $0 }), axis: .vertical
            )
            .textFieldStyle(.plain)
            .font(.system(size: 14))
            .lineLimit(3...14)
            .focused($focused)
            .padding(.horizontal, 16)
            .padding(.top, 14)
            .padding(.bottom, 8)
            .onKeyPress(.return, phases: .down) { press in
                if press.modifiers.contains(.shift) || press.modifiers.contains(.option) {
                    surface.prompt += "\n"
                    return .handled
                }
                if surface.ready { surface.start() }
                return .handled
            }
            HStack(spacing: 2) {
                hostMenu
                folderButton
                agentMenu
                permissionMenu
                Spacer(minLength: 8)
                Button {
                    surface.start()
                } label: {
                    Group {
                        if surface.starting {
                            ProgressView().controlSize(.small)
                        } else {
                            Icon("arrow-up", size: 15)
                                .foregroundStyle(
                                    surface.ready ? Color(nsColor: .textBackgroundColor) : Color.secondary)
                        }
                    }
                    .frame(width: 28, height: 28)
                    .background(Circle().fill(surface.ready ? Color.primary : Color.primary.opacity(0.1)))
                }
                .buttonStyle(.plain)
                .disabled(!surface.ready)
                .help("Start on \(hostName) (Return)")
            }
            .padding(.horizontal, 8)
            .padding(.bottom, 8)
        }
        .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(Palette.raised))
        .overlay(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .strokeBorder(focused ? Color.primary.opacity(0.16) : Palette.line))
        .shadow(color: .black.opacity(0.06), radius: 10, y: 3)
    }

    // MARK: - Where

    private var chosenHost: HostView? {
        surface.hosts.first { $0.hostId == surface.machine }
    }

    private var hostName: String {
        chosenHost.map { $0.local ? "this Mac" : PlaceNames.host($0.name) } ?? "a host"
    }

    private var hostMenu: some View {
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
            Pick(icon: chosenHost.map(HostWords.lucide) ?? "monitor",
                 text: chosenHost.map { $0.local ? "This Mac" : PlaceNames.host($0.name) } ?? "Choose a host")
        }
        .menuStyle(.button)
        .buttonStyle(HoverButtonStyle())
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Where it runs")
    }

    private var folderButton: some View {
        Button {
            browsing = true
        } label: {
            Pick(icon: "folder", text: folder)
        }
        .buttonStyle(HoverButtonStyle())
        .disabled(surface.machine == nil)
        .help(surface.directory.isEmpty ? "Choose its folder" : PlaceNames.path(surface.directory))
        .popover(isPresented: $browsing, arrowEdge: .bottom) {
            DirectoryChooser(surface: surface) { browsing = false }
        }
    }

    private var folder: String {
        let name = (surface.directory as NSString).lastPathComponent
        return name.isEmpty ? "Choose a folder" : name
    }

    // MARK: - What runs there

    private func signedIn(_ provider: Provider) -> Bool? {
        chosenHost?.providers.first { $0.provider == provider }?.signedIn
    }

    /// The agent, and the model and effort it starts with.
    private var agentMenu: some View {
        Menu {
            Section("Agent") {
                ForEach([Provider.claude, .codex], id: \.self) { provider in
                    Toggle(
                        isOn: Binding(get: { surface.provider == provider }, set: { _ in surface.choose(provider: provider) })
                    ) {
                        Text(Self.title(provider))
                        if signedIn(provider) == false { Text("Not signed in on \(hostName)") }
                    }
                }
            }
            if let settings = surface.settings, surface.offer == .offered {
                if !settings.models.isEmpty {
                    Section("Model") {
                        ForEach(settings.models, id: \.value) { choice in
                            Toggle(choice.displayName, isOn: Binding(
                                get: { choice.current }, set: { _ in surface.choose(.model(choice.value)) }))
                        }
                    }
                }
                if settings.changeable.effort == .pick, !settings.efforts.isEmpty {
                    Section("Effort") {
                        ForEach(settings.efforts, id: \.value) { choice in
                            Toggle(choice.value.capitalized, isOn: Binding(
                                get: { choice.current }, set: { _ in surface.choose(.effort(choice.value)) }))
                        }
                    }
                }
                if settings.changeable.mode == .pick, !settings.modes.isEmpty {
                    Section("Mode") {
                        ForEach(settings.modes, id: \.value) { choice in
                            Toggle(ChatWords.mode(choice), isOn: Binding(
                                get: { choice.current }, set: { _ in surface.choose(.mode(choice.value)) }))
                        }
                    }
                }
            }
            Divider()
            Toggle("New Worktree", isOn: Binding(get: { surface.newWorktree }, set: { surface.newWorktree = $0 }))
        } label: {
            Pick(icon: nil, text: agentWords)
        }
        .menuStyle(.button)
        .buttonStyle(HoverButtonStyle())
        .menuIndicator(.hidden)
        .fixedSize()
        .help("The agent and its model")
    }

    /// "Claude · Opus High", and "· worktree" when it gets one.
    private var agentWords: String {
        var words = [Self.title(surface.provider)]
        if let settings = surface.settings, surface.offer == .offered {
            let model = settings.models.first(where: \.current)?.displayName
            let effort = settings.changeable.effort == .pick
                ? settings.efforts.first(where: \.current)?.value.capitalized : nil
            let detail = [model, effort].compactMap { $0 }.joined(separator: " ")
            if !detail.isEmpty { words.append(detail) }
        }
        if surface.newWorktree { words.append("worktree") }
        return words.joined(separator: " · ")
    }

    @ViewBuilder private var permissionMenu: some View {
        if let settings = surface.settings, surface.offer == .offered, settings.changeable.permission == .pick,
            let current = settings.permissions.first(where: \.current)
        {
            Menu {
                ForEach(settings.permissions.filter(\.settable), id: \.value) { choice in
                    Toggle(isOn: Binding(get: { choice.current }, set: { _ in surface.choose(.permission(choice.value)) })) {
                        Text(ChatWords.permission(choice))
                        if !ChatWords.permissionDetail(choice).isEmpty { Text(ChatWords.permissionDetail(choice)) }
                    }
                }
            } label: {
                Pick(
                    icon: current.neverAsks ? "circle-alert" : "hand", text: ChatWords.permission(current),
                    tint: current.neverAsks ? .orange : .secondary)
            }
            .menuStyle(.button)
            .buttonStyle(HoverButtonStyle())
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Permission")
        }
    }

    static func title(_ provider: Provider) -> String {
        switch provider {
        case .codex: "Codex"
        case .claude, .unspecified: "Claude"
        }
    }

    // MARK: - Said under it

    @ViewBuilder private var notes: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let failure = surface.failure {
                Label { Text(failure).textSelection(.enabled) } icon: { Icon("triangle-alert", size: 13) }
                    .foregroundStyle(.red)
            }
            if surface.listing == .unavailable {
                Text("\(hostName.capitalizedFirst) could not list its folders. Choose one, or type a path.")
            }
            if signedIn(surface.provider) == false {
                Text("\(Self.title(surface.provider)) is not signed in on \(hostName). Its agents can’t work there until it is.")
            }
            if surface.machine != nil, surface.offer == .unavailable {
                Text("\(hostName.capitalizedFirst) could not say what \(Self.title(surface.provider)) offers; the agent starts with the host’s defaults.")
            }
            if !recent.isEmpty, surface.failure == nil {
                HStack(spacing: 6) {
                    Text("Recent").foregroundStyle(.tertiary)
                    ForEach(recent, id: \.path) { directory in
                        Button(directory.name) { surface.choose(directory: directory.path) }
                            .buttonStyle(ChipStyle())
                            .help(PlaceNames.path(directory.path))
                    }
                }
            }
        }
        .font(.system(size: 12))
        .foregroundStyle(.secondary)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// Other folders the host was used in lately, the chosen one apart.
    private var recent: [Directory] {
        Array(surface.likely.filter { $0.path != surface.directory }.prefix(4))
    }
}

extension String {
    fileprivate var capitalizedFirst: String { prefix(1).uppercased() + dropFirst() }
}

/// A chip under the new-agent field: what it is set to, opening its menu.
private struct Pick: View {
    let icon: String?
    let text: String
    var tint: Color = .secondary

    var body: some View {
        HStack(spacing: 5) {
            if let icon { Icon(icon, size: 13) }
            Text(text).lineLimit(1)
            Icon("chevron-down", size: 11).opacity(0.7)
        }
        .font(.system(size: 12.5))
        .foregroundStyle(tint)
        .padding(.horizontal, 8)
        .frame(height: 28)
        .contentShape(Rectangle())
    }
}

/// The directories a host has to offer, searched as typed, and a field for
/// one it does not list.
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
                TextField("Or a path: ~/somewhere/else", text: Binding(get: { surface.typed }, set: { surface.typed = $0 }))
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 12, design: .monospaced))
                    .onSubmit(useTyped)
                Button("Use", action: useTyped)
                    .disabled(typedPath.isEmpty)
            }
            .padding(10)
        }
        .frame(width: 400)
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
                message("No matching folders on this host.")
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
                message("Add repositories in amux on this host, or type a path.")
            }
        }
        switch surface.listing {
        case .asking: message("Reading this host’s repositories…")
        case .unavailable: message("This host could not list its repositories.")
        case .none, .ready: EmptyView()
        }
    }

    private func row(_ directory: Directory, recent: Bool) -> some View {
        Button {
            surface.choose(directory: directory.path)
            done()
        } label: {
            HStack(spacing: 9) {
                Icon(recent ? "clock" : "folder", size: 14)
                    .foregroundStyle(.secondary)
                    .frame(width: 16)
                VStack(alignment: .leading, spacing: 1) {
                    Text(directory.name).font(.system(size: 13))
                    Text(PlaceNames.path(directory.path))
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
                Spacer(minLength: 6)
                if surface.directory == directory.path {
                    Icon("check", size: 13).foregroundStyle(Color.accentColor)
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
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            .padding(10)
    }
}

/// A folder to start in, one click away.
private struct ChipStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 8)
            .frame(height: 22)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(configuration.isPressed ? Palette.selection : Palette.well))
            .contentShape(Rectangle())
    }
}
