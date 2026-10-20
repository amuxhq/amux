import AmuxCore
import AmuxDesign
import SwiftUI

/// What somebody did on New Agent. Like every other screen it decides nothing
/// and reaches nothing: choosing a machine means asking that machine what it
/// has to offer, and starting an agent means a request leaving the phone, and
/// both of those belong to whoever owns the connection.
public enum NewAgentAction: Equatable, Sendable {
    /// Start on this machine instead. What the last machine offered goes with
    /// it — a directory on one machine names nothing on another.
    case point(HostId)
    /// Ask the machine again with what is being searched for. Only sent for a
    /// machine whose first answer stopped at the limit.
    case search
    /// Start it.
    case start
    /// Left without starting anything.
    case cancel
}

/// Starting an agent: one machine, one directory, one agent.
///
/// Raised as a sheet over the list, the way a new message is, and
/// everything is on it at once: nothing is a step. Which machine, where, and
/// what runs there are three answers to one question, and a person who has
/// changed their mind about the machine has usually changed their mind about
/// the directory as well — putting them on separate pages would mean walking
/// back through a wizard to say so. Where comes first, then the agent and
/// what it runs with, and the name last, since it fills itself in from the
/// directory.
///
/// The button at the foot names the machine. It is the last thing read before
/// something is started somewhere else, and "Start" alone would leave that
/// unstated on the one control where it matters.
public struct NewAgent: View {
    @Environment(\.design) private var design
    @Environment(\.reducesTransparency) private var reducesTransparency
    /// How tall the Start bar floating at the foot is, so the last rows can
    /// always scroll clear of it, at any text size.
    @State private var footHeight: CGFloat = 110
    private let model: NewAgentStore
    private let hosts: HostsStore
    private let actions: @MainActor (NewAgentAction) -> Void

    public init(
        model: NewAgentStore, hosts: HostsStore,
        actions: @escaping @MainActor (NewAgentAction) -> Void
    ) {
        self.model = model
        self.hosts = hosts
        self.actions = actions
    }

    public var body: some View {
        ZStack(alignment: .bottom) {
            Ground()
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    place
                    agent
                    naming
                }
                .padding(.horizontal, design.metrics.gutter)
                .padding(.top, 8)
                // Clear of the bar at the foot, which floats over this.
                .padding(.bottom, footHeight + 20)
            }
            .scrollIndicators(.hidden)
            .scrollDismissesKeyboard(.interactively)
            foot
        }
        .navigationTitle("New Agent")
        .toolbarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                BarButton("xmark", label: String(localized: "Cancel"), identifier: "new-agent.cancel") {
                    actions(.cancel)
                }
            }
            .sharedBackgroundVisibility(reducesTransparency ? .hidden : .automatic)
        }
        // The chooser is a sheet of its own over this one: what is being
        // chosen is one field of the thing being built, and the rest of it
        // stays underneath to come back to.
        .sheet(isPresented: Binding(get: { model.browsing }, set: { model.browsing = $0 })) {
            NewAgentDirectories(model: model, actions: actions)
                .presentationDetents([.medium, .large])
                .reportsHere()
        }
        .onChange(of: model.machine.flatMap { hosts.host($0)?.online }, initial: true) { _, online in
            if let online { model.machineReached(online) }
        }
        // A screen is a container of the things on it, not a name for all of
        // them. Without this the system spreads this identifier over every
        // element underneath — the title, the buttons, the rows — so
        // everything on the screen answers to the screen's own name, for
        // VoiceOver and for anything driving the app alike.
        .accessibilityElement(children: .contain)
        .identified("new-agent", value: model.directory)
    }

    private var rule: some View {
        Rectangle()
            .fill(design.hairline.color)
            .frame(height: design.metrics.hairline)
    }

    // MARK: - Where

    /// The machine and the directory on it, with the directories it was used
    /// in most recently as chips under them: the answer is almost always
    /// somewhere this machine has been used before, and the whole list is one
    /// tap away for the times it is not.
    private var place: some View {
        VStack(alignment: .leading, spacing: 9) {
            SectionHead(title: "Where")
            if hosts.hosts.isEmpty {
                Explain("Pair a host before starting an agent.")
                    .identified("new-agent.no-hosts")
            } else {
                Surface {
                    VStack(spacing: 0) {
                        machine
                        rule
                        directory
                    }
                }
                if !chips.isEmpty { recent }
            }
            if model.listing == .unavailable {
                Explain("\(machineName) could not list its repositories. Choose where its agents work, or type a path.")
                    .identified("new-agent.directory.unavailable")
            }
        }
    }

    /// The machines this phone is paired with, in a menu, whether or not they
    /// are answering.
    ///
    /// An unreachable one is listed and cannot be chosen. Leaving it out
    /// would be this screen deciding that a machine somebody paired with does
    /// not exist; listing it greyed, with why under its name, says the true
    /// thing, which is that it is away and nothing can be started on it until
    /// it is back.
    private var machine: some View {
        let chosen = model.machine.flatMap { hosts.host($0) }
        let items = hosts.hosts.map { host in
            MenuItem(
                title: host.name, subtitle: reach(host), chosen: model.machine == host.id,
                disabled: !host.online
            ) { if let id = host.id { actions(.point(id)) } }
        }
        let value = chosen?.name ?? String(localized: "Choose")
        return MenuButton(
            name: String(localized: "Host, \(value)"), identifier: "new-agent.host", items: items,
            value: chosen?.name
        ) {
            FieldRow(label: String(localized: "Host"), value: value)
        }
    }

    /// "macOS · via relay", or why the machine cannot be chosen.
    private func reach(_ host: HostView) -> String {
        switch host.group {
        case .away: return String(localized: "Away")
        case .offline: return String(localized: "Offline")
        case .onThisNetwork, .throughTheRelay:
            let route = host.group == .onThisNetwork
                ? String(localized: "direct") : String(localized: "via relay")
            return [host.platform, route].compactMap { $0 }.joined(separator: " · ")
        }
    }

    private var directory: some View {
        Button { model.browsing = true } label: {
            FieldRow(
                label: String(localized: "Directory"),
                value: model.directory.isEmpty ? String(localized: "Choose") : model.directory,
                mono: !model.directory.isEmpty)
        }
        .buttonStyle(.amuxRow)
        .disabled(model.machine == nil)
        .accessibilityLabel("Directory")
        .identified(
            "new-agent.directory", label: "Directory",
            value: model.directory, enabled: model.machine != nil)
    }

    /// The other directories this machine was used in recently — the chosen one
    /// is in the row above and is not repeated as a chip.
    private var chips: [Directory] {
        model.recent.filter { $0.path != model.directory }.prefix(4).map { $0 }
    }

    private var recent: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 7) {
                ForEach(chips) { project in
                    Button { model.choose(directory: project.path) } label: {
                        Text(project.name)
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.ink.color)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 6)
                            .background {
                                Capsule().fill(design.sunken.color)
                                    .overlay(Capsule().strokeBorder(
                                        design.hairline.color, lineWidth: 1))
                            }
                            .thumbTarget(y: 8)
                    }
                    .buttonStyle(.amuxControl)
                    .accessibilityLabel("Start in \(project.name)")
                    .identified(
                        "new-agent.recent.\(project.name)", label: "Start in \(project.name)",
                        value: project.path)
                    .reclaimingThumbTarget(y: 8)
                }
            }
            .padding(.vertical, 1)
        }
        .scrollIndicators(.hidden)
    }

    // MARK: - What runs there

    /// Whether the chosen host has said this provider is signed in there;
    /// nil until it says.
    private func signedIn(_ provider: NewAgentStore.Provider) -> Bool? {
        guard let machine = model.machine else { return nil }
        return hosts.host(machine)?.providers.first { $0.provider == provider.wire }?.signedIn
    }

    /// Which agent, and the model, effort, permission and mode it starts
    /// with, from what the host says its provider offers, and the
    /// new-worktree switch. The agent heads the group because it decides what
    /// the rows under it offer.
    private var agent: some View {
        VStack(alignment: .leading, spacing: 9) {
            SectionHead(title: "Agent")
            Surface {
                VStack(spacing: 0) {
                    providerPicker
                    ForEach(Array(pickers.enumerated()), id: \.offset) { _, picker in
                        rule
                        picker
                    }
                    rule
                    worktree
                }
            }
            if signedIn(model.provider) == false {
                Explain("\(model.provider.title) is not signed in on \(machineName). Its agents can’t work there until it is.")
                    .identified("new-agent.signed-out", value: model.provider.rawValue)
            }
            if model.machine != nil {
                switch model.offers[model.provider] {
                case .offered?:
                    EmptyView()
                case .unavailable?:
                    Explain("\(machineName) could not say what \(model.provider.title) offers. The agent starts with the host’s defaults.")
                        .identified("new-agent.offer.unavailable")
                case .asking?, nil:
                    Explain("Asking \(machineName) what \(model.provider.title) offers…")
                        .identified("new-agent.offer.asking")
                }
            }
        }
    }

    /// A menu rather than a control with a place for each: more agents than
    /// two are coming, and a menu row says which one is not signed in here.
    private var providerPicker: some View {
        let items = NewAgentStore.Provider.allCases.map { provider in
            MenuItem(
                title: provider.title,
                subtitle: signedIn(provider) == false
                    ? String(localized: "Not signed in on \(machineName)") : nil,
                chosen: model.provider == provider
            ) { model.choose(provider: provider) }
        }
        return MenuButton(
            name: String(localized: "Agent, \(model.provider.title)"), identifier: "new-agent.provider",
            items: items, value: model.provider.rawValue
        ) {
            FieldRow(label: String(localized: "Agent"), value: model.provider.title)
        }
    }

    private var pickers: [AnyView] {
        guard case .offered? = model.offers[model.provider], let view = model.settings else { return [] }
        var rows = [AnyView(modelPicker(view))]
        if view.changeable.effort == .pick { rows.append(AnyView(effortPicker(view))) }
        if view.changeable.permission == .pick { rows.append(AnyView(permissionPicker(view))) }
        if view.changeable.mode == .pick { rows.append(AnyView(modePicker(view))) }
        return rows
    }

    private var hostDefault: String { String(localized: "Host default") }

    private func modelPicker(_ view: SettingsView) -> some View {
        let current = view.models.first(where: \.current)?.displayName ?? hostDefault
        let items = [MenuItem(title: hostDefault, chosen: model.model == nil) {
            model.choose(.model(nil))
        }] + view.models.map { offered in
            MenuItem(title: offered.displayName, chosen: offered.current) {
                model.choose(.model(offered.value))
            }
        }
        return picker(String(localized: "Model"), id: "new-agent.model", current: current, items: items)
    }

    private func effortPicker(_ view: SettingsView) -> some View {
        let worded = { (effort: EffortChoice) in
            effort.default ? String(localized: "\(effort.value) (default)") : effort.value
        }
        let items = [MenuItem(
            title: String(localized: "Model default"), chosen: model.effort == nil
        ) { model.choose(.effort(nil)) }] + view.efforts.map { effort in
            MenuItem(title: worded(effort), chosen: effort.current) {
                model.choose(.effort(effort.value))
            }
        }
        return picker(
            String(localized: "Effort"), id: "new-agent.effort",
            current: model.effort ?? String(localized: "Model default"), items: items)
    }

    private func permissionPicker(_ view: SettingsView) -> some View {
        let current = view.permissions.first(where: \.current).map(ChatWords.permission) ?? hostDefault
        // One that acts without asking reads red, as on a chat's settings.
        let items = [MenuItem(title: hostDefault, chosen: model.permission == nil) {
            model.choose(.permission(nil))
        }] + view.permissions.map { permission in
            MenuItem(
                title: ChatWords.permission(permission),
                destructive: permission.neverAsks, chosen: permission.current
            ) { model.choose(.permission(permission.value)) }
        }
        return picker(
            String(localized: "Permission"), id: "new-agent.permission", current: current, items: items)
    }

    private func modePicker(_ view: SettingsView) -> some View {
        let items = view.modes.map { mode in
            MenuItem(title: ChatWords.mode(mode), chosen: mode.current) {
                model.choose(.mode(mode.value))
            }
        }
        return picker(
            String(localized: "Mode"), id: "new-agent.mode",
            current: view.modes.first(where: \.current).map(ChatWords.mode) ?? hostDefault, items: items)
    }

    private func picker(_ label: String, id: String, current: String, items: [MenuItem]) -> some View {
        MenuButton(name: "\(label), \(current)", identifier: id, items: items, value: current) {
            FieldRow(label: label, value: current)
        }
    }

    private var worktree: some View {
        Toggle(isOn: Binding(get: { model.newWorktree }, set: { model.newWorktree = $0 })) {
            VStack(alignment: .leading, spacing: 2) {
                Text("New worktree")
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                Text("A worktree of its own, made from the directory’s repository")
                    .designFont(.detail, design)
                    .foregroundStyle(design.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .tint(design.accent.color)
        .oneSwitch(
            String(localized: "New worktree, A worktree of its own, made from the directory’s repository"),
            isOn: Binding(get: { model.newWorktree }, set: { model.newWorktree = $0 }))
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: 44)
        .identified("new-agent.worktree", label: "New worktree", value: model.newWorktree ? "on" : "off")
    }

    // MARK: - What it is called

    /// The name, standing in the field as the suggestion until it is changed.
    ///
    /// Filled rather than left as a placeholder: the suggestion is what will
    /// be sent if nothing is typed, and a grey hint would suggest the field
    /// was empty. The machine still has the last word, and a name it refuses
    /// comes back as its own sentence under the host.
    private var naming: some View {
        Surface {
            HStack(spacing: 10) {
                Text("Name")
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                TextField(
                    "Name",
                    text: Binding(get: { model.name }, set: { model.choose(name: $0) }))
                    .textFieldStyle(.plain)
                    .multilineTextAlignment(.trailing)
                    .designFont(.mono, design)
                    .foregroundStyle(design.inkMuted.color)
                    .tint(design.accentColor)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.done)
                    .identified("new-agent.name", label: "Name", value: model.name)
            }
            .padding(.horizontal, 14)
            .frame(minHeight: 48)
        }
    }

    // MARK: - Starting it

    private var machineName: String {
        model.machine.flatMap { hosts.host($0)?.name } ?? "the host"
    }

    private var foot: some View {
        BottomAction {
            // Over the button that was pressed, in the machine's own words, as
            // pairing's refusal is said beside the code it refused.
            if let failure = model.failure {
                Text(failure)
                    .designFont(.detail, design)
                    .foregroundStyle(design.removed.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, Space.xs)
                    .padding(.bottom, 10)
                    .identified("new-agent.refusal", value: failure)
            }
            Button { actions(.start) } label: {
                ActionLabel(
                    model.starting ? "Starting…" : "Start on \(machineName)", fill: true)
            }
            .buttonStyle(.amuxControl)
            .disabled(!model.ready)
            .identified(
                "new-agent.start", label: "Start on \(machineName)",
                value: model.starting ? "starting" : "ready", enabled: model.ready)
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { footHeight = $0 }
    }
}

/// The directories a machine has to offer, and a field for one it does not.
///
/// The typed path is under the list rather than instead of it. A machine that
/// would not enumerate anything still runs agents, and a directory that is not
/// a repository is an ordinary thing to want; the list is the shortcut and the
/// field is the answer to everything it does not cover.
public struct NewAgentDirectories: View {
    @Environment(\.design) private var design
    @Environment(\.reducesTransparency) private var reducesTransparency
    let model: NewAgentStore
    let actions: @MainActor (NewAgentAction) -> Void

    public init(model: NewAgentStore, actions: @escaping @MainActor (NewAgentAction) -> Void) {
        self.model = model
        self.actions = actions
    }

    public var body: some View {
        NavigationStack {
            ZStack {
                Ground()
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        search
                        listing
                        path
                    }
                    .padding(.horizontal, design.metrics.gutter)
                    .padding(.vertical, 8)
                }
                .scrollDismissesKeyboard(.interactively)
            }
            .navigationTitle("Directory")
            .toolbarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    BarButton("checkmark", label: String(localized: "Done"), identifier: "new-agent.browse.done") {
                        model.browsing = false
                    }
                }
                .sharedBackgroundVisibility(reducesTransparency ? .hidden : .automatic)
            }
        }
        .tint(design.accentColor)
        .accessibilityElement(children: .contain)
        .identified("new-agent.browse", value: model.query)
    }

    /// Always in sight rather than pulled down from under the title: in a
    /// chooser, searching is most of what is done.
    @ViewBuilder
    private var search: some View {
        if model.listing != .unavailable {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 15, weight: .medium))
                    .foregroundStyle(design.inkMuted.color)
                TextField("Search repositories", text: Binding(
                    get: { model.query },
                    set: { typed in
                        model.query = typed
                        actions(.search)
                    }))
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.search)
            }
            .padding(.horizontal, 12)
            .frame(minHeight: 40)
            .background(Capsule().fill(design.sunken.color))
            .identified("new-agent.search", label: "Search repositories", value: model.query)
        }
    }

    @ViewBuilder
    private var listing: some View {
        VStack(alignment: .leading, spacing: 12) {
            if !model.recent.isEmpty && model.query.isEmpty {
                SectionHead(title: "Recent")
                RowGroup(items: model.recent) { project in
                    row(project, recent: true)
                }
            }
            switch model.listing {
            case .asking:
                Explain("Reading this host’s projects…")
                    .identified("new-agent.browse.reading")
            case .unavailable:
                Explain("This host could not list its repositories.")
                    .identified("new-agent.browse.unavailable")
            case .none, .ready:
                if !model.found.isEmpty {
                    SectionHead(title: model.query.isEmpty ? "Repositories" : "Found")
                    RowGroup(items: model.found) { project in
                        row(project, recent: false)
                    }
                } else if model.listing == .ready {
                    Explain(nothing)
                        .identified("new-agent.browse.none")
                }
            }
        }
    }

    /// What "no repositories" means here: only useful beside where the machine
    /// looked, which is the roots it reported.
    private var nothing: String {
        guard model.query.isEmpty else { return "No matching repositories on this host." }
        guard !model.roots.isEmpty else { return "This host listed no repositories." }
        return "No repositories under \(model.roots.joined(separator: ", "))."
    }

    private func row(_ project: Directory, recent: Bool) -> some View {
        Button {
            model.choose(directory: project.path)
            model.browsing = false
        } label: {
            HStack(spacing: 11) {
                Image(systemName: recent ? "clock" : "folder")
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(design.inkFaint.color)
                    .frame(width: 20)
                VStack(alignment: .leading, spacing: 2) {
                    Text(project.name)
                        .designFont(.identifier, design)
                        .foregroundStyle(design.ink.color)
                    Text(project.path)
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
                Spacer(minLength: 6)
                if model.directory == project.path {
                    Image(systemName: "checkmark")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(design.accent.color)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .frame(minHeight: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxRow)
        .accessibilityLabel(project.path)
        .identified(
            "new-agent.project.\(project.name)", label: project.path,
            value: model.directory == project.path ? "chosen" : "")
    }

    /// A path typed by hand, for a directory the machine did not list. The
    /// machine has the last word on whether it exists — this phone has no way
    /// to know — so it is taken as written and the refusal, if there is one,
    /// comes back from the machine in its own words.
    private var path: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "Or a path")
            HStack(spacing: 8) {
                TextField("~/somewhere/else", text: Binding(
                    get: { model.typed }, set: { model.typed = $0 }))
                    .designFont(.mono, design)
                    .foregroundStyle(design.ink.color)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                    .background {
                        RoundedRectangle(
                            cornerRadius: design.metrics.controlRadius, style: .continuous)
                            .fill(design.sunken.color)
                    }
                    .identified("new-agent.typed", label: "Or a path", value: model.typed)
                Button {
                    model.choose(directory: model.typedPath)
                    model.browsing = false
                } label: {
                    ActionLabel("Use", kind: .outline)
                }
                .buttonStyle(.amuxControl)
                .disabled(model.typedPath.isEmpty)
                .identified(
                    "new-agent.typed.use", label: "Use This Path",
                    enabled: !model.typedPath.isEmpty)
            }
        }
    }
}
