import AmuxCore
import AmuxDesign
import AmuxWords
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
    /// Keep a folder among the machine's repositories, or let it go.
    case pin(Directory, Bool)
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
    @FocusState private var naming: Bool
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
            ScrollViewReader { scroll in
                ScrollView {
                    VStack(alignment: .leading, spacing: 22) {
                        place
                        agent
                        name.id(Self.nameRow)
                    }
                    .padding(.horizontal, design.metrics.gutter)
                    .padding(.top, 8)
                    .padding(.bottom, 20)
                }
                .scrollIndicators(.hidden)
                .scrollDismissesKeyboard(.interactively)
                // The bar at the foot floats over the list as an inset of
                // the scroll view rather than over it, so what scrolls into
                // view comes to rest above the bar, not under it.
                .safeAreaInset(edge: .bottom, spacing: 0) { foot }
                // The system brings only the text field's own line above
                // the keyboard; the whole row, the last on the page, comes
                // up with it, so its card is not cut by the bar.
                .onChange(of: naming) { _, focused in
                    guard focused else { return }
                    withAnimation { scroll.scrollTo(Self.nameRow, anchor: .bottom) }
                }
            }
        }
        .navigationTitle("New Agent")
        .toolbarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                BarButton("xmark", label: String(localized: "Cancel"), identifier: "new-agent.cancel") {
                    actions(.cancel)
                }
            }
        }
        // The chooser is a sheet of its own over this one: what is being
        // chosen is one field of the thing being built, and the rest of it
        // stays underneath to come back to.
        .sheet(isPresented: Binding(get: { model.browsing }, set: { model.browsing = $0 })) {
            NewAgentDirectories(model: model, actions: actions)
                .presentationDetents([.medium, .large])
                .surface()
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
                Explain("Pair a device before starting an agent.")
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
            name: String(localized: "Device, \(value)"), identifier: "new-agent.host", items: items,
            value: chosen?.name
        ) {
            FieldRow(label: String(localized: "Device"), value: value)
        }
    }

    /// "macOS · via relay", or why the computer cannot be chosen.
    private func reach(_ host: HostView) -> String {
        switch host.group {
        case .away: return String(localized: "Away")
        case .offline:
            return hosts.isConnecting(host.id) ? String(localized: "Connecting") : String(localized: "Offline")
        case .onThisNetwork, .throughTheRelay:
            let route = host.group != .onThisNetwork ? String(localized: "Relayed")
                : host.sameNetwork ? String(localized: "Local") : String(localized: "Direct")
            return [host.platform?.name, route].compactMap { $0 }.joined(separator: " · ")
        }
    }

    private var directory: some View {
        Button { model.browsing = true } label: {
            FieldRow(
                label: String(localized: "Directory"),
                value: model.directory.isEmpty
                    ? String(localized: "Choose") : PlaceNames.path(model.directory),
                mono: !model.directory.isEmpty)
        }
        .buttonStyle(.amuxRow)
        .disabled(model.machine == nil)
        .accessibilityLabel("Directory")
        .identified(
            "new-agent.directory", label: "Directory",
            value: model.directory, enabled: model.machine != nil)
    }

    /// The other directories this machine was used in recently, then its
    /// repositories — the chosen one is in the row above and is not repeated as
    /// a chip.
    private var chips: [Directory] {
        model.likely.filter { $0.path != model.directory }.prefix(4).map { $0 }
    }

    private var recent: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 7) {
                ForEach(chips) { repository in
                    Button { model.choose(directory: repository.path) } label: {
                        Text(repository.name)
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
                    .accessibilityLabel("Start in \(repository.name)")
                    .identified(
                        "new-agent.recent.\(repository.name)", label: "Start in \(repository.name)",
                        value: repository.path)
                    .reclaimingThumbTarget(y: 8)
                }
            }
            .padding(.vertical, 1)
        }
        .scrollIndicators(.hidden)
    }

    // MARK: - What runs there

    /// Which agent, and the model, effort, permission and mode it starts
    /// with, from what the host says that agent offers, and the
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
            if let chosen = model.chosenAgent, chosen.signedIn == false {
                // In the agent's own words for what to do, on the computer
                // where it keeps its credential.
                Explain("\(chosen.name) is not signed in on \(machineName). Its agents can’t work there until it is. \(ChatWords.signInStepsThere(chosen.signInHint))")
                    .identified("new-agent.signed-out", value: chosen.provider)
            }
            if model.machine != nil, let chosen = model.chosenAgent {
                switch model.offer {
                case .offered(let catalogue)? where catalogue.models.isEmpty:
                    // Nothing listed yet, as for an agent that names its
                    // models only once a conversation has started there.
                    Explain("\(chosen.name) starts on its own model and settings.")
                        .identified("new-agent.offer.own")
                case .offered?:
                    EmptyView()
                case .unavailable?:
                    Explain("\(machineName) could not say what \(chosen.name) offers. The agent starts with the device’s defaults.")
                        .identified("new-agent.offer.unavailable")
                case .asking?, nil:
                    Explain("Asking \(machineName) what \(chosen.name) offers…")
                        .identified("new-agent.offer.asking")
                }
            } else if model.machine != nil {
                Explain("Asking \(machineName) which agents it runs…")
                    .identified("new-agent.agents.asking")
            }
        }
    }

    /// A menu rather than a control with a place for each: a host offers as
    /// many agents as its person set up, and a menu row says which one is
    /// not signed in here.
    private var providerPicker: some View {
        let items = model.agents.map { offered in
            MenuItem(
                title: offered.name,
                subtitle: offered.signedIn == false
                    ? String(localized: "Not signed in on \(machineName)") : nil,
                chosen: model.provider == offered.provider
            ) { model.choose(provider: offered.provider) }
        }
        let name = model.chosenAgent?.name ?? String(localized: "Choose")
        return MenuButton(
            name: String(localized: "Agent, \(name)"), identifier: "new-agent.provider",
            items: items, value: model.provider
        ) {
            FieldRow(label: String(localized: "Agent"), value: name)
        }
    }

    /// A row for each setting the agent takes a pick of at start; none when
    /// it lists nothing to pick from, and it starts on its own defaults.
    private var pickers: [AnyView] {
        guard case .offered? = model.offer, let view = model.settings else { return [] }
        var rows: [AnyView] = []
        if view.changeable.model == .pick, !view.models.isEmpty { rows.append(AnyView(modelPicker(view))) }
        if view.changeable.effort == .pick { rows.append(AnyView(effortPicker(view))) }
        if view.changeable.permission == .pick { rows.append(AnyView(permissionPicker(view))) }
        if view.changeable.mode == .pick { rows.append(AnyView(modePicker(view))) }
        return rows
    }

    /// What a setting reads when the host cannot name it.
    private var unnamed: String { "…" }

    private func modelPicker(_ view: SettingsView) -> some View {
        let current = view.models.first(where: \.current)?.displayName ?? unnamed
        let items = view.models.map { offered in
            MenuItem(title: offered.displayName, chosen: offered.current) {
                model.choose(.model(offered.value))
            }
        }
        return picker(String(localized: "Model"), id: "new-agent.model", current: current, items: items)
    }

    private func effortPicker(_ view: SettingsView) -> some View {
        let items = view.efforts.map { effort in
            MenuItem(title: effort.value, chosen: effort.current) {
                model.choose(.effort(effort.value))
            }
        }
        return picker(
            String(localized: "Effort"), id: "new-agent.effort",
            current: view.efforts.first(where: \.current)?.value ?? unnamed, items: items)
    }

    private func permissionPicker(_ view: SettingsView) -> some View {
        let current = view.permissions.first(where: \.current).map(ChatWords.permission) ?? unnamed
        // One that acts without asking reads red, as on a chat's settings.
        let items = view.permissions.map { permission in
            MenuItem(
                title: ChatWords.permission(permission),
                destructive: permission.neverAsks, chosen: permission.current
            ) { model.choose(.permission(permission.value)) }
        }
        return picker(
            String(localized: "Permission"), id: "new-agent.permission", current: current, items: items)
    }

    /// With none marked in force, as for an agent whose modes name no
    /// normal one, nothing is picked and the agent starts in the mode its
    /// own settings name.
    private func modePicker(_ view: SettingsView) -> some View {
        var items = view.modes.map { mode in
            MenuItem(title: ChatWords.mode(mode), chosen: mode.current) {
                model.choose(.mode(mode.value))
            }
        }
        if !view.modes.contains(where: \.normal) {
            items.insert(
                MenuItem(title: ChatWords.agentsOwnMode, chosen: !view.modes.contains(where: \.current)) {
                    model.choose(.mode(nil))
                }, at: 0)
        }
        return picker(
            String(localized: "Mode"), id: "new-agent.mode",
            current: view.modes.first(where: \.current).map(ChatWords.mode) ?? ChatWords.agentsOwnMode,
            items: items)
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

    private static let nameRow = "new-agent.name-row"

    /// The name, standing in the field as the suggestion until it is changed.
    ///
    /// Filled rather than left as a placeholder: the suggestion is what will
    /// be sent if nothing is typed, and a grey hint would suggest the field
    /// was empty. The machine still has the last word, and a name it refuses
    /// comes back as its own sentence under the host.
    private var name: some View {
        Surface {
            HStack(spacing: 10) {
                Text("Name")
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                TextField(
                    "Optional",
                    text: Binding(get: { model.name }, set: { model.choose(name: $0) }))
                    .textFieldStyle(.plain)
                    .multilineTextAlignment(.trailing)
                    .designFont(.mono, design)
                    .foregroundStyle(design.inkMuted.color)
                    .tint(design.accentColor)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.done)
                    .focused($naming)
                    .identified("new-agent.name", label: "Name", value: model.name)
            }
            .padding(.horizontal, 14)
            .frame(minHeight: 48)
        }
    }

    // MARK: - Starting it

    private var machineName: String {
        model.machine.flatMap { hosts.host($0)?.name } ?? "the device"
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
                TextField("Search folders", text: Binding(
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
            .identified("new-agent.search", label: "Search folders", value: model.query)
        }
    }

    /// The host's repositories first, as its person chose and ordered them,
    /// then where its agents worked lately; a search lists what matches
    /// either in their place.
    @ViewBuilder
    private var listing: some View {
        VStack(alignment: .leading, spacing: 12) {
            if model.searching {
                if !model.found.isEmpty {
                    SectionHead(title: "Found")
                    RowGroup(items: model.found) { repository in
                        row(repository, recent: false)
                    }
                } else if model.listing != .asking {
                    Explain("No matching folders on this device.")
                        .identified("new-agent.browse.none")
                }
            } else {
                if !model.repositories.isEmpty {
                    SectionHead(title: "Repositories")
                    RowGroup(items: model.repositories) { repository in
                        row(repository, recent: false)
                    }
                }
                if !model.recent.isEmpty {
                    SectionHead(title: "Recent")
                    RowGroup(items: model.recent) { repository in
                        row(repository, recent: true)
                    }
                }
                if !model.history.isEmpty {
                    SectionHead(title: "From Claude Code and Codex")
                    RowGroup(items: model.history) { repository in
                        row(repository, recent: true)
                    }
                }
                if model.listing == .ready && model.repositories.isEmpty && model.recent.isEmpty
                    && model.history.isEmpty
                {
                    Explain("Nothing has started on this device yet. Type a path below.")
                        .identified("new-agent.browse.none")
                }
            }
            switch model.listing {
            case .asking:
                Explain("Reading this device’s repositories…")
                    .identified("new-agent.browse.reading")
            case .unavailable:
                Explain("This device could not list its repositories.")
                    .identified("new-agent.browse.unavailable")
            case .none, .ready:
                EmptyView()
            }
        }
    }

    private func row(_ repository: Directory, recent: Bool) -> some View {
        HStack(spacing: 0) {
            choice(repository, recent: recent)
            pin(repository)
        }
    }

    private func choice(_ repository: Directory, recent: Bool) -> some View {
        Button {
            model.choose(directory: repository.path)
            model.browsing = false
        } label: {
            HStack(spacing: 11) {
                Image(systemName: recent ? "clock" : "folder")
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(design.inkFaint.color)
                    .frame(width: 20)
                VStack(alignment: .leading, spacing: 2) {
                    Text(repository.name)
                        .designFont(.identifier, design)
                        .foregroundStyle(design.ink.color)
                    Text(PlaceNames.path(repository.path))
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
                Spacer(minLength: 6)
                if model.directory == repository.path {
                    Image(systemName: "checkmark")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(design.accent.color)
                }
            }
            .padding(.leading, 14)
            .padding(.vertical, 10)
            .frame(minHeight: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxRow)
        .accessibilityLabel(repository.path)
        .identified(
            "new-agent.repository.\(repository.name)", label: repository.path,
            value: model.directory == repository.path ? "chosen" : "")
    }

    /// Keeps the folder among the machine's repositories, or lets it go: a
    /// filled pin for one kept.
    private func pin(_ repository: Directory) -> some View {
        let pinned = model.isPinned(repository.path)
        return Button {
            actions(.pin(repository, !pinned))
        } label: {
            Image(systemName: pinned ? "pin.fill" : "pin")
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(pinned ? design.accent.color : design.inkFaint.color)
                .frame(width: 44, height: 44)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(pinned ? "Remove from Repositories" : "Keep in Repositories")
        .identified("new-agent.pin.\(repository.name)", value: pinned ? "pinned" : "")
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
