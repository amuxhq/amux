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

    var starting: Bool { get }
    var failure: String? { get }
    var ready: Bool { get }
    func start()
}

/// Starting an agent: where, which agent with what, and its name, all on
/// one sheet, with the button naming the host it starts on.
public struct NewAgentSheet: View {
    let surface: any NewAgentSurface
    let cancel: @MainActor () -> Void
    @State private var browsing = false

    public init(surface: any NewAgentSurface, cancel: @escaping @MainActor () -> Void = {}) {
        self.surface = surface
        self.cancel = cancel
    }

    public var body: some View {
        VStack(spacing: 0) {
            Form {
                whereSection
                agentSection
                Section {
                    TextField("Name", text: Binding(get: { surface.name }, set: { surface.name = $0 }), prompt: Text("Optional"))
                } footer: {
                    note("Left empty, the host names it.")
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
            Rectangle().fill(Palette.line).frame(height: 1)
            foot
        }
        .frame(width: 540)
        .frame(minHeight: 520)
        .background(Palette.content)
        .onChange(of: chosenHost.map(HostWords.online), initial: true) { _, online in
            if let online { surface.reached(online) }
        }
    }

    // MARK: - Where

    private var chosenHost: HostView? {
        surface.hosts.first { $0.hostId == surface.machine }
    }

    private var hostName: String {
        chosenHost.map { $0.local ? "This Mac" : PlaceNames.host($0.name) } ?? "the host"
    }

    @ViewBuilder private var whereSection: some View {
        Section("Where") {
            Picker("Host", selection: Binding(get: { surface.machine ?? [] }, set: { surface.point(at: $0) })) {
                if surface.machine == nil { Text("Choose").tag([UInt8]()) }
                ForEach(surface.hosts, id: \.hostId) { host in
                    Text(hostTitle(host)).tag(host.hostId)
                        .selectionDisabled(!HostWords.online(host))
                }
            }
            LabeledContent("Directory") {
                HStack(spacing: 8) {
                    if !surface.directory.isEmpty {
                        Text(PlaceNames.path(surface.directory))
                            .font(.system(size: 12, design: .monospaced))
                            .lineLimit(1)
                            .truncationMode(.head)
                    }
                    Button(surface.directory.isEmpty ? "Choose…" : "Change…") { browsing = true }
                        .disabled(surface.machine == nil)
                        .popover(isPresented: $browsing, arrowEdge: .trailing) {
                            DirectoryChooser(surface: surface) { browsing = false }
                        }
                }
            }
            if !chips.isEmpty {
                FlowLayout(spacing: 6) {
                    ForEach(chips, id: \.path) { directory in
                        Button(directory.name) { surface.choose(directory: directory.path) }
                            .buttonStyle(ChipStyle())
                            .help(PlaceNames.path(directory.path))
                    }
                }
            }
            if surface.listing == .unavailable {
                note("\(hostName) could not list its repositories. Choose where its agents work, or type a path.")
            }
        }
    }

    /// The other directories this host was used in lately, then its
    /// repositories; the chosen one is in the row above.
    private var chips: [Directory] {
        Array(surface.likely.filter { $0.path != surface.directory }.prefix(5))
    }

    private func hostTitle(_ host: HostView) -> String {
        let name = host.local ? "This Mac" : PlaceNames.host(host.name)
        return HostWords.online(host) ? name : "\(name) — \(HostWords.reach(host))"
    }

    // MARK: - What runs there

    private func signedIn(_ provider: Provider) -> Bool? {
        chosenHost?.providers.first { $0.provider == provider }?.signedIn
    }

    @ViewBuilder private var agentSection: some View {
        Section("Agent") {
            Picker("Agent", selection: Binding(get: { surface.provider }, set: { surface.choose(provider: $0) })) {
                ForEach([Provider.claude, .codex], id: \.self) { provider in
                    Text(signedIn(provider) == false ? "\(Self.title(provider)) — not signed in" : Self.title(provider))
                        .tag(provider)
                }
            }
            if let settings = surface.settings, surface.offer == .offered {
                pickers(settings)
            }
            Toggle(isOn: Binding(get: { surface.newWorktree }, set: { surface.newWorktree = $0 })) {
                Text("New worktree")
                Text("A worktree of its own, made from the directory’s repository")
            }
            if signedIn(surface.provider) == false {
                note("\(Self.title(surface.provider)) is not signed in on \(hostName). Its agents can’t work there until it is.")
            }
            if surface.machine != nil {
                switch surface.offer {
                case .offered?: EmptyView()
                case .unavailable?:
                    note("\(hostName) could not say what \(Self.title(surface.provider)) offers. The agent starts with the host’s defaults.")
                case .asking?, nil:
                    note("Asking \(hostName) what \(Self.title(surface.provider)) offers…")
                }
            }
        }
    }

    @ViewBuilder private func pickers(_ settings: SettingsView) -> some View {
        if !settings.models.isEmpty {
            Picker("Model", selection: pick(settings.models.first(where: \.current)?.value) { .model($0) }) {
                ForEach(settings.models, id: \.value) { Text($0.displayName).tag(Optional($0.value)) }
            }
        }
        if settings.changeable.effort == .pick, !settings.efforts.isEmpty {
            Picker("Effort", selection: pick(settings.efforts.first(where: \.current)?.value) { .effort($0) }) {
                ForEach(settings.efforts, id: \.value) { Text($0.value).tag(Optional($0.value)) }
            }
        }
        if settings.changeable.permission == .pick, !settings.permissions.isEmpty {
            let current = settings.permissions.first(where: \.current)
            Picker("Permission", selection: pick(current?.value) { .permission($0) }) {
                ForEach(settings.permissions, id: \.value) { choice in
                    Text(ChatWords.permission(choice)).tag(Optional(choice.value))
                        .selectionDisabled(!choice.settable)
                }
            }
            if let current, current.neverAsks {
                Label("It acts without asking.", systemImage: "exclamationmark.triangle.fill")
                    .font(.system(size: 12))
                    .foregroundStyle(.red)
            }
        }
        if settings.changeable.mode == .pick, !settings.modes.isEmpty {
            Picker("Mode", selection: pick(settings.modes.first(where: \.current)?.value) { .mode($0) }) {
                ForEach(settings.modes, id: \.value) { Text(ChatWords.mode($0)).tag(Optional($0.value)) }
            }
        }
    }

    private func pick(_ current: String?, _ make: @escaping (String?) -> NewAgentPick) -> Binding<String?> {
        Binding(get: { current }, set: { surface.choose(make($0)) })
    }

    static func title(_ provider: Provider) -> String {
        switch provider {
        case .codex: "Codex"
        case .claude, .unspecified: "Claude"
        }
    }

    // MARK: - Starting it

    private var foot: some View {
        HStack(spacing: 10) {
            if let failure = surface.failure {
                Text(failure)
                    .font(.system(size: 12))
                    .foregroundStyle(.red)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
            Spacer(minLength: 12)
            Button("Cancel", action: cancel).keyboardShortcut(.cancelAction)
            Button(surface.starting ? "Starting…" : "Start on \(hostName)", action: surface.start)
                .keyboardShortcut(.defaultAction)
                .disabled(!surface.ready)
        }
        .controlSize(.large)
        .padding(16)
    }

    private func note(_ text: String) -> some View {
        Text(text)
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
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
                Image(systemName: recent ? "clock" : "folder")
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
                    Image(systemName: "checkmark").font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Color.accentColor)
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

/// A directory to start in, one click away.
private struct ChipStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 11.5, design: .monospaced))
            .padding(.horizontal, 9)
            .frame(height: 22)
            .background(Capsule().fill(configuration.isPressed ? Palette.line : Palette.hover))
            .overlay(Capsule().strokeBorder(Palette.line))
            .contentShape(Capsule())
    }
}
