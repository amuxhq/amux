import AmuxValues
import AmuxWords
import SwiftUI

/// The threads of work the profile reaches, in the runtime's order, with
/// the settled ones behind one row at the foot.
///
/// A row is a family's head: the agent the person started. The agents it
/// started, and the subagents Claude or Codex run for it, are not listed:
/// the head stands for them, saying when one of them needs the person, and
/// they are reached from its chat.
struct FleetSidebar: View {
    let content: MainContent
    @Binding var place: Place
    @Binding var search: String
    let actions: MainActions

    @State private var renaming: FleetRow?
    @State private var newName = ""
    @State private var deleting: FleetRow?
    @FocusState private var listFocused: Bool
    @FocusState private var searching: Bool
    /// The bar shows the search field in place of its title.
    @State private var searchShown = false

    private var fleet: FleetContent { content.fleet }

    var body: some View {
        VStack(spacing: 0) {
            bar
            if !place.settled, content.toFinish > 0 {
                FinishSettingUp(count: content.toFinish, actions: actions)
            }
            list
            if !place.settled, let settled = fleet.settled {
                VStack(spacing: 0) {
                    Rectangle().fill(Palette.line).frame(height: 1)
                    FootRow(title: "Settled", count: Int(settled.families), symbol: "chevron-right") {
                        place.settled = true
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                }
            }
        }
        .background {
            // ⌘F reaches the search from anywhere in the window.
            Button("Search") { showSearch() }
                .keyboardShortcut("f")
                .opacity(0)
                .frame(width: 0, height: 0)
                .accessibilityHidden(true)
        }
        .alert("Rename agent", isPresented: isRenaming, presenting: renaming) { row in
            TextField("Name", text: $newName)
            Button("Rename") { actions.act(.rename(newName), row.card.agent) }
            Button("Cancel", role: .cancel) {}
        }
        .sheet(isPresented: isDeleting) {
            if let row = deleting {
                DeleteSheet(card: row.card, ask: actions.deleteWarning) {
                    deleting = nil
                } confirm: { act in
                    deleting = nil
                    actions.act(act, row.card.agent)
                }
            }
        }
    }

    /// The heads, in order. The runtime lists a family's members only
    /// under a head unfolded for them, which this sidebar never asks for.
    private var rows: [FleetRow] {
        if place.settled { return (fleet.settled?.rows ?? []).filter { $0.depth == 0 } }
        return heads.filter { !hidden($0) }
    }

    private var heads: [FleetRow] { fleet.live.flatMap(\.rows).filter { $0.depth == 0 } }

    /// The heads at work, which are not news until they stop: left out of
    /// the list unless the person shows them, with the toggle in the bar.
    /// A pinned head stays where the person put it, a family with a member
    /// waiting on the person stays in view, and a search shows everything
    /// it finds.
    private var working: [FleetRow] { heads.filter(atWork) }

    private func hidden(_ row: FleetRow) -> Bool { !place.working && atWork(row) }

    private func atWork(_ row: FleetRow) -> Bool {
        guard search.isEmpty, row.card.filing != .pinned, row.card.familyAttention != .needsYou else { return false }
        switch RowState(row: row) {
        case .working, .starting: return true
        default: return false
        }
    }

    /// The window bar over the list: what it lists, and search; the search
    /// field in its place while it is used; or the way back from the
    /// settled agents.
    @ViewBuilder private var bar: some View {
        if place.settled {
            WindowBar(leading: 6, rule: false) {
                Button {
                    place.settled = false
                } label: {
                    HStack(spacing: 6) {
                        Icon("chevron-left", size: 14).foregroundStyle(Palette.textSecondary)
                        Text("Settled").font(.ui(.prose, weight: .medium)).foregroundStyle(Palette.textSecondary)
                    }
                    .padding(.horizontal, 6)
                    .frame(height: 26)
                    .contentShape(Rectangle())
                }
                .buttonStyle(HoverButtonStyle())
                .keyboardShortcut(.escape, modifiers: [])
                Spacer()
            }
        } else if searchShown || !search.isEmpty {
            WindowBar(leading: 8, rule: false) {
                SearchField(text: $search, focused: $searching)
                    .onChange(of: search) { _, words in actions.search(words) }
                    .onChange(of: searching) { _, focused in
                        if !focused, search.isEmpty { searchShown = false }
                    }
                    .onExitCommand {
                        search = ""
                        searchShown = false
                    }
            }
        } else {
            WindowBar(leading: 8, rule: false) {
                Button(action: showSearch) {
                    HStack(spacing: 8) {
                        Icon("search", size: 16).foregroundStyle(Palette.textSecondary)
                        Text("Search").font(.ui(.prose, weight: .medium)).foregroundStyle(Palette.textSecondary)
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 6)
                    .frame(height: 28)
                    .contentShape(Rectangle())
                }
                .buttonStyle(HoverButtonStyle())
                .help("Search (⌘F)")
                if !working.isEmpty {
                    WorkingToggle(
                        count: working.count, shown: place.working,
                        holdsOpen: !place.working && working.contains { $0.card.agent == selected }
                    ) {
                        withAnimation(.snappy(duration: 0.2)) { place.working.toggle() }
                    }
                }
                BarButton(symbol: "square-pen", help: "New agent (⌘N)") { actions.newAgent() }
            }
        }
    }

    private func showSearch() {
        place.settled = false
        searchShown = true
        Task { @MainActor in searching = true }
    }

    @ViewBuilder private var list: some View {
        if rows.isEmpty {
            empty.frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ScrollViewReader { reader in
                ScrollView {
                    // One flat list, so a row keeps its identity when it
                    // moves between sections and redraws in its new place.
                    LazyVStack(alignment: .leading, spacing: 2) {
                        ForEach(entries) { entry in
                            switch entry {
                            case .title(_, let text): SectionTitle(text: text, first: entry.id == entries.first?.id)
                            case .row(let row): item(row)
                            }
                        }
                    }
                    .padding(.horizontal, 8)
                    .padding(.top, 4)
                    .padding(.bottom, 8)
                }
                .scrollIndicators(.automatic)
                .focusable()
                .focusEffectDisabled()
                .focused($listFocused)
                .onKeyPress(.upArrow) { step(-1, reader) }
                .onKeyPress(.downArrow) { step(1, reader) }
            }
        }
    }

    /// What the list draws: the section headings and their rows, in order.
    private enum Entry: Identifiable {
        case title(SectionKind, String)
        case row(FleetRow)

        enum ID: Hashable {
            case title(SectionKind)
            case row(AgentKey)
        }

        var id: ID {
            switch self {
            case .title(let kind, _): .title(kind)
            case .row(let row): .row(row.card.agent)
            }
        }
    }

    private var entries: [Entry] {
        if place.settled { return rows.map(Entry.row) }
        return fleet.live.flatMap { section in
            (title(section).map { [Entry.title(section.kind, $0)] } ?? [])
                + section.rows.filter { $0.depth == 0 && !hidden($0) }.map(Entry.row)
        }
    }

    private func item(_ row: FleetRow) -> some View {
        FleetRowView(
            row: row, now: fleet.now, selected: selected == row.card.agent,
            hostShown: row.card.agent.host == content.thisMac?.hostId, quick: quick(row), slim: place.settled
        )
        .onTapGesture {
            place.agent = row.card.agent
            listFocused = true
        }
        .contextMenu { menu(row) }
    }

    /// The row lit for the open agent: its own, or its family head's.
    private var selected: AgentKey? { place.agent.flatMap(content.listed) }

    /// A heading over a section: none. The runtime's order says it, the
    /// pinned rows carry a pin and the ones that need the person say so in
    /// their status, so the list reads as one inbox.
    private func title(_ section: FleetSection) -> String? { nil }

    /// Moves the selection up or down the list, as arrow keys do in a
    /// sidebar.
    private func step(_ by: Int, _ reader: ScrollViewProxy) -> KeyPress.Result {
        let keys = rows.map(\.card.agent)
        guard !keys.isEmpty else { return .ignored }
        let at = selected.flatMap { keys.firstIndex(of: $0) }
        let next = at.map { min(max($0 + by, 0), keys.count - 1) } ?? (by > 0 ? 0 : keys.count - 1)
        place.agent = keys[next]
        reader.scrollTo(Entry.ID.row(keys[next]))
        return .handled
    }

    /// What a row offers under the pointer: pinning and settling, the two
    /// things done to many rows in a row.
    private func quick(_ row: FleetRow) -> [QuickAction] {
        let agent = row.card.agent
        var quick: [QuickAction] = []
        if row.pinned {
            quick.append(QuickAction(symbol: "pin-off", help: "Unpin") { actions.act(.unfile, agent) })
        } else if row.pinnable {
            quick.append(QuickAction(symbol: "pin", help: "Pin") { actions.act(.pin, agent) })
        }
        if row.closed {
            quick.append(QuickAction(symbol: "archive-restore", help: "Unsettle") { actions.act(.unfile, agent) })
        } else if !row.card.closeHeld {
            quick.append(QuickAction(symbol: "circle-check", help: "Settle") { actions.act(.close, agent) })
        }
        return quick
    }

    @ViewBuilder private func menu(_ row: FleetRow) -> some View {
        let agent = row.card.agent
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
        Button("Rename…") {
            newName = row.card.name
            renaming = row
        }
        // A provider subagent goes with its parent, which deletes it.
        if !row.native {
            Divider()
            Button("Delete…", role: .destructive) { deleting = row }
        }
    }

    private var isRenaming: Binding<Bool> {
        Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })
    }

    private var isDeleting: Binding<Bool> {
        Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } })
    }

    @ViewBuilder private var empty: some View {
        if !fleet.loaded {
            ProgressView().controlSize(.small).frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            VStack(alignment: .leading) {
                Text(emptyWords)
                    .font(.ui(.meta))
                    .foregroundStyle(Palette.textTertiary)
                    .padding(.leading, 18)
                    .padding(.trailing, 16)
                    .padding(.top, 10)
                Spacer()
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var emptyWords: String {
        if place.settled { return "Nothing settled yet." }
        if !search.isEmpty { return "No agents match “\(search)”." }
        return "Your agents will show up here."
    }
}

/// Something a row offers under the pointer.
struct QuickAction {
    let symbol: String
    let help: String
    let action: () -> Void
}

/// A thread of work as a card of three lines: where it works (its
/// repository's monogram and name, a pin when pinned) with its status, or
/// else its age, at the right; its name; then what it is doing or asking
/// while it is live, else its branch and host, with the provider's glyph at
/// the right. Every row is the same height, so the list reads evenly.
struct FleetRowView: View {
    let row: FleetRow
    let now: Date
    let selected: Bool
    /// The host goes without saying: it is this Mac, or the page the row
    /// sits on is the host's own, so its place leaves the host out.
    var hostShown = false
    /// The surface the row sits on. Kept for callers; the row draws on
    /// whatever is under it.
    var ground: Color = Palette.content
    var quick: [QuickAction] = []
    /// One line: a settled agent, out of the way.
    var slim = false

    @State private var hovering = false

    var body: some View {
        if slim { slimBody } else { fullBody }
    }

    private var slimBody: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            RepositoryTile(repository: row.card.repository, size: 14, native: row.native, quiet: true)
            Text(row.name)
                .font(.ui(.prose))
                .foregroundStyle(Palette.text.opacity(0.75))
                .lineLimit(1)
            Spacer(minLength: 4)
            trailing
        }
        .padding(.horizontal, 10)
        .frame(height: 36)
        .background(ground(selected: selected))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .help(described)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(selected ? [.isSelected, .isButton] : .isButton)
    }

    private func ground(selected: Bool) -> some View {
        let shape = RoundedRectangle(cornerRadius: Radius.row, style: .continuous)
        return shape.fill(selected ? Palette.selection : (hovering ? Palette.hover : .clear))
            .overlay(shape.strokeBorder(selected ? Palette.selectionRing : .clear))
    }

    private var fullBody: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                RepositoryTile(repository: row.card.repository, size: 14, native: row.native, quiet: dimmed)
                Text(repositoryName)
                    .font(.ui(.meta))
                    .foregroundStyle(Palette.textSecondary)
                    .lineLimit(1)
                if row.pinned {
                    Icon("pin", size: 11).foregroundStyle(Palette.textTertiary)
                }
                Spacer(minLength: 4)
                trailing
            }
            .frame(height: 16)
            Text(row.name)
                .font(.ui(.prose, weight: dimmed ? .regular : .medium))
                .foregroundStyle(dimmed ? Palette.text.opacity(0.75) : Palette.text)
                .lineLimit(1)
                .frame(height: 20)
            HStack(spacing: 6) {
                thirdLine
                Spacer(minLength: 4)
                if !row.native {
                    if let glyph = ProviderMark.glyph(row.card.provider) {
                        Icon(glyph, size: 12, brand: true)
                            .foregroundStyle(Palette.textTertiary)
                            .help(row.card.providerName)
                    } else {
                        Text(row.card.providerName)
                            .font(.ui(.meta))
                            .foregroundStyle(Palette.textTertiary)
                            .lineLimit(1)
                            .layoutPriority(-1)
                    }
                }
            }
            .frame(height: 16)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(ground(selected: selected))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .help(described)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(selected ? [.isSelected, .isButton] : .isButton)
    }

    private var repositoryName: String {
        if let name = row.card.repository?.name, !name.isEmpty { return name }
        return PlacesWords.place(row)
    }

    /// What the agent is doing or asking; at rest, where it works.
    @ViewBuilder private var thirdLine: some View {
        if let words {
            Text(words.text)
                .font(.ui(.meta))
                .foregroundStyle(words.color)
                .lineLimit(1)
                .truncationMode(.tail)
        } else {
            Text(place)
                .font(.ui(.meta))
                .foregroundStyle(Palette.textSecondary)
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }

    /// The words the third line takes in place of the place, if any: an
    /// ask (its own or a member's), why it stopped, the step at work, or
    /// that its host cannot be reached.
    private var words: (text: String, color: Color)? {
        if let loud = row.loud, let line = FleetWords.secondLine(loud.secondLine, provider: row.card.providerName, now: now),
            line.ink == .ask
        {
            return ("\(loud.name): \(line.text)", Palette.you)
        }
        switch state {
        case .needsYou:
            return (said.text, said.color == Palette.you ? Palette.you : Palette.textSecondary)
        case .exited(.failed), .working, .starting, .hostOffline, .hostAway, .unsupported:
            return (said.text, Palette.textSecondary)
        default:
            return nil
        }
    }

    private var state: RowState { RowState(row: row) }

    /// Agents at rest read quieter than the ones at work or waiting, so
    /// the eye goes to what is happening.
    private var dimmed: Bool {
        if row.card.familyAttention == .needsYou { return false }
        switch state {
        case .settled, .idle, .exited(.ended), .exited(.finished): return true
        default: return false
        }
    }

    /// Under the pointer, the row's own actions; else its status where it
    /// has one worth saying, else its age.
    @ViewBuilder private var trailing: some View {
        if hovering, !quick.isEmpty {
            HStack(spacing: 0) {
                ForEach(Array(quick.enumerated()), id: \.offset) { _, action in
                    Button(action: action.action) {
                        Icon(action.symbol, size: 13)
                            .foregroundStyle(Palette.textSecondary)
                            .frame(width: 20, height: 18)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(HoverButtonStyle())
                    .help(action.help)
                }
            }
            // Buttons with no words: centred on the row's words, not hung
            // from their baseline.
            .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 4.5 }
        } else if let status {
            HStack(spacing: 4) {
                if let symbol = status.symbol { Icon(symbol, size: 12) }
                if status.ticks {
                    TimelineView(.periodic(from: .now, by: 1)) { context in
                        Text("\(status.word) \(Self.since(row.card.phaseSinceMs, at: context.date))")
                            .monospacedDigit()
                    }
                } else {
                    Text(status.word)
                }
            }
            // The states that need something carry a glyph and weight;
            // work going on is the normal state and reads as plain words.
            .font(.ui(.meta, weight: status.symbol == nil ? .regular : .medium))
            .foregroundStyle(status.color)
            .lineLimit(1)
        } else {
            Text(row.age(at: now))
                .font(.ui(.meta))
                .foregroundStyle(Palette.textTertiary)
                .monospacedDigit()
        }
    }

    /// The status the age slot gives way to.
    private var status: (symbol: String?, word: String, color: Color, ticks: Bool)? {
        if row.card.familyAttention == .needsYou || state == .needsYou {
            return ("shield", "Needs you", Palette.you, false)
        }
        switch state {
        case .working: return (nil, "Working", Palette.working, row.card.phaseSinceMs > 0)
        case .starting: return (nil, "Starting", Palette.working, false)
        case .exited(.failed): return ("circle-alert", "Failed", Palette.failure, false)
        case .hostOffline, .hostAway: return ("cloud-off", "Offline", Palette.textTertiary, false)
        default: return nil
        }
    }

    /// How long since `ms`, in the sidebar's short form: "21s", "4m", "1h".
    static func since(_ ms: Int64, at date: Date) -> String {
        let seconds = max(0, Int(date.timeIntervalSince1970) - Int(ms / 1000))
        if seconds < 60 { return "\(seconds)s" }
        if seconds < 3600 { return "\(seconds / 60)m" }
        return "\(seconds / 3600)h"
    }

    /// "fix-scroll · build-box": the branch, and the host unless it goes
    /// without saying.
    private var place: String {
        var parts: [String] = []
        if let branch = row.card.branch, !branch.isEmpty { parts.append(branch) }
        if !hostShown { parts.append(PlaceNames.host(row.hostName)) }
        if parts.isEmpty { parts.append(PlaceNames.path(row.workingDirectory)) }
        return parts.joined(separator: " · ")
    }

    /// What the row would say on its third line, as every client words it.
    private var said: (text: String, color: Color) {
        if let loud = row.loud,
            let line = FleetWords.secondLine(loud.secondLine, provider: row.card.providerName, now: now)
        {
            return ("\(loud.name): \(line.text)", color(line.ink))
        }
        if let line = FleetWords.secondLine(
            row.secondLine, provider: row.card.providerName, now: now, backgroundJobs: row.card.backgroundJobs)
        {
            return (line.text, color(line.ink))
        }
        let word = state.word ?? ""
        let host = PlaceNames.host(row.hostName)
        return (state.namesTheHost ? word : "\(word) · \(host)", Palette.textSecondary)
    }

    /// The row in full, for the tooltip: its state, host and folder.
    private var described: String {
        [said.text, "\(PlaceNames.host(row.hostName)) · \(PlaceNames.path(row.workingDirectory))"]
            .filter { !$0.isEmpty }.joined(separator: "\n")
    }

    private func color(_ ink: FleetWords.Ink) -> Color {
        switch ink {
        case .ask: Palette.you
        case .quiet: Palette.textSecondary
        case .warning: Palette.caution
        case .error: Palette.failure
        }
    }
}

/// "Settled 4 ›" at the sidebar's foot, under a hairline.
/// The row at the sidebar's foot that shows the settled agents: its name,
/// how many, and the way to them.
private struct FootRow: View {
    let title: String
    let count: Int
    let symbol: String
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 6) {
                Text(title)
                Spacer()
                Text("\(count)").foregroundStyle(Palette.textTertiary).monospacedDigit()
                Icon(symbol, size: 12)
                    .foregroundStyle(Palette.textTertiary)
            }
            .font(.ui(.meta, weight: .medium))
            .foregroundStyle(Palette.textSecondary)
            .padding(.horizontal, 10)
            .frame(height: 30)
            .contentShape(Rectangle())
        }
        .buttonStyle(HoverButtonStyle())
    }
}

/// Shows or hides the agents at work, which are not news until they stop:
/// "2 working", in the rows' own word rather than a glyph to learn, lit
/// while they are shown and faintly while it hides the agent open in the
/// centre.
private struct WorkingToggle: View {
    let count: Int
    let shown: Bool
    let holdsOpen: Bool
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            Text("\(count) working")
                .font(.ui(.meta, weight: .medium))
                .monospacedDigit()
                // Smaller than "Search" beside it, so centred it rides a
                // point above that word's baseline; this puts them on one.
                .offset(y: 1)
            .foregroundStyle(shown ? Palette.text : Palette.textSecondary)
            .padding(.horizontal, 7)
            .frame(height: 28)
            .background(
                RoundedRectangle(cornerRadius: Radius.control, style: .continuous)
                    .fill(shown ? Palette.selection : (holdsOpen ? Palette.hover : .clear)))
            .contentShape(Rectangle())
        }
        .buttonStyle(HoverButtonStyle())
        .help(shown ? "Hide the agents at work" : (count == 1 ? "Show the agent at work" : "Show the \(count) agents at work"))
        .accessibilityLabel(shown ? "Hide agents at work" : "Show \(count) agents at work")
    }
}

/// Search as a plain row, the field showing only once it is used, so an
/// idle sidebar holds rows, not an empty box.
struct SearchField: View {
    @Binding var text: String
    var prompt = "Search"
    var focused: FocusState<Bool>.Binding?
    @FocusState private var own: Bool
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 8) {
            Icon("search", size: 15)
                .foregroundStyle(Palette.textSecondary)
                .frame(width: 18)
            field
                .textFieldStyle(.plain)
                .font(.ui(.body))
                .onExitCommand { text = "" }
            if !text.isEmpty {
                Button {
                    text = ""
                } label: {
                    Icon("x", size: 12).foregroundStyle(Palette.textTertiary)
                }
                .buttonStyle(.plain)
                .help("Clear search")
            } else if !isFocused {
                Text("⌘F").font(.ui(.caption)).foregroundStyle(Palette.textTertiary)
            }
        }
        .padding(.horizontal, 8)
        .frame(height: 28)
        .background(
            RoundedRectangle(cornerRadius: Radius.row, style: .continuous)
                .fill(isFocused || !text.isEmpty ? Palette.well : (hovering ? Palette.hover : .clear)))
        .onHover { hovering = $0 }
    }

    @ViewBuilder private var field: some View {
        if let focused {
            TextField(prompt, text: $text).focused(focused)
        } else {
            TextField(prompt, text: $text).focused($own)
        }
    }

    private var isFocused: Bool { focused?.wrappedValue ?? own }
}

/// "Finish setting up", with the steps left, until they are done or it is
/// dismissed: an ordinary sidebar row, its dismiss shown under the pointer.
private struct FinishSettingUp: View {
    let count: UInt32
    let actions: MainActions
    @State private var hovering = false

    var body: some View {
        SidebarAction(symbol: "list-checks", title: "Finish setting up", count: hovering ? nil : Int(count)) {
            actions.finishSettingUp()
        }
        .overlay(alignment: .trailing) {
            if hovering {
                BarButton(symbol: "x", help: "Don’t show again", size: 12) { actions.dismissFinishing() }
                    .padding(.trailing, 2)
            }
        }
        .onHover { hovering = $0 }
        .padding(.horizontal, 8)
    }
}
