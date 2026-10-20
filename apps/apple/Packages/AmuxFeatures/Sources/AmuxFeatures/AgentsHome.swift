import AmuxCore
import AmuxDesign
import SwiftUI

/// What happened on the Agents home. The screen never navigates and never
/// reaches the network; it says what the person did and the shell decides
/// where that leads.
public enum HomeAction: Equatable, Sendable {
    case open(AgentKey)
    /// List only the agents this keeps: typed words, a state or a host.
    case filter(FleetFilter)
    /// Lists or folds away a family's members.
    case toggleFamily(AgentKey)
    case newAgent
    case switchAccount(AccountId)
    case addAccount
    case signIn
    /// Sign back into one account this phone lists, from its row.
    case signInAgain(AccountId)
    case subscribe
    /// Raise the settings sheet from the account menu.
    case openSettings
    /// Pair with a machine this phone has found. There is no pairing without
    /// one: a code is typed for a particular machine, and a machine that has
    /// not been found is reached by scanning the code it prints instead.
    case pair(HostId)
    case openExceptions
    /// The list was pulled, or the screen came back into view. Only then may
    /// the ordering regroup.
    case refresh
}

/// The screen the app opens onto.
///
/// A row's only job is to be worth opening or worth skipping. Nothing here
/// decides anything about an agent: a decision belongs to the conversation
/// that owns it, not to a button on a list.
///
/// The title, its subtitle and the buttons beside it are the system
/// navigation bar's: a large title that shrinks as the list scrolls, with
/// glass buttons, the way a chat app's list reads.
public struct AgentsHome: View {
    @Environment(\.design) private var design
    @Environment(\.dynamicTypeSize) private var typeSize
    @Environment(\.photographed) private var photographed
    @Environment(\.reducesMotion) private var reducesMotion
    @Environment(\.reducesTransparency) private var reducesTransparency
    private let model: FleetStore
    private let accounts: AccountRegistry
    /// The machines, for the one thing this screen says about them: a phone
    /// with no agents yet is a phone that has not paired, and what it has
    /// found on its network is the shortest way out of that.
    private let hosts: HostsStore
    private let actions: @MainActor (HomeAction) -> Void
    /// The fold is view state, not fleet state: opening it is a thing this
    /// screen is doing, and coming back to the screen starts it closed again.
    /// Whether the account switcher is out. View state for the same reason,
    /// and handed in only so a capture can ask for the panel: which accounts
    /// this phone has is a fact, having the list open is not.
    /// What the list is narrowed to: words typed into the search field, and
    /// the chip chosen under it.
    @State private var words = ""
    @State private var only: FleetOnly?

    public init(
        model: FleetStore,
        accounts: AccountRegistry,
        hosts: HostsStore,
        actions: @escaping @MainActor (HomeAction) -> Void
    ) {
        self.model = model
        self.accounts = accounts
        self.hosts = hosts
        self.actions = actions
    }

    public var body: some View {
        home
    }

    private var home: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                // The list whenever there is one. An account is never what
                // decides this: a phone that is signed out and paired with a
                // machine on its own network has a fleet worth reading, and a
                // phone that remembers agents it cannot refresh has one too.
                // What it cannot do is said on the exceptions line.
                //
                // Without agents, what is worth saying depends on whether this
                // phone has a machine at all. One that has paired is not a
                // phone with nothing paired, and offering to pair again there
                // would say the pairing it just did never happened.
                if !model.rows.isEmpty || filtering {
                    fleet
                } else if !model.loaded {
                    // A reopened app draws before its saved hosts are read;
                    // saying nothing is paired then would be untrue a moment
                    // later, so nothing is said until it is known.
                    Color.clear
                } else if !model.machines.isEmpty {
                    noAgentsYet
                } else {
                    nothingPairedYet
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        }
        .navigationTitle("Agents")
        .navigationSubtitle(subtitle)
        // Pulled down from under the title, as a chat app's search is. It
        // matches what a row says, as the terminal's filter does.
        .searchable(
            text: $words, placement: .navigationBarDrawer(displayMode: .automatic),
            prompt: Text("Search"))
        .onChange(of: filter) { _, filter in actions(.filter(filter)) }
        .toolbar {
            // Under the large title the bar's own subtitle sits tight against
            // the display face's short descenders; drawn here it takes the
            // list's preview type and a breath of space from the title.
            if !subtitle.isEmpty {
                ToolbarItem(placement: .largeSubtitle) {
                    Text(subtitle)
                        .designFont(.detail, design)
                        .foregroundStyle(design.inkMuted.color)
                        .padding(.top, Space.xs)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            ToolbarItem(placement: .topBarLeading) {
                accountMenu
            }
            // The disc is its own glass, as the + is; the bar's ring
            // around it would draw a second one.
            .sharedBackgroundVisibility(
                reducesTransparency || accounts.selectedAccount != nil ? .hidden : .automatic)
            if canStartAnAgent {
                ToolbarItem(placement: .topBarTrailing) {
                    Button { actions(.newAgent) } label: { Image(systemName: "plus") }
                        .prominentBarButton()
                        .tint(design.accent.color)
                        .accessibilityLabel("New Agent")
                        .accessibilityIdentifier("home.newAgent")
                }
                // The bar sets its buttons on glass of its own; with less
                // transparency asked for, the button's own fill is enough.
                .sharedBackgroundVisibility(reducesTransparency ? .hidden : .automatic)
            }
        }
        .onAppear { actions(.refresh) }
        // Ages count while the list is on screen, as the terminal's do. A
        // compared picture holds its clock still.
        .task(id: photographed) {
            guard !photographed else { return }
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                model.refreshOrder(now: Date())
            }
        }
        // A screen is a container of the things on it, not a name for all of
        // them. Without this the system spreads this identifier over every
        // element underneath — the title, the buttons, the rows — so
        // everything on the screen answers to the screen's own name, for
        // VoiceOver and for anything driving the app alike.
        .accessibilityElement(children: .contain)
        .identified("home", value: accounts.gate.name)
    }

    // MARK: - Header

    /// Whether anybody is signed in on this phone. An account listed with Sign
    /// In beside it is remembered, not signed in.
    private var signedIn: Bool { accounts.accounts.contains(where: \.signedIn) }

    /// Whether any machine would actually run something started now.
    private var canStartAnAgent: Bool {
        model.machines.contains(where: \.online)
    }

    /// What the switcher asked for, in the words this screen speaks.
    ///
    /// Switching, adding and signing back in all leave the screen, because
    /// none of them is something a list can do.
    private func switcher(_ action: AccountsAction) {
        switch action {
        case .select(let id): actions(.switchAccount(id))
        case .add: actions(.addAccount)
        case .signIn(let id): actions(.signInAgain(id))
        // Nothing else reaches this screen: the rest of an account's actions
        // live under You, where there is room to state what they do.
        case .signOut, .remove, .confirmRemoval, .cancelRemoval, .delete, .subscription,
             .appearance, .shareUsage, .support, .report, .exportDump:
            break
        }
    }

    /// The accounts, in the system's own menu hung from the disc: each by
    /// name with what this phone knows of it under, the one on screen
    /// ticked, and a signed-out one offering to sign back in; then Settings.
    /// Always there, since it is also the way to the settings.
    ///
    /// The disc is the bar's other circle: the account's initials on the same
    /// accent glass as the + across from it, and the same size, so the two
    /// read as a pair. With no account signed in there is nobody to name, so
    /// it is a person on the bar's plain glass: hollow rather than absent,
    /// because not having a usable account yet is a state, not a hole.
    @ViewBuilder
    private var accountMenu: some View {
        if let entry = accounts.selectedAccount {
            accountMenu {
                Text(InitialsDisc.initials(of: entry.name))
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(design.onAccent.color)
                    .fixedSize()
                    .frame(width: 30, height: 30)
                    // The picture covers the whole button, glass and all:
                    // a face in a teal ring would be a badge, not a person.
                    .overlay { AccountPicture(url: entry.account.picture, size: Self.barButton) }
            }
            .menuStyle(.button)
            .prominentBarButton()
            .buttonBorderShape(.circle)
            .tint(design.accent.color)
            // The bar keeps room for the ring it would have drawn round a
            // menu; without that ring the disc would sit this far in from the
            // margin the + keeps on the other side.
            .padding(.leading, -Space.s)
        } else {
            accountMenu { Image(systemName: "person") }
                .barItemSurface()
        }
    }

    /// A bar button's size: the system draws its glass buttons this large.
    static let barButton: CGFloat = 44

    private func accountMenu(@ViewBuilder label: () -> some View) -> some View {
        Menu {
            Section {
                ForEach(accounts.accounts) { entry in
                    Button {
                        switcher(entry.signedIn ? .select(entry.id) : .signIn(entry.id))
                    } label: {
                        Label {
                            Text(entry.name)
                            Text(entry.signedIn ? entry.line : String(localized: "Signed out · tap to sign in"))
                        } icon: {
                            if entry.id == accounts.selected { Image(systemName: "checkmark") }
                        }
                    }
                    .identified("account.\(entry.account.email)", label: entry.name)
                }
            }
            Button { switcher(.add) } label: { Label("Add Account", systemImage: "plus") }
                .identified("accounts.add", label: "Add Account")
            // Everything else about accounts and this phone, where there is
            // room to say what each thing does.
            Section {
                Button { actions(.openSettings) } label: { Label("Settings", systemImage: "gearshape") }
                    .identified("accounts.settings", label: "Settings")
            }
        } label: {
            label()
        }
        .accessibilityLabel("Switch account")
        .accessibilityIdentifier("home.accounts")
    }

    /// Under the title, only what needs saying: whose fleet this is when the
    /// phone holds more than one account, how many agents need the person,
    /// and on an empty phone why it is empty. A calm fleet has no subtitle;
    /// the list says the rest.
    private var subtitle: String {
        if accounts.accounts.count > 1, let entry = accounts.selectedAccount {
            let waiting = model.needYou
            return waiting == 0 ? entry.name : "\(entry.name) · \(waiting) need you"
        }
        if !model.rows.isEmpty {
            return model.needYou == 0 ? "" : "\(model.needYou) need you"
        }
        if !model.loaded { return "" }
        if !model.machines.isEmpty {
            let count = model.machines.count
            return "No agents yet · \(count) host\(count == 1 ? "" : "s")"
        }
        // Not "Not signed in" and not "Not subscribed". A phone with no agents
        // is a phone that has paired with nothing, whatever its account is
        // doing: amux is free on the network this phone is already on, so an
        // empty list is never evidence that something has to be bought.
        return "Nothing paired yet"
    }

    // MARK: - The list

    private var sections: [HomeSection] { model.sections }

    /// The agents that are live, in one list, with the ones that need the
    /// person at the top; the stopped ones folded away under it.
    private var live: [AgentRow] {
        sections.filter { $0.kind != .exited }.flatMap(\.rows)
    }

    private var stopped: HomeSection? { sections.first { $0.kind == .exited } }

    /// Names a machine on a row only when there is more than one to tell
    /// apart.
    private var namesHosts: Bool { Set(model.rows.map(\.hostName)).count > 1 }

    private var fleet: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                chips
                    .padding(.bottom, Space.s)
                if filtering && model.rows.isEmpty {
                    Text("Nothing matches")
                        .designFont(.detail, design)
                        .foregroundStyle(design.inkMuted.color)
                        .padding(.horizontal, Space.margin)
                        .padding(.top, Space.xl)
                        .identified("home.noMatches", label: "Nothing matches")
                }
                if !callouts.isEmpty {
                    CalloutStrip(callouts: callouts, act: actions)
                        .padding(.top, Space.xs)
                        .padding(.bottom, Space.s)
                }
                ForEach(Array(live.enumerated()), id: \.element.id) { index, row in
                    agentRow(row)
                    if index < live.count - 1 { rule(row) }
                }
                if let stopped {
                    stoppedFold(stopped)
                    if !stopped.folded {
                        // One block, so it fades and settles as a whole: it
                        // comes down from under the line that opened it and
                        // goes back up into it.
                        VStack(alignment: .leading, spacing: 0) {
                            ForEach(stopped.rows) { row in
                                agentRow(row)
                                rule(row)
                            }
                        }
                        .transition(.opacity.combined(with: .offset(y: -Space.m)))
                    }
                }
            }
            .padding(.top, Space.xs)
            .padding(.bottom, 120)
            // Keyed on which rows are where and nothing else. A row is
            // equatable over its whole card, so animating on the sections
            // themselves would set the list moving every time an agent
            // changed its headline or aged by a minute. Safe because
            // regrouping only happens on a refresh, which is something the
            // reader did.
            .moving(value: sections.map { $0.rows.map(\.id) })
        }
        .scrollIndicators(.hidden)
        .refreshable { actions(.refresh) }
    }

    // MARK: - Narrowing the list

    private var filter: FleetFilter { FleetFilter(words: words, only: only) }

    private var filtering: Bool {
        !words.trimmingCharacters(in: .whitespaces).isEmpty || only != nil
    }

    /// One state or one machine at a time, as a chat app's folders are:
    /// everything, what needs you, what is working, and each machine when
    /// the list spans more than one.
    private var chips: some View {
        ScrollView(.horizontal) {
            HStack(spacing: Space.s) {
                chip(String(localized: "All"), nil, id: "all")
                chip(
                    model.needYou > 0
                        ? String(localized: "Needs you \(model.needYou)") : String(localized: "Needs you"),
                    .needsYou, id: "needsYou")
                chip(String(localized: "Working"), .working, id: "working")
                // Counted from the machines rather than the rows, so a list
                // narrowed to one machine still offers the others.
                if model.machines.count > 1 {
                    ForEach(model.machines.sorted { $0.name < $1.name }, id: \.hostId) { host in
                        chip(PlaceNames.host(host.name), .host(host: host.hostId), id: "host.\(host.name)")
                    }
                }
            }
            .padding(.horizontal, Space.margin)
        }
        .scrollIndicators(.hidden)
    }

    private func chip(_ title: String, _ narrowing: FleetOnly?, id: String) -> some View {
        let on = only == narrowing
        return Button { only = narrowing } label: {
            Text(title)
                .designFont(.caption, design)
                .fontWeight(.medium)
                .foregroundStyle(on ? design.ground.color : design.ink.color)
                .padding(.horizontal, Space.m)
                .frame(height: 32)
                .background(Capsule().fill(on ? design.ink.color : design.sunken.color))
                .contentShape(Capsule())
        }
        .buttonStyle(.amuxControl)
        .accessibilityAddTraits(on ? .isSelected : [])
        .identified("home.chip.\(id)", label: title, value: on ? "on" : "off")
    }

    /// A hairline under a row, starting where its text does, as a system
    /// list draws one.
    private func rule(_ row: AgentRow) -> some View {
        Rectangle()
            .fill(design.hairline.color)
            .frame(height: design.metrics.hairline)
            .padding(.leading, Space.margin + AgentRowView.textInset + indent(row))
            .accessibilityHidden(true)
    }

    /// Members sit under their head by one step of the mark's column, so a
    /// member's mark lines up with its head's name.
    private func indent(_ row: AgentRow) -> CGFloat { CGFloat(row.depth) * AgentRowView.textInset }

    /// A row, indented under its family's head, with the head's control for
    /// listing its members under its age.
    private func agentRow(_ row: AgentRow) -> some View {
        let state = RowState(row: row)
        let heads = row.depth == 0 && row.children > 0
        // A stop that is held until the host is back says so before anything
        // else the row would say; the chat has its Cancel.
        let second = row.card.stopHeld
            ? (text: String(localized: "Stop held · sends when \(row.hostName) is back"), ink: FleetWords.Ink.warning)
            : FleetWords.secondLine(
                row.secondLine, kind: row.card.kind, now: model.orderedAt,
                backgroundJobs: row.card.backgroundJobs)
        let content = AgentRowView(
            row: row, state: state, host: namesHosts ? PlaceNames.host(row.hostName) : nil,
            now: model.orderedAt, second: second, leavesRoomTrailing: heads)
        return openable(row, state, content)
            .overlay(alignment: .bottomTrailing) {
                if heads { family(row) }
            }
            .padding(.leading, Space.margin + indent(row))
            .padding(.trailing, Space.margin)
    }

    // An agent of a kind this build has no chat for is listed and not
    // offered to open. A button that led to a chat of which not one row
    // could be read would be a worse answer than the row saying so where it
    // stands.
    @ViewBuilder
    private func openable(_ row: AgentRow, _ state: RowState, _ content: AgentRowView) -> some View {
        if row.readable {
            Button { actions(.open(row.id)) } label: { content }
                .buttonStyle(.amuxPush)
                .accessibilityLabel(spoken(row, state))
                .identified("home.row.\(row.id)", label: spoken(row, state), value: state.name)
        } else {
            content
                .accessibilityElement(children: .combine)
                .accessibilityLabel(spoken(row, state))
                .identified("home.row.\(row.id)", label: spoken(row, state), value: state.name)
        }
    }

    /// How many agents a head started, and the control that lists them, in
    /// the row's trailing column under its age.
    private func family(_ row: AgentRow) -> some View {
        let said = row.expanded
            ? "Hide \(row.children) started by \(row.name)"
            : "Show \(row.children) started by \(row.name)"
        return Button { actions(.toggleFamily(row.id)) } label: {
            HStack(spacing: Space.xs) {
                if row.speaksForAMember { NeedsYouDot() }
                Text("\(row.children)")
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkMuted.color)
                Image(systemName: "chevron.down")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
                    .rotationEffect(.degrees(row.expanded ? 180 : 0))
            }
            .padding(.leading, Space.m)
            .padding(.vertical, Space.m)
            .frame(minWidth: 44, minHeight: 44, alignment: .trailing)
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxControl)
        .accessibilityLabel(said)
        .identified(
            "home.family.\(row.id)", label: said, value: row.expanded ? "expanded" : "folded")
    }

    /// What a row says to somebody who cannot see it, in the order the row
    /// says it: who, what, where, how long, and what it needs.
    private func spoken(_ row: AgentRow, _ state: RowState) -> String {
        var parts = [row.name]
        if let branch = row.branch { parts.append(String(localized: "on \(branch)")) }
        if let said = state.spoken { parts.append(said) }
        if let second = FleetWords.secondLine(
            row.secondLine, kind: row.card.kind, now: model.orderedAt,
            backgroundJobs: row.card.backgroundJobs
        ) {
            parts.append(second.text)
        }
        if row.speaksForAMember { parts.append("a subagent needs you") }
        parts.append([PlaceNames.host(row.hostName), row.workingDirectory]
            .joined(separator: ", "))
        parts.append(row.age(at: model.orderedAt) + " ago")
        if row.unread { parts.append("unread") }
        return parts.joined(separator: ", ")
    }

    /// A section's heading, as every client words it.
    static func title(_ kind: SectionKind) -> String {
        switch kind {
        case .needsYou: String(localized: "Needs you")
        case .live: String(localized: "Live")
        case .exited: String(localized: "Exited")
        }
    }

    /// The stopped agents, folded into one quiet line: a hairline with the
    /// section's name set into it where the rows' names start, closing the
    /// live list the way the chat marks where its agent started.
    ///
    /// Opened and closed in one explicit animation rather than by watching
    /// the fold, so the rows leave as smoothly as they arrive and nothing
    /// else on the screen is swept along with them.
    private func stoppedFold(_ section: HomeSection) -> some View {
        let names = section.rows.filter { $0.depth == 0 }.map(\.name).joined(separator: ", ")
        let title = "\(Self.title(section.kind)) · \(section.families)"
        let hairline = Rectangle().fill(design.hairline.color).frame(height: design.metrics.hairline)
        return Button {
            withAnimation(photographed || reducesMotion ? nil : Motion.standard) {
                model.toggle(section.kind)
            }
        } label: {
            HStack(spacing: Space.s) {
                hairline
                    .frame(width: AgentRowView.markColumn)
                    .padding(.trailing, Space.m - Space.s)
                Text(Self.title(section.kind))
                    .designFont(.sectionTitle, design)
                    .foregroundStyle(design.inkMuted.color)
                    .layoutPriority(1)
                Text("\(section.families)")
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkFaint.color)
                    .layoutPriority(1)
                hairline
                Image(systemName: "chevron.right")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
                    .rotationEffect(.degrees(section.folded ? 0 : 90))
                    .frame(width: 16)
            }
            .padding(.horizontal, Space.margin)
            .frame(minHeight: 44)
            .contentShape(Rectangle())
        }
        // Nothing drawn under the thumb: the rows opening or closing beneath
        // it are the answer to the press. A control's press would shrink this
        // full-width line about its centre, sliding the heading inward and
        // back on every tap, and a row's tint flashes a slab over a line that
        // is mostly empty space.
        .buttonStyle(.amuxPush)
        .padding(.top, Space.l)
        .accessibilityLabel("\(title), \(names)")
        .identified("home.fold.\(section.id)", label: "\(title), \(names)", value: names)
    }

    /// The one line a home is allowed above the list.
    ///
    /// It is not a summary — a summary is a card that must be filled, so it
    /// fills itself with counts nobody asked for. It is an exceptions line: it
    /// appears only when something is actually wrong, it takes one row when it
    /// does, and when everything is fine the top of the screen is the list.
    ///
    /// The order is the one the desktop's banner follows, and for the same
    /// reason: a connection that is down outranks everything, then a machine
    /// the relay can see and this account may not reach, then a machine
    /// nothing can reach on a phone with no account, and last the plain fact
    /// that a machine is offline. A phone that can reach every machine it owns
    /// shows none of it and is never asked for an account.
    private var exceptions: (text: String, act: HomeAction)? {
        if signedIn, let trouble = model.relayTrouble {
            return (trouble, model.relay == .signInAgain ? .signIn : .openExceptions)
        }
        if let away = model.awayHost {
            return ("\(away) is away · subscribe to reach your agents from anywhere", .subscribe)
        }
        guard signedIn else {
            return model.unreachableHost == nil ? nil : (SignInCopy.caption, .signIn)
        }
        return model.exceptions.map { ($0, .openExceptions) }
    }

    /// What the list asks of the person before its rows: pairing a machine
    /// found nearby, then whatever stands between the phone and the fleet.
    private var callouts: [Callout<HomeAction>] {
        var callouts = found.compactMap { host -> Callout<HomeAction>? in
            guard let id = host.id else { return nil }
            return Callout(
                id: "pair.\(host.name)", glyph: "plus.circle",
                title: String(localized: "\(host.name) is nearby"),
                detail: String(localized: "Pair it to see its agents here"),
                button: String(localized: "Pair"), act: .pair(id))
        }
        if let exceptions {
            callouts.append(Callout(
                id: "exceptions", glyph: glyph(for: exceptions.act), title: exceptions.text,
                detail: nil, button: nil, act: exceptions.act))
        }
        return callouts
    }

    /// What the line is about: a link that is down, or an account that would
    /// open one.
    private func glyph(for act: HomeAction) -> String {
        switch act {
        case .signIn, .subscribe: "person.slash"
        default: "wifi.slash"
        }
    }

    // MARK: - Nothing to reach yet

    /// Not a splash: the real home screen, empty.
    ///
    /// A splash would teach that this is a service you subscribe to; an empty
    /// list teaches that it is a client for machines you own. So the one
    /// action is pairing, and an account is offered under it as what pairing
    /// is not: reaching those machines when you are not on their network. A
    /// phone that has already found something on this network leads with that
    /// — there is nothing to type and nothing to sign into, and the machine is
    /// right there.
    ///
    /// There is no Pair button of its own. Pairing is always with a particular
    /// machine: one this phone has found is offered by name above, and one it
    /// has not found prints a code to scan. A button that opened a keypad for
    /// no machine at all led to a code that could not be sent anywhere.
    private var nothingPairedYet: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("No agents yet")
                        .designFont(.screenTitle, design)
                        .foregroundStyle(design.ink.color)
                        .identified("home.empty.firstRun", value: "No agents yet")
                    Explain("Pair with a host and its agents appear here.")
                        .identified("home.empty.explain")
                }
                if !found.isEmpty { offers }
                PairingHint()
                    .identified("home.empty.howToPair")
                if !signedIn {
                    SignInCallToAction(identifier: "home.empty.signIn") { actions(.signIn) }
                }
            }
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 30)
            .padding(.bottom, 120)
        }
        .scrollIndicators(.hidden)
    }

    /// Paired, and nothing running yet.
    ///
    /// The phone has done the part that needed doing, so the screen says what
    /// comes next rather than offering to pair again. Starting an agent is the
    /// one action, and only where a machine would run it; the callouts say
    /// why not otherwise, as they do above a list.
    private var noAgentsYet: some View {
        ScrollView {
            if !callouts.isEmpty {
                CalloutStrip(callouts: callouts, act: actions)
                    .padding(.top, Space.l)
            }
            VStack(alignment: .leading, spacing: 18) {
                // No headline of its own: the subtitle under the title already
                // says there are no agents and how many hosts there are.
                Explain("Start an agent on a host and it appears here.")
                    .identified("home.empty.noAgents")
                if canStartAnAgent {
                    Button { actions(.newAgent) } label: {
                        ActionLabel("New Agent", kind: .primary, fill: true)
                    }
                    .buttonStyle(.amuxControl)
                    .accessibilityLabel("New Agent")
                    .identified("home.empty.newAgent", label: "New Agent")
                }
            }
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 30)
            .padding(.bottom, 120)
        }
        .scrollIndicators(.hidden)
    }

    /// The machines on this network this phone has found and not paired with.
    ///
    /// Offered here and not only on the Hosts tab because this is where
    /// somebody is standing when they have nothing: the shortest true sentence
    /// about an empty phone on a network with a host on it is that the host is
    /// right there.
    private var found: [HostView] { hosts.candidates(.onThisNetwork) }

    private var offers: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "On this network")
            RowGroup(items: found) { host in offer(host) }
        }
    }

    private func offer(_ host: HostView) -> some View {
        HStack(spacing: 11) {
            VStack(alignment: .leading, spacing: 2) {
                Text(host.name)
                    .designFont(.identifier, design)
                    .foregroundStyle(design.ink.color)
                Text(offered(host))
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
            }
            Spacer(minLength: 6)
            Button { if let id = host.id { actions(.pair(id)) } } label: {
                ActionLabel("Pair", kind: .outline)
            }
            .buttonStyle(.amuxRow)
            .accessibilityLabel("Pair with \(host.name)")
            .identified("home.pair.\(host.id?.description ?? host.name)", label: "Pair with \(host.name)")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: 44)
        .accessibilityElement(children: .contain)
        .identified("home.offer.\(host.id?.description ?? host.name)", label: spokenOffer(host), value: "found")
    }

    private func offered(_ host: HostView) -> String {
        var parts: [String] = []
        if let platform = host.platform { parts.append(platform) }
        parts.append("found")
        return parts.joined(separator: " · ")
    }

    private func spokenOffer(_ host: HostView) -> String {
        var parts = [host.name]
        if let platform = host.platform { parts.append(platform) }
        parts.append("found, not paired")
        return parts.joined(separator: ", ")
    }

}

extension FleetGate {
    /// The state's own word, for a capture and a door query to agree on.
    public var name: String {
        switch self {
        case .ready: "ready"
        case .signedOut: "signed-out"
        case .unsubscribed: "unsubscribed"
        }
    }
}

/// One agent, as a list row: what state it is in, who it is, and what it
/// last said or is asking.
///
/// The leading mark is the agent's state, where a chat app puts a face: an
/// agent has no face, and its state is what a person scans the list for. The
/// mark is loud only when the agent needs the person. The words under the
/// name are the agent's own, so the state is never spelled out as well
/// unless there is nothing else to say.
///
/// Two columns: the name over its words, and a trailing column of facts
/// about it (the machine and the age) on the name's baseline.
struct AgentRowView: View {
    @Environment(\.design) private var design
    @Environment(\.dynamicTypeSize) private var typeSize
    let row: AgentRow
    let state: RowState
    /// The machine, when the list spans more than one.
    let host: String?
    let now: Date
    /// What its state calls for, in words, when there is something to say.
    let second: (text: String, ink: FleetWords.Ink)?
    /// Whether a control sits over the trailing end of the second line.
    var leavesRoomTrailing = false

    /// The mark's column and the gap after it: where the text starts, and
    /// the rule under the row with it.
    static let markColumn: CGFloat = Space.m
    static let textInset: CGFloat = markColumn + Space.m

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Space.m) {
            StateMark(state: state)
                .frame(width: Self.markColumn)
                // Centred on the name's lowercase letters rather than sat on
                // its baseline.
                .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 4.5 }
            VStack(alignment: .leading, spacing: Space.xxs) {
                HStack(alignment: .firstTextBaseline, spacing: Space.s) {
                    Text(row.name)
                        .designFont(.identifier, design)
                        .foregroundStyle(stopped ? design.inkMuted.color : design.ink.color)
                        .lineLimit(1)
                        .layoutPriority(1)
                    Spacer(minLength: Space.xs)
                    Text(facts)
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                        // An age moves with the clock: a compared screenshot masks it.
                        .reported("home.row.\(row.id).age.volatile")
                }
                if let preview {
                    Text(withCode(preview.text))
                        .designFont(.detail, design)
                        .foregroundStyle(ink(preview.ink))
                        // What an agent is asking is worth a second line; what
                        // it is doing or last said is a glance.
                        .lineLimit(state.needsYou || typeSize.isAccessibilitySize ? 2 : 1)
                        .padding(.trailing, leavesRoomTrailing ? 40 : 0)
                        .reported("home.row.\(row.id).second")
                }
            }
        }
        .padding(.vertical, Space.m)
        .frame(maxWidth: .infinity, minHeight: 64, alignment: .leading)
        .contentShape(Rectangle())
    }

    /// The machine and the age, as one quiet run of facts.
    private var facts: String {
        [host, row.age(at: now)].compactMap { $0 }.joined(separator: " · ")
    }

    private var stopped: Bool {
        if case .exited = state { return true }
        return false
    }

    /// The agent's own words, or the state's where it has none: a machine
    /// that is out of reach says so first, and a stopped agent says how it
    /// ended.
    private var preview: (text: String, ink: FleetWords.Ink)? {
        if state.namesTheHost, let word = state.word {
            let rest = second?.text ?? state.elaboration
            return ([word, rest].compactMap { $0 }.joined(separator: " · "), .quiet)
        }
        if let second { return second }
        switch state {
        case .exited, .unsupported, .starting:
            let words = [state.word, state.elaboration].compactMap { $0 }
            return words.isEmpty ? nil : (words.joined(separator: " · "), .quiet)
        default:
            return nil
        }
    }

    /// The preview with the command or path in it set in the code face, as
    /// a terminal or an editor would show it, so it reads as something to
    /// run or open and not as prose.
    private func withCode(_ text: String) -> AttributedString {
        var words = AttributedString(text)
        if let code, !code.isEmpty, let range = words.range(of: code) {
            words[range].font = design.font(.mono)
        }
        return words
    }

    /// The command or path the second line names, if it names one.
    private var code: String? {
        switch row.secondLine {
        case .ask(let ask):
            switch ask.subject {
            case .command(let command): ChatWords.firstLine(command)
            case .edit(let path, let files, _) where files <= 1: path
            default: nil
            }
        case .step(let line):
            if case .running = line.activity.kind, let step = line.step { ChatWords.firstLine(step) } else { nil }
        default:
            nil
        }
    }

    private func ink(_ ink: FleetWords.Ink) -> Color {
        switch ink {
        case .ask: design.ink.color
        case .quiet: design.inkMuted.color
        case .warning: design.accent.color
        case .error: design.removed.color
        }
    }
}

/// How a working agent's mark moves. Several, while one is chosen.
public enum WorkingMark: String, CaseIterable, Sendable {
    /// A dot that slowly swells and fades, like breathing.
    case breathing
    /// Three small dots that rise in turn, as a chat app shows typing.
    case typing
    /// A short arc travelling round a faint ring.
    case orbit
    /// A ring that fills and empties.
    case filling
}

private struct WorkingMarkKey: EnvironmentKey {
    static let defaultValue = WorkingMark.breathing
}

extension EnvironmentValues {
    public var workingMark: WorkingMark {
        get { self[WorkingMarkKey.self] }
        set { self[WorkingMarkKey.self] = newValue }
    }
}

/// A state's mark by the state's name, for a catalogue to show every mark.
public struct StateMarkSample: View {
    let state: String
    public init(_ state: String) { self.state = state }
    public var body: some View { StateMark(state: sampled) }

    private var sampled: RowState {
        switch state {
        case "needs-you": .needsYou
        case "working": .working
        case "away": .hostAway("studio")
        case "stopped": .exited(.finished)
        default: .idle
        }
    }
}

/// An agent's state as a mark, in the column a chat list keeps for a face.
///
/// Needs you is the only filled, coloured mark on the screen. Working moves,
/// so a list of agents at work is visibly alive; it holds still in front of
/// a camera and for a reader who asked for less motion. Idle is an empty
/// ring, and a machine out of reach or a stopped agent is fainter still.
struct StateMark: View {
    @Environment(\.design) private var design
    @Environment(\.photographed) private var photographed
    @Environment(\.reducesMotion) private var reducesMotion
    @Environment(\.workingMark) private var working
    let state: RowState

    static let size: CGFloat = 10

    var body: some View {
        Group {
            switch state {
            case .needsYou:
                Circle().fill(design.accent.color)
            case .working, .starting:
                Working(style: working, still: photographed || reducesMotion)
            case .idle:
                Circle().strokeBorder(design.inkFaint.color, lineWidth: 1.5)
            case .hostAway, .hostOffline:
                Circle().strokeBorder(
                    design.inkFaint.color, style: StrokeStyle(lineWidth: 1.5, dash: [2, 2.2]))
            case .exited, .unsupported:
                Circle().fill(design.hairline.color)
            }
        }
        .frame(width: Self.size, height: Self.size)
        .accessibilityHidden(true)
    }

    private struct Working: View {
        @Environment(\.design) private var design
        let style: WorkingMark
        let still: Bool

        var body: some View {
            TimelineView(.animation(paused: still)) { context in
                let t = still ? 0.25 : context.date.timeIntervalSinceReferenceDate
                switch style {
                case .breathing:
                    let phase = (sin(t * .pi / 1.1) + 1) / 2
                    Circle()
                        .fill(design.ink.color)
                        .scaleEffect(0.55 + 0.45 * phase)
                        .opacity(0.35 + 0.5 * phase)
                case .typing:
                    HStack(spacing: 1.5) {
                        ForEach(0..<3) { i in
                            let phase = (sin((t * 2.4 - Double(i) * 0.7) * .pi) + 1) / 2
                            Circle()
                                .fill(design.ink.color)
                                .frame(width: 2.6, height: 2.6)
                                .opacity(0.3 + 0.7 * phase)
                                .offset(y: -1.5 * phase)
                        }
                    }
                    .frame(width: 14)
                case .orbit:
                    ZStack {
                        Circle().strokeBorder(design.hairline.color, lineWidth: 1.5)
                        Circle()
                            .trim(from: 0, to: 0.22)
                            .stroke(design.ink.color, style: StrokeStyle(lineWidth: 1.5, lineCap: .round))
                            .padding(0.75)
                            .rotationEffect(.radians(t.truncatingRemainder(dividingBy: 1.4) / 1.4 * 2 * .pi))
                    }
                case .filling:
                    let phase = (t.truncatingRemainder(dividingBy: 2.4)) / 2.4
                    let fill = phase < 0.5 ? phase * 2 : 1
                    let start = phase < 0.5 ? 0 : (phase - 0.5) * 2
                    ZStack {
                        Circle().strokeBorder(design.hairline.color, lineWidth: 1.5)
                        Circle()
                            .trim(from: start, to: fill)
                            .stroke(design.ink.color, style: StrokeStyle(lineWidth: 1.5, lineCap: .round))
                            .padding(0.75)
                            .rotationEffect(.degrees(-90))
                    }
                }
            }
        }
    }
}


// MARK: - Callouts

/// Something a list asks of the person, above its rows.
struct Callout<Action: Equatable>: Identifiable, Equatable {
    let id: String
    let glyph: String
    let title: String
    let detail: String?
    /// The verb on its button, when it has one; otherwise the whole callout
    /// is the button.
    let button: String?
    let act: Action
    /// What a driver names its card by.
    var identifier: String?
}

/// The callouts: one takes the full width; more than one narrow so the next
/// one shows at the edge, saying the strip slides.
struct CalloutStrip<Action: Equatable>: View {
    let callouts: [Callout<Action>]
    let act: (Action) -> Void

    var body: some View {
        if callouts.count == 1, let only = callouts.first {
            CalloutCard(callout: only, act: act).padding(.horizontal, Space.margin)
        } else {
            ScrollView(.horizontal) {
                HStack(spacing: Space.s) {
                    ForEach(callouts) { callout in
                        CalloutCard(callout: callout, act: act)
                            .containerRelativeFrame(.horizontal) { width, _ in
                                (width - Space.margin * 2) * 0.86
                            }
                    }
                }
                .scrollTargetLayout()
            }
            .contentMargins(.horizontal, Space.margin, for: .scrollContent)
            .scrollTargetBehavior(.viewAligned)
            .scrollIndicators(.hidden)
        }
    }
}

/// A callout as a card: a quiet surface with its mark, its words and its
/// button.
private struct CalloutCard<Action: Equatable>: View {
    @Environment(\.design) private var design
    let callout: Callout<Action>
    let act: (Action) -> Void

    var body: some View {
        Button { act(callout.act) } label: {
            HStack(spacing: Space.m) {
                Image(systemName: callout.glyph)
                    .font(.system(size: 18, weight: .medium))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 24)
                VStack(alignment: .leading, spacing: Space.xxs) {
                    Text(callout.title)
                        .designFont(.bodyEmphasis, design)
                        .foregroundStyle(design.ink.color)
                        .lineLimit(1)
                    if let detail = callout.detail {
                        Text(detail)
                            .designFont(.caption, design)
                            .foregroundStyle(design.inkMuted.color)
                            .lineLimit(1)
                    }
                }
                Spacer(minLength: Space.s)
                if let button = callout.button {
                    Text(button)
                        .designFont(.caption, design)
                        .fontWeight(.semibold)
                        .foregroundStyle(design.onAccent.color)
                        .padding(.horizontal, Space.m)
                        .frame(height: 30)
                        .background(Capsule().fill(design.accent.color))
                } else {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(design.inkFaint.color)
                }
            }
            .padding(.horizontal, Space.m)
            .frame(minHeight: 60)
            .background(
                RoundedRectangle(cornerRadius: design.metrics.controlRadius)
                    .fill(design.sunken.color))
            .contentShape(RoundedRectangle(cornerRadius: design.metrics.controlRadius))
        }
        .buttonStyle(.amuxControl)
        .identified(callout.identifier ?? "home.callout.\(callout.id)", label: callout.title)
    }
}
