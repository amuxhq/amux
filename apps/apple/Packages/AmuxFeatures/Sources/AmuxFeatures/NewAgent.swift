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

/// Starting an agent: one machine, one directory, one layer.
///
/// Everything is on one screen and nothing is a step. Which machine, where,
/// and what runs there are three answers to one question, and a person who has
/// changed their mind about the machine has usually changed their mind about
/// the directory as well — putting them on separate pages would mean walking
/// back through a wizard to say so.
///
/// The button at the foot names the machine. It is the last thing read before
/// something is started somewhere else, and "Start" alone would leave that
/// unstated on the one control where it matters.
public struct NewAgent: View {
    @Environment(\.design) private var design
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
        ZStack {
            Ground()
            VStack(spacing: 0) {
                bar
                middle
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            foot
            // The chooser opens over the screen rather than beside it: what is
            // being chosen is one field of the thing being built, and going
            // somewhere else to choose it would take the machine, the layer
            // and the button that starts it off the screen.
            if model.browsing {
                DirectorySheet(model: model, actions: actions)
                    .transition(.move(edge: .bottom))
            }
        }
        .moving(value: model.browsing)
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

    /// The scrolling middle, split out because the screen's own body already
    /// carries the bar, the foot and the chooser.
    private var middle: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                machines
                directory
                naming
                layers
            }
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 8)
            // Clear of the bar at the foot, which floats over this.
            .padding(.bottom, 130)
        }
        .scrollIndicators(.hidden)
    }

    // MARK: - The bar

    private var bar: some View {
        ZStack {
            Text("New Agent")
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
                .identified("new-agent.title", value: "New Agent")
            HStack {
                BackLink("Agents", identifier: "new-agent.back") {
                    actions(.cancel)
                }
                Spacer()
            }
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.vertical, 10)
    }

    // MARK: - Which machine

    /// The machines this phone is paired with, whether or not they are
    /// answering.
    ///
    /// An unreachable one is on the list and cannot be chosen. Leaving it out
    /// would be this screen deciding that a machine somebody paired with does
    /// not exist; showing it disabled says the true thing, which is that it is
    /// away and nothing can be started on it until it is back.
    private var machines: some View {
        VStack(alignment: .leading, spacing: 9) {
            SectionHead(title: "Host")
            if hosts.hosts.isEmpty {
                Explain("Pair a host before starting an agent.")
                    .identified("new-agent.no-hosts")
            } else {
                RowGroup(items: hosts.hosts) { host in
                    machine(host)
                }
            }
        }
    }

    private func machine(_ host: HostView) -> some View {
        let live = host.online
        return Button { if let id = host.id { actions(.point(id)) } } label: {
            HStack(spacing: 11) {
                Radio(chosen: model.machine == host.id)
                    .opacity(live ? 1 : 0.4)
                VStack(alignment: .leading, spacing: 2) {
                    Text(host.name)
                        .designFont(.identifier, design)
                        .foregroundStyle(live ? design.ink.color : design.inkFaint.color)
                    Text(reach(host))
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkFaint.color)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 6)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .frame(minHeight: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxRow)
        .disabled(!live)
        .accessibilityLabel(spoken(host))
        .accessibilityAddTraits(model.machine == host.id ? [.isSelected] : [])
        .identified(
            "new-agent.host.\(host.id?.description ?? host.name)", label: spoken(host),
            value: model.machine == host.id ? "chosen" : "not chosen", enabled: live)
    }

    /// "macOS · via relay", or the sentence that says why this row cannot be
    /// chosen. The refusal is on the row it is about rather than under the
    /// group, because it is the reason this one row is grey.
    private func reach(_ host: HostView) -> String {
        var parts: [String] = []
        if let platform = host.platform { parts.append(platform) }
        parts.append(route(host))
        return parts.joined(separator: " · ")
    }

    private func route(_ host: HostView) -> String {
        switch host.group {
        case .onThisNetwork: "direct"
        case .throughTheRelay: "via relay"
        case .away: "away, cannot start here"
        case .offline: "offline, cannot start here"
        }
    }

    private func spoken(_ host: HostView) -> String {
        var parts = [host.name]
        if let platform = host.platform { parts.append(platform) }
        parts.append(route(host))
        return parts.joined(separator: ", ")
    }

    // MARK: - Where

    /// Where the agent will start, and the directories this machine was used
    /// in most recently under it.
    ///
    /// One row and a handful of chips rather than a list: the answer is almost
    /// always somewhere this machine has been used before, and the whole
    /// enumeration is one tap away for the times it is not.
    private var directory: some View {
        VStack(alignment: .leading, spacing: 9) {
            SectionHead(title: "Directory")
            Surface {
                Button { model.browsing = true } label: {
                    HStack(spacing: 10) {
                        Image(systemName: "folder")
                            .font(.system(size: 15, weight: .medium))
                            .foregroundStyle(design.inkMuted.color)
                        Text(model.directory.isEmpty ? "Choose a directory" : model.directory)
                            .designFont(.mono, design)
                            .foregroundStyle(
                                model.directory.isEmpty ? design.inkFaint.color : design.ink.color)
                            .lineLimit(1)
                            .truncationMode(.head)
                        Spacer(minLength: 6)
                        Image(systemName: "chevron.right")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(design.inkFaint.color)
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 14)
                    .frame(minHeight: 44)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.amuxRow)
                .disabled(model.machine == nil)
                .accessibilityLabel("Directory")
                .identified(
                    "new-agent.directory", label: "Directory",
                    value: model.directory, enabled: model.machine != nil)
            }
            if !chips.isEmpty { recent }
            if model.listing == .unavailable {
                Explain("\(machineName) could not list its projects. Choose where its agents work, or type a path.")
                    .identified("new-agent.directory.unavailable")
            }
            if let failure = model.failure {
                Refusal(text: failure)
                    .identified("new-agent.refusal", value: failure)
            }
        }
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

    // MARK: - What it is called

    /// The name, standing in the field as the suggestion until it is changed.
    ///
    /// Filled rather than left as a placeholder: the suggestion is what will
    /// be sent if nothing is typed, and a grey hint would suggest the field
    /// was empty. The machine still has the last word, and a name it refuses
    /// comes back as its own sentence under the host.
    private var naming: some View {
        VStack(alignment: .leading, spacing: 9) {
            SectionHead(title: "Name")
            TextField(
                "Name",
                text: Binding(get: { model.name }, set: { model.choose(name: $0) }))
                .textFieldStyle(.plain)
                .designFont(.mono, design)
                .foregroundStyle(design.ink.color)
                .tint(design.accentColor)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .submitLabel(.done)
                .padding(.horizontal, 14)
                .frame(minHeight: 44)
                .background {
                    RoundedRectangle(
                        cornerRadius: design.metrics.controlRadius, style: .continuous)
                        .fill(design.sunken.color)
                }
                .identified("new-agent.name", label: "Name", value: model.name)
        }
    }

    // MARK: - What runs there

    /// Whether the chosen host has said this provider is signed in there;
    /// nil until it says.
    private func signedIn(_ provider: NewAgentStore.Provider) -> Bool? {
        guard let machine = model.machine else { return nil }
        return hosts.host(machine)?.providers.first { $0.provider == provider.wire }?.signedIn
    }

    private var layers: some View {
        VStack(alignment: .leading, spacing: 9) {
            SectionHead(title: "Agent")
            HStack(spacing: 10) {
                ForEach(NewAgentStore.Provider.allCases) { provider in
                    LayerCard(
                        provider: provider, chosen: model.provider == provider,
                        signedOut: signedIn(provider) == false,
                        choose: { model.choose(provider: provider) })
                }
            }
            if signedIn(model.provider) == false {
                Explain("\(model.provider.title) is not signed in on \(machineName). Its agents can’t work there until it is.")
                    .identified("new-agent.signed-out", value: model.provider.rawValue)
            }
            settings
        }
    }

    // MARK: - What it runs with

    /// The model, effort, permission and mode the agent starts with, from
    /// what the host says its provider offers, and the new-worktree switch.
    @ViewBuilder
    private var settings: some View {
        if model.machine != nil {
            switch model.offers[model.provider] {
            case .offered?:
                Surface {
                    VStack(spacing: 0) {
                        ForEach(Array(pickers.enumerated()), id: \.offset) { index, picker in
                            if index > 0 { rule }
                            picker
                        }
                    }
                }
            case .unavailable?:
                Explain("\(machineName) could not say what \(model.provider.title) offers. The agent starts with the host’s defaults.")
                    .identified("new-agent.offer.unavailable")
            case .asking?, nil:
                Explain("Asking \(machineName) what \(model.provider.title) offers…")
                    .identified("new-agent.offer.asking")
            }
        }
        Surface { worktree }
    }

    private var rule: some View {
        Rectangle()
            .fill(design.hairline.color)
            .frame(height: design.metrics.hairline)
    }

    private var pickers: [AnyView] {
        guard let view = model.settings else { return [] }
        var rows = [AnyView(modelPicker(view))]
        if view.changeable.effort == .pick { rows.append(AnyView(effortPicker(view))) }
        if view.changeable.permission == .pick { rows.append(AnyView(permissionPicker(view))) }
        if view.changeable.mode == .pick { rows.append(AnyView(modePicker(view))) }
        return rows
    }

    private var hostDefault: String { String(localized: "Host default") }

    private func modelPicker(_ view: SettingsView) -> some View {
        let current = view.models.first(where: \.current)?.displayName ?? hostDefault
        let items = [MenuItem(title: hostDefault, systemImage: "", chosen: model.model == nil) {
            model.choose(.model(nil))
        }] + view.models.map { offered in
            MenuItem(title: offered.displayName, systemImage: "", chosen: offered.current) {
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
            title: String(localized: "Model default"), systemImage: "", chosen: model.effort == nil
        ) { model.choose(.effort(nil)) }] + view.efforts.map { effort in
            MenuItem(title: worded(effort), systemImage: "", chosen: effort.current) {
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
        let items = [MenuItem(title: hostDefault, systemImage: "", chosen: model.permission == nil) {
            model.choose(.permission(nil))
        }] + view.permissions.map { permission in
            MenuItem(
                title: ChatWords.permission(permission), systemImage: "",
                destructive: permission.neverAsks, chosen: permission.current
            ) { model.choose(.permission(permission.value)) }
        }
        return picker(
            String(localized: "Permission"), id: "new-agent.permission", current: current, items: items)
    }

    private func modePicker(_ view: SettingsView) -> some View {
        let items = view.modes.map { mode in
            MenuItem(title: ChatWords.mode(mode), systemImage: "", chosen: mode.current) {
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
        .tint(design.ink.color)
        .oneSwitch(
            String(localized: "New worktree, A worktree of its own, made from the directory’s repository"),
            isOn: Binding(get: { model.newWorktree }, set: { model.newWorktree = $0 }))
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: 44)
        .identified("new-agent.worktree", label: "New worktree", value: model.newWorktree ? "on" : "off")
    }

    // MARK: - Starting it

    private var machineName: String {
        model.machine.flatMap { hosts.host($0)?.name } ?? "the host"
    }

    private var foot: some View {
        VStack {
            Spacer(minLength: 0)
            BottomAction {
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
}

/// One layer, as a card that is chosen whole; one the host has said is
/// signed out there says so.
private struct LayerCard: View {
    @Environment(\.design) private var design
    let provider: NewAgentStore.Provider
    let chosen: Bool
    let signedOut: Bool
    let choose: @MainActor () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Image(systemName: glyph)
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(chosen ? design.ink.color : design.inkMuted.color)
            Text(provider.title)
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            if signedOut {
                Text("Not signed in")
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.removed.color)
                    .lineLimit(1)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background {
            let shape = RoundedRectangle(
                cornerRadius: design.metrics.cardRadius, style: .continuous)
            ZStack {
                shape.fill(design.raised.color)
                shape.strokeBorder(
                    chosen ? design.ink.color : design.hairline.color,
                    lineWidth: chosen ? 1.5 : 1)
            }
        }
        .contentShape(RoundedRectangle(
            cornerRadius: design.metrics.cardRadius, style: .continuous))
        .onTapGesture(perform: choose)
        // One element that VoiceOver activates: as a container of its
        // texts, double-tapping the card did nothing, since a tap gesture
        // is not an action VoiceOver can take.
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(chosen ? [.isButton, .isSelected] : .isButton)
        .accessibilityAction { choose() }
        .identified(
            "new-agent.provider.\(provider.rawValue)",
            label: signedOut ? "\(provider.title), not signed in" : provider.title,
            value: chosen ? "chosen" : "not chosen")
    }

    private var glyph: String {
        switch provider {
        case .claude: "asterisk"
        case .codex: "chevron.left.forwardslash.chevron.right"
        }
    }
}

/// Something the machine refused, in the machine's own words.
private struct Refusal: View {
    @Environment(\.design) private var design
    let text: String

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "exclamationmark.triangle")
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(design.inkMuted.color)
            Text(text)
                .designFont(.detail, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                .fill(design.sunken.color)
        }
        .accessibilityElement(children: .combine)
    }
}

/// The directories a machine has to offer, and a field for one it does not.
///
/// The typed path is under the list rather than instead of it. A machine that
/// would not enumerate anything still runs agents, and a directory that is not
/// a repository is an ordinary thing to want; the list is the shortcut and the
/// field is the answer to everything it does not cover.
private struct DirectorySheet: View {
    @Environment(\.design) private var design
    let model: NewAgentStore
    let actions: @MainActor (NewAgentAction) -> Void

    var body: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 0)
            panel
        }
        .background(alignment: .top) {
            Color.black.opacity(Glass.scrim)
                .ignoresSafeArea()
                .onTapGesture { model.browsing = false }
        }
    }

    private var panel: some View {
        VStack(alignment: .leading, spacing: 12) {
            Capsule()
                .fill(design.hairline.color)
                .frame(width: 40, height: 5)
                .frame(maxWidth: .infinity)
            HStack(alignment: .firstTextBaseline) {
                Text("Directory")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                Spacer(minLength: 8)
                Button { model.browsing = false } label: {
                    ActionLabel("Done", kind: .plain)
                }
                .buttonStyle(.amuxControl)
                .identified("new-agent.browse.done", label: "Done")
            }
            search
            ViewThatFits(in: .vertical) {
                listing
                ScrollView { listing }.scrollIndicators(.hidden)
            }
            path
        }
        .padding(16)
        .frame(maxWidth: .infinity)
        .background {
            UnevenRoundedRectangle(
                topLeadingRadius: design.metrics.cardRadius,
                topTrailingRadius: design.metrics.cardRadius, style: .continuous)
                .fill(design.raised.color)
                .ignoresSafeArea(edges: .bottom)
        }
        .accessibilityElement(children: .contain)
        .identified("new-agent.browse", value: model.query)
    }

    @ViewBuilder
    private var search: some View {
        if model.listing != .unavailable {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(design.inkFaint.color)
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
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.sunken.color)
            }
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
                Explain("This host could not list its projects.")
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
